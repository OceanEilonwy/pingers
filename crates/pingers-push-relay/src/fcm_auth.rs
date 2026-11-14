// SPDX-License-Identifier: AGPL-3.0-only
//! FCM HTTP v1 authorisation: an OAuth 2 access token for the relay's Google service account,
//! obtained with a JWT bearer assertion (RFC 7523) signed RS256, cached, and refreshed
//! [`REFRESH_MARGIN`] before it expires.
//!
//! The signing is pingers-crypto's (`rs256` feature, style guide §8): this module builds the
//! header and claims and hands them to [`sign_compact_jws_rs256`]. The private key is moved out
//! of the parsed file into a [`Zeroizing`] buffer and parsed once into pingers-crypto's
//! [`Rs256Key`], which AWS-LC zeroizes when it is freed. Nothing here implements `Debug` over the
//! key, and no error carries any part of the file.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use pingers_crypto::{Rs256Key, sign_compact_jws_rs256};
use pingers_time::{Clock, UnixSeconds};
use reqwest::Url;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use zeroize::Zeroizing;

/// The one OAuth scope the relay asks for.
pub const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

/// How long an assertion is valid: Google accepts at most one hour.
pub const ASSERTION_LIFETIME: Duration = Duration::from_hours(1);

/// How long before an access token expires the relay fetches a new one.
pub const REFRESH_MARGIN: Duration = Duration::from_mins(5);

const JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// What is wrong with the service-account file. Never carries any part of it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServiceAccountError {
    /// Not JSON, or a required field (`client_email`, `private_key`, `token_uri`) is missing
    /// or not a string. The parser's message is not kept: it can quote values.
    #[error("service account file: syntax or schema error at line {line}")]
    Syntax {
        /// One-based line of the error.
        line: usize,
    },
    /// `type` is present and is not `service_account`.
    #[error("service account file: type is not \"service_account\"")]
    NotServiceAccount,
    /// `client_email` is empty.
    #[error("service account file: client_email is empty")]
    NoClientEmail,
    /// `token_uri` is not an `https://` URL (`http://` is allowed to loopback addresses only,
    /// for tests).
    #[error("service account file: token_uri is not an https URL")]
    BadTokenUri,
    /// `private_key` is not a PEM PKCS #8 RSA key of 2048 to 8192 bits.
    #[error("service account file: private_key is not a PEM PKCS #8 RSA key")]
    BadPrivateKey,
}

/// Why no access token could be obtained.
#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenError {
    /// The assertion could not be built or signed.
    #[error("cannot sign the token request")]
    Sign,
    /// The token endpoint could not be reached, or its answer could not be read.
    #[error("cannot reach the token endpoint")]
    Unreachable,
    /// The token endpoint refused the assertion.
    #[error("the token endpoint answered HTTP {status}")]
    Refused {
        /// The HTTP status.
        status: u16,
    },
    /// The answer was not a bearer token with a positive lifetime.
    #[error("the token endpoint's answer is not a usable access token")]
    BadResponse,
}

/// A Google service account, from the JSON key file the Firebase console downloads.
pub struct ServiceAccount {
    client_email: String,
    /// The `aud` claim: `token_uri` exactly as written in the file.
    audience: String,
    token_uri: Url,
    key_id: Option<String>,
    key: Rs256Key,
}

impl fmt::Debug for ServiceAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceAccount")
            .field("client_email", &self.client_email)
            .field("token_uri", &self.audience)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct FileServiceAccount {
    #[serde(rename = "type")]
    kind: Option<String>,
    client_email: String,
    private_key: String,
    private_key_id: Option<String>,
    token_uri: String,
}

impl ServiceAccount {
    /// Parses a service-account key file's text. Only `client_email`, `private_key`,
    /// `token_uri` and, if present, `private_key_id` and `type` are read.
    ///
    /// # Errors
    ///
    /// [`ServiceAccountError`], which names the field at fault and nothing of its value.
    pub fn from_json(text: &str) -> Result<Self, ServiceAccountError> {
        let file: FileServiceAccount = serde_json::from_str(text)
            .map_err(|error| ServiceAccountError::Syntax { line: error.line() })?;
        let FileServiceAccount {
            kind,
            client_email,
            private_key,
            private_key_id,
            token_uri,
        } = file;
        // Moved, not copied: the buffer is wiped when this goes out of scope.
        let private_key = Zeroizing::new(private_key);
        if kind.is_some_and(|kind| kind != "service_account") {
            return Err(ServiceAccountError::NotServiceAccount);
        }
        if client_email.is_empty() {
            return Err(ServiceAccountError::NoClientEmail);
        }
        let endpoint = token_endpoint(&token_uri)?;
        let key = Rs256Key::from_pkcs8_pem(&private_key)
            .map_err(|_| ServiceAccountError::BadPrivateKey)?;
        Ok(Self {
            client_email,
            audience: token_uri,
            token_uri: endpoint,
            key_id: private_key_id.filter(|id| !id.is_empty()),
            key,
        })
    }

