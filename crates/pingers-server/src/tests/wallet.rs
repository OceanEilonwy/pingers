// SPDX-License-Identifier: AGPL-3.0-only
//! A device's credentials against the server under test, fetched the way a device fetches
//! them: register an account, authenticate, read the issuer key, issue a Privacy Pass batch.

use std::sync::{Mutex, PoisonError};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pingers_anon::{PrivacyPassToken, TokenCount, begin_token_issuance};
use pingers_crypto::{AccountSigningKey, sign_auth_challenge, sign_registration};
use pingers_ohttp::{BinaryRequest, Header};
use pingers_proto::{
    AuthRequest, AuthResponse, PrivacyPassIssueRequest, PrivacyPassIssueResponse, PrivacyPassKeys,
    RegisterRequest, RegisterResponse, WireDecode as _, WireEncode as _,
};
use pingers_time::Clock as _;
use zeroize::Zeroizing;

use super::{LOOPBACK, Running, TestError, connect, read_all, rng, send, server_id};

/// A direct request's status and body.
pub(super) struct Answer {
    pub(super) status: StatusCode,
    pub(super) body: Vec<u8>,
}

/// Sends a direct request (`route` is `"<METHOD> <path>"`) with optional `authorization`.
pub(super) async fn direct(
    server: &Running,
    route: &str,
    authorization: Option<&str>,
    body: Vec<u8>,
) -> Result<Answer, TestError> {
    let (method, path) = route
        .split_once(' ')
        .ok_or("route needs a method and a path")?;
    let mut client = connect(server.address, LOOPBACK).await?;
    let mut request = Request::builder()
        .method(Method::from_bytes(method.as_bytes())?)
        .uri(format!("http://server{path}"));
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    let response = send(&mut client, request.body(Body::from(body))?).await?;
    let status = response.status();
    Ok(Answer {
        status,
        body: read_all(response).await?,
    })
}

/// An account and its unspent tokens.
pub(super) struct Wallet {
    pub(super) bearer: String,
    tokens: Mutex<Vec<PrivacyPassToken>>,
}

impl Wallet {
    /// Registers the account of `key` and fetches `count` tokens.
    pub(super) async fn fetch(server: &Running, key: u8, count: u16) -> Result<Self, TestError> {
        let signing = AccountSigningKey::from_bytes(Zeroizing::new([key; 32]));
        let registration = RegisterRequest {
            signature_key: *signing.public_key().as_bytes(),
            proof: *sign_registration(&signing, &server_id()?)?.as_bytes(),
            registration_code: None,
        };
        let registered = direct(server, "POST /v1/accounts", None, registration.encode()?).await?;
        assert_eq!(registered.status, StatusCode::OK);
        let account_id = RegisterResponse::decode(&registered.body)?.account_id;
        let auth = AuthRequest {
            account_id,
            nonce_signature: *sign_auth_challenge(
                &signing,
                &server_id()?,
                server.clock.now().bucket(),
            )?
            .as_bytes(),
        };
        let authed = direct(server, "POST /v1/auth", None, auth.encode()?).await?;
        assert_eq!(authed.status, StatusCode::OK);
        let bearer = format!(
            "Bearer {}",
            AuthResponse::decode(&authed.body)?.bearer.expose()
        );
        let tokens = if count == 0 {
            Vec::new()
        } else {
            Self::issue(server, &bearer, count).await?
        };
        Ok(Self {
            bearer,
            tokens: Mutex::new(tokens),
        })
    }

    /// Issues a batch of `count` tokens for `bearer`.
    pub(super) async fn issue(
        server: &Running,
        bearer: &str,
        count: u16,
    ) -> Result<Vec<PrivacyPassToken>, TestError> {
        let keys = direct(server, "GET /v1/pp/keys", None, Vec::new()).await?;
        let public_key = PrivacyPassKeys::decode(&keys.body)?
            .keys
            .last()
            .ok_or("the issuer has no key")?
            .public_key
            .clone();
        let pending = begin_token_issuance(&public_key, TokenCount::new(count)?, &mut rng(5))?;
        let issued = direct(
            server,
            "POST /v1/pp/issue",
            Some(bearer),
            PrivacyPassIssueRequest::new(pending.request().to_vec())?.encode()?,
        )
        .await?;
        assert_eq!(issued.status, StatusCode::OK);
        let response = PrivacyPassIssueResponse::decode(&issued.body)?;
        Ok(pending.finish(response.token_response())?)
    }

    /// The `authorization` header spending the next token.
    pub(super) fn spend(&self) -> Result<Header, TestError> {
        let token = self
            .tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
            .ok_or("the wallet is empty")?;
        Ok(Header {
            name: "authorization".to_owned(),
            value: token.authorization().as_bytes().to_vec(),
        })
    }

    /// `request` with the next token added.
    pub(super) fn pay(&self, mut request: BinaryRequest) -> Result<BinaryRequest, TestError> {
        request.headers.push(self.spend()?);
        Ok(request)
    }
}
