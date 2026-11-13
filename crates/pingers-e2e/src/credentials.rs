// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy Pass tokens for the devices' anonymous requests (design B.4.8): the server refuses
//! board syncs, mailbox registrations and posts, and group requests without an Anon
//! credential. The pool registers an account over the identified API, fetches a batch, and
//! hands out one token per credentialed request. Tokens are unlinkable, so sharing one pool
//! between the simulated devices changes nothing the server can see.

use std::net::SocketAddr;

use pingers_anon::{PrivacyPassToken, TokenCount, begin_token_issuance};
use pingers_crypto::{AccountSigningKey, sign_auth_challenge, sign_registration};
use pingers_ohttp::{BinaryRequest, Header};
use pingers_proto::{
    AuthRequest, AuthResponse, PrivacyPassIssueRequest, PrivacyPassIssueResponse, PrivacyPassKeys,
    RegisterRequest, RegisterResponse, ServerId, WireDecode as _, WireEncode as _,
};
use pingers_testkit::seeded_rng;
use pingers_time::{Clock as _, ManualClock};
use rand_chacha::ChaCha20Rng;
use tokio::sync::Mutex;

use crate::E2eError;
use crate::stack::{Http2Request, http2};

/// Tokens fetched per batch (one fresh account each, well within the daily allowance).
const BATCH: u16 = 32;

/// Whether `request` needs an Anon credential (board syncs, mailbox registrations and posts).
fn needs_credential(request: &BinaryRequest) -> bool {
    let path = request.path.split('?').next().unwrap_or_default();
    request.method == "POST"
        && (path == "/v1/board/sync"
            || path == "/v1/mailboxes"
            || (path.starts_with("/v1/mailboxes/") && path.ends_with("/msgs")))
}

struct Pool {
    tokens: Vec<PrivacyPassToken>,
    rng: ChaCha20Rng,
}

/// Tokens from one server, refilled on demand.
pub struct TokenPool {
    server: SocketAddr,
    identified: std::sync::atomic::AtomicU64,
    server_id: ServerId,
    clock: ManualClock,
    pool: Mutex<Pool>,
}

impl TokenPool {
    pub(crate) fn new(server: SocketAddr, server_id: ServerId, clock: ManualClock) -> Self {
        Self {
            server,
            identified: std::sync::atomic::AtomicU64::new(0),
            server_id,
            clock,
            pool: Mutex::new(Pool {
                tokens: Vec::new(),
                rng: seeded_rng(0x70_6b_65_6e),
            }),
        }
    }

    async fn call(
        &self,
        route: (&str, &str),
        authorization: Option<&str>,
        body: Vec<u8>,
    ) -> Result<Vec<u8>, E2eError> {
        let (method, path) = route;
        self.identified
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (status, body) = http2(
            self.server,
            Http2Request {
                method,
                path,
                content_type: Some("application/cbor"),
                authorization,
                body,
            },
        )
        .await?;
        if status == 200 {
            Ok(body)
        } else {
            Err(E2eError::Status {
                what: path.to_owned(),
                status,
            })
        }
    }

    /// Registers a new account and fetches a batch for it.
    async fn refill(&self, pool: &mut Pool) -> Result<(), E2eError> {
        let key = AccountSigningKey::generate(&mut pool.rng);
        let registration = RegisterRequest {
            signature_key: *key.public_key().as_bytes(),
            proof: *sign_registration(&key, &self.server_id)?.as_bytes(),
            registration_code: None,
        };
        let registered = self
            .call(("POST", "/v1/accounts"), None, registration.encode()?)
            .await?;
        let auth = AuthRequest {
            account_id: RegisterResponse::decode(&registered)?.account_id,
            nonce_signature: *sign_auth_challenge(
                &key,
                &self.server_id,
                self.clock.now().bucket(),
            )?
            .as_bytes(),
        };
        let authed = self
            .call(("POST", "/v1/auth"), None, auth.encode()?)
            .await?;
        let bearer = format!("Bearer {}", AuthResponse::decode(&authed)?.bearer.expose());
        let keys =
            PrivacyPassKeys::decode(&self.call(("GET", "/v1/pp/keys"), None, Vec::new()).await?)?;
        let issuer_key = keys
            .keys
            .last()
            .ok_or_else(|| E2eError::step("the server has no issuer key"))?;
        let pending = begin_token_issuance(
            &issuer_key.public_key,
            TokenCount::new(BATCH)?,
            &mut pool.rng,
        )?;
        let issued = self
            .call(
                ("POST", "/v1/pp/issue"),
                Some(&bearer),
                PrivacyPassIssueRequest::new(pending.request().to_vec())?.encode()?,
            )
            .await?;
        pool.tokens
            .extend(pending.finish(PrivacyPassIssueResponse::decode(&issued)?.token_response())?);
        Ok(())
    }

    /// How many direct (identified or public) requests fetching tokens took.
    #[must_use]
    pub fn direct_requests(&self) -> u64 {
        self.identified.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Adds a token to `request` if its route needs an Anon credential.
    pub(crate) async fn attach(&self, request: &mut BinaryRequest) -> Result<(), E2eError> {
        if !needs_credential(request) {
            return Ok(());
        }
        let mut pool = self.pool.lock().await;
        if pool.tokens.is_empty() {
            self.refill(&mut pool).await?;
        }
        let token = pool
            .tokens
            .pop()
            .ok_or_else(|| E2eError::step("no token after a refill"))?;
        request.headers.push(Header {
            name: "authorization".to_owned(),
            value: token.authorization().as_bytes().to_vec(),
        });
        Ok(())
    }
}
