// SPDX-License-Identifier: AGPL-3.0-only
//! What a restart keeps: accounts and bearers live in `quota_db`, so a device neither
//! registers again nor loses its bearer, a changed registration code does not lock it out,
//! and a deleted account stays deleted.

use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use pingers_crypto::{AccountSigningKey, sign_auth_challenge, sign_registration};
use pingers_proto::{
    AccountId, AuthRequest, AuthResponse, RegisterRequest, RegisterResponse, SecretText,
    WireDecode as _, WireEncode as _,
};
use pingers_time::Clock as _;
use zeroize::Zeroizing;

use super::wallet::{Wallet, direct};
use super::{Echo, Running, Setup, TestError, TestResult, configured, run, server_id};
use crate::Database;

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "pingers-server-{name}-{}.sqlite",
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let _stale = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    path
}

/// A server keeping its quota store in `quota_db`, taking only `code` for new accounts.
async fn server_with(quota_db: &Path, code: &str) -> Result<Running, TestError> {
    let mut config = configured(Setup::default())?;
    config.quota_store = Database::File(quota_db.to_path_buf());
    config.registration_codes = vec![SecretText::new(code.to_owned())];
    run(config, Echo).await
}

fn signing(key: u8) -> AccountSigningKey {
    AccountSigningKey::from_bytes(Zeroizing::new([key; 32]))
}

async fn register(server: &Running, key: u8, code: Option<&str>) -> Result<StatusCode, TestError> {
    let signing = signing(key);
    let registration = RegisterRequest {
        signature_key: *signing.public_key().as_bytes(),
        proof: *sign_registration(&signing, &server_id()?)?.as_bytes(),
        registration_code: code.map(|text| SecretText::new(text.to_owned())),
    };
    let answer = direct(server, "POST /v1/accounts", None, registration.encode()?).await?;
    if answer.status == StatusCode::OK {
        let id: AccountId = RegisterResponse::decode(&answer.body)?.account_id;
        assert_eq!(id, pingers_crypto::account_id(&signing.public_key()));
    }
    Ok(answer.status)
}

/// `Ok(Some(header))` with a fresh bearer, `Ok(None)` if the server refused.
async fn authenticate(server: &Running, key: u8) -> Result<Option<String>, TestError> {
    let signing = signing(key);
    let auth = AuthRequest {
        account_id: pingers_crypto::account_id(&signing.public_key()),
        nonce_signature: *sign_auth_challenge(
            &signing,
            &server_id()?,
            server.clock.now().bucket(),
        )?
        .as_bytes(),
    };
    let answer = direct(server, "POST /v1/auth", None, auth.encode()?).await?;
    if answer.status != StatusCode::OK {
        return Ok(None);
    }
    Ok(Some(format!(
        "Bearer {}",
        AuthResponse::decode(&answer.body)?.bearer.expose()
    )))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_keeps_accounts_and_bearers() -> TestResult {
    let quota_db = scratch("restart");
    let first = server_with(&quota_db, "old").await?;
    assert_eq!(register(&first, 1, Some("old")).await?, StatusCode::OK);
    let bearer = authenticate(&first, 1).await?.ok_or("no bearer")?;
    assert_eq!(register(&first, 2, Some("old")).await?, StatusCode::OK);
    let doomed = authenticate(&first, 2).await?.ok_or("no bearer")?;
    let deleted = direct(&first, "DELETE /v1/accounts/me", Some(&doomed), Vec::new()).await?;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    first.stop().await?;

    // The operator reset the code before the restart.
    let second = server_with(&quota_db, "new").await?;
    // The bearer made before the restart still buys tokens.
    let tokens = Wallet::issue(&second, &bearer, 1).await?;
    assert_eq!(tokens.len(), 1);
    // The device needs no code to register again, and can still authenticate.
    assert_eq!(register(&second, 1, None).await?, StatusCode::OK);
    assert!(authenticate(&second, 1).await?.is_some());
    // A new device with the old code is refused; the deleted account stays deleted.
    assert_eq!(
        register(&second, 3, Some("old")).await?,
        StatusCode::FORBIDDEN
    );
    assert!(authenticate(&second, 2).await?.is_none());
    let gone = direct(&second, "POST /v1/pp/issue", Some(&doomed), Vec::new()).await?;
    assert_eq!(gone.status, StatusCode::UNAUTHORIZED);
    // The bearer still ends an hour after it was made.
    second.clock.advance(pingers_quota::BEARER_LIFETIME);
    let expired = direct(&second, "POST /v1/pp/issue", Some(&bearer), Vec::new()).await?;
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    second.stop().await
}

/// Wrong registration codes are counted against the address they come from; a
/// `pingers-client-address` header a client sends itself is dropped, so it cannot dodge the
/// limit. Once over, even the right code is refused with 429 until the window ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_codes_from_one_address_are_limited_whatever_it_claims() -> TestResult {
    use axum::body::Body;
    use axum::http::{Method, Request};

    const CODE: &str = "0123456789abcdef0123456789abcdef";
    let server = server_with(&scratch("code-attempts"), CODE).await?;
    let attempt = |key: u8, code: &str, claimed: u8| -> Result<Request<Body>, TestError> {
        let signing = signing(key);
        let registration = RegisterRequest {
            signature_key: *signing.public_key().as_bytes(),
            proof: *sign_registration(&signing, &server_id()?)?.as_bytes(),
            registration_code: Some(SecretText::new(code.to_owned())),
        };
        Ok(Request::builder()
            .method(Method::POST)
            .uri("http://server/v1/accounts")
            .header("pingers-client-address", format!("192.0.2.{claimed}"))
            .body(Body::from(registration.encode()?))?)
    };
    let limit = u8::try_from(pingers_quota::CODE_ATTEMPTS_PER_ADDRESS)?;
    for index in 0..limit {
        let mut client = super::connect(server.address, super::LOOPBACK).await?;
        let refused = super::send(
            &mut client,
            attempt(index.wrapping_add(10), "wrong", index)?,
        )
        .await?;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }
    let mut client = super::connect(server.address, super::LOOPBACK).await?;
    let limited = super::send(&mut client, attempt(100, CODE, 200)?).await?;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    server.clock.advance(pingers_quota::CODE_ATTEMPT_WINDOW);
    assert_eq!(register(&server, 100, Some(CODE)).await?, StatusCode::OK);
    server.stop().await
}