    /// The signed JWT bearer assertion for `now`: RS256, `iss` the client email, `scope`
    /// [`FCM_SCOPE`], `aud` the token URI, valid for [`ASSERTION_LIFETIME`].
    ///
    /// # Errors
    ///
    /// [`TokenError::Sign`] if signing fails.
    pub fn assertion(&self, now: UnixSeconds) -> Result<Zeroizing<String>, TokenError> {
        let issued_at = now.get();
        let expires_at = issued_at.saturating_add(ASSERTION_LIFETIME.as_secs());
        let header = match &self.key_id {
            Some(key_id) => serde_json::json!({"alg": "RS256", "typ": "JWT", "kid": key_id}),
            None => serde_json::json!({"alg": "RS256", "typ": "JWT"}),
        };
        let claims = serde_json::json!({
            "iss": self.client_email,
            "scope": FCM_SCOPE,
            "aud": self.audience,
            "iat": issued_at,
            "exp": expires_at,
        });
        let header = serde_json::to_vec(&header).map_err(|_| TokenError::Sign)?;
        let claims = serde_json::to_vec(&claims).map_err(|_| TokenError::Sign)?;
        sign_compact_jws_rs256(&self.key, &header, &claims).map_err(|_| TokenError::Sign)
    }
}

fn token_endpoint(text: &str) -> Result<Url, ServiceAccountError> {
    let url = Url::parse(text).map_err(|_| ServiceAccountError::BadTokenUri)?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => IpAddr::V4(address).is_loopback(),
        Some(url::Host::Ipv6(address)) => IpAddr::V6(address).is_loopback(),
        Some(url::Host::Domain(_)) | None => false,
    };
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        _ => Err(ServiceAccountError::BadTokenUri),
    }
}

/// A minted access token, as the `Authorization` header value. No `Debug` or `Clone`.
struct CachedToken {
    authorization: Zeroizing<String>,
    refresh_at: UnixSeconds,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    token_type: Option<String>,
}

/// Access tokens for one service account: minted on first use, reused until
/// [`REFRESH_MARGIN`] before they expire, minted by one caller at a time so concurrent pushes
/// cause one refresh, not one each.
pub struct FcmAuth {
    account: ServiceAccount,
    clock: Arc<dyn Clock>,
    cache: tokio::sync::Mutex<Option<CachedToken>>,
}

impl fmt::Debug for FcmAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FcmAuth")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl FcmAuth {
    /// Tokens for `account`, with expiry judged by `clock`.
    #[must_use]
    pub fn new(account: ServiceAccount, clock: Arc<dyn Clock>) -> Self {
        Self {
            account,
            clock,
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// The `Authorization` header value (`Bearer <token>`), from the cache or freshly minted
    /// through `client`. The caller bounds the wait with its own timeout.
    ///
    /// Cancellation-safe: dropping the future leaves the cache as it was, and the next caller
    /// mints.
    ///
    /// # Errors
    ///
    /// [`TokenError`] if a token was needed and could not be minted.
    pub async fn authorization(
        &self,
        client: &reqwest::Client,
    ) -> Result<Zeroizing<String>, TokenError> {
        let mut cache = self.cache.lock().await;
        let now = self.clock.now();
        if let Some(cached) = cache.as_ref()
            && now < cached.refresh_at
        {
            return Ok(cached.authorization.clone());
        }
        let minted = self.mint(client, now).await?;
        let authorization = minted.authorization.clone();
        *cache = Some(minted);
        Ok(authorization)
    }

    /// Drops the cached token, so the next push mints a new one: for when FCM answers 401.
    pub async fn forget(&self) {
        *self.cache.lock().await = None;
    }

    async fn mint(
        &self,
        client: &reqwest::Client,
        now: UnixSeconds,
    ) -> Result<CachedToken, TokenError> {
        let assertion = self.account.assertion(now)?;
        let body = Zeroizing::new(
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", JWT_BEARER_GRANT)
                .append_pair("assertion", &assertion)
                .finish(),
        );
        let response = client
            .post(self.account.token_uri.clone())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body.as_bytes().to_vec())
            .send()
            .await
            .map_err(|_| TokenError::Unreachable)?;
        let status = response.status();
        if !status.is_success() {
            return Err(TokenError::Refused {
                status: status.as_u16(),
            });
        }
        let answer = response
            .bytes()
            .await
            .map_err(|_| TokenError::Unreachable)?;
        let answer: TokenResponse =
            serde_json::from_slice(&answer).map_err(|_| TokenError::BadResponse)?;
        let access_token = Zeroizing::new(answer.access_token);
        let bearer = answer
            .token_type
            .is_none_or(|kind| kind.eq_ignore_ascii_case("bearer"));
        let header_safe =
            !access_token.is_empty() && access_token.bytes().all(|byte| byte.is_ascii_graphic());
        if !bearer || !header_safe || answer.expires_in == 0 {
            return Err(TokenError::BadResponse);
        }
        let reuse_for = answer.expires_in.saturating_sub(REFRESH_MARGIN.as_secs());
        Ok(CachedToken {
            authorization: Zeroizing::new(format!("Bearer {}", access_token.as_str())),
            refresh_at: UnixSeconds::new(now.get().saturating_add(reuse_for)),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
