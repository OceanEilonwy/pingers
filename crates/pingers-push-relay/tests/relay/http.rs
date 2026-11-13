// SPDX-License-Identifier: AGPL-3.0-only
//! The HTTP API end to end in-process: real router, real dispatcher, a fake opener (real HPKE
//! is covered by the ntfy end-to-end test) and a loopback distributor.

use std::time::Duration;

use axum::http::StatusCode;
use pingers_crypto::PushBind;
use pingers_proto::{
    PushClass, PushHandle, PushPlatform, PushToken, PushTokenPlaintext, SealedPushBlob, Urgency,
};
use pingers_push_relay::delivery::{Dispatcher, DispatcherSettings, EndpointPolicy};
use pingers_push_relay::keys::KeyId;
use pingers_push_relay::request::PushRequest;
use pingers_testkit::ManualClock;
use pingers_time::UnixSeconds;

use crate::common::{CREDENTIAL, Distributor, FakeOpener, KEY_ID, RunningRelay, TestResult};

const NOW: u64 = 1_800_000_000;
const BIND: [u8; 32] = [0x42; 32];
const HANDLE: [u8; 8] = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80];

fn plaintext(endpoint: &str) -> PushTokenPlaintext {
    PushTokenPlaintext {
        platform: PushPlatform::UnifiedPush,
        class: PushClass::Standard,
        token: PushToken::from_bytes(endpoint.as_bytes().to_vec()),
        not_after: UnixSeconds::new(NOW + 3600),
    }
}

fn request(blob: &SealedPushBlob, bind: [u8; 32]) -> Vec<u8> {
    PushRequest {
        key_id: KeyId::new(KEY_ID),
        blob: *blob,
        bind: PushBind::from_bytes(bind),
        urgency: Urgency::Alert,
        handle: PushHandle::from_bytes(HANDLE),
    }
    .encode_json()
}

async fn relay_with(opener: FakeOpener) -> Result<RunningRelay, Box<dyn std::error::Error>> {
    let clock = ManualClock::new(UnixSeconds::new(NOW));
    let dispatcher = Dispatcher::new(
        pingers_push_relay::http_client()?,
        clock.clone(),
        DispatcherSettings {
            timeout: Duration::from_secs(10),
            unifiedpush: Some(EndpointPolicy::AllowInsecureLocal),
            fcm: None,
        },
    );
    RunningRelay::start(opener, dispatcher, clock).await
}

async fn post(
    relay: &RunningRelay,
    credential: Option<&str>,
    body: Vec<u8>,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut builder = reqwest::Client::new()
        .post(relay.url("/v1/push"))
        .body(body);
    if let Some(credential) = credential {
        builder = builder.bearer_auth(credential);
    }
    builder.send().await
}

#[tokio::test]
async fn a_push_reaches_the_distributor_and_answers_202() -> TestResult {
    let distributor = Distributor::start(StatusCode::CREATED).await?;
    let mut opener = FakeOpener::default();
    let blob = opener.seal(BIND, 1, plaintext(&distributor.endpoint("upTopic")));
    let relay = relay_with(opener).await?;
    let response = post(&relay, Some(CREDENTIAL), request(&blob, BIND)).await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(response.content_length(), Some(0));
    assert_eq!(
        distributor.received(),
        vec![r#"{"v":1,"h":"1020304050607080"}"#.to_owned()]
    );
    Ok(())
}

#[tokio::test]
async fn without_a_credential_the_answer_is_401_with_a_bearer_challenge() -> TestResult {
    let relay = relay_with(FakeOpener::default()).await?;
    let response = post(&relay, None, b"{}".to_vec()).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .map(reqwest::header::HeaderValue::as_bytes),
        Some(&b"Bearer"[..])
    );
    let response = post(
        &relay,
        Some("wrong-credential-0123456789abcdef"),
        b"{}".to_vec(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn a_blob_moved_to_another_mailbox_answers_422_and_sends_nothing() -> TestResult {
    let distributor = Distributor::start(StatusCode::CREATED).await?;
    let mut opener = FakeOpener::default();
    let blob = opener.seal(BIND, 1, plaintext(&distributor.endpoint("upTopic")));
    let relay = relay_with(opener).await?;
    let response = post(&relay, Some(CREDENTIAL), request(&blob, [0x43; 32])).await?;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(distributor.received().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_token_the_distributor_has_dropped_answers_410() -> TestResult {
    let distributor = Distributor::start(StatusCode::NOT_FOUND).await?;
    let mut opener = FakeOpener::default();
    let blob = opener.seal(BIND, 1, plaintext(&distributor.endpoint("upGone")));
    let relay = relay_with(opener).await?;
    let response = post(&relay, Some(CREDENTIAL), request(&blob, BIND)).await?;
    assert_eq!(response.status(), StatusCode::GONE);
    Ok(())
}

#[tokio::test]
async fn a_replayed_blob_answers_429_after_the_token_budget() -> TestResult {
    let distributor = Distributor::start(StatusCode::CREATED).await?;
    let mut opener = FakeOpener::default();
    let blob = opener.seal(BIND, 1, plaintext(&distributor.endpoint("upReplay")));
    let relay = relay_with(opener).await?;
    // The shared fixture allows 2 pushes per minute per token.
    for expected in [
        StatusCode::ACCEPTED,
        StatusCode::ACCEPTED,
        StatusCode::TOO_MANY_REQUESTS,
    ] {
        let response = post(&relay, Some(CREDENTIAL), request(&blob, BIND)).await?;
        assert_eq!(response.status(), expected);
    }
    assert_eq!(distributor.received().len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_malformed_body_answers_400_and_an_oversized_one_413() -> TestResult {
    let relay = relay_with(FakeOpener::default()).await?;
    let response = post(&relay, Some(CREDENTIAL), b"{\"key_id\":1}".to_vec()).await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = post(&relay, Some(CREDENTIAL), vec![b' '; 5000]).await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn the_key_config_is_public_cacheable_and_pinnable() -> TestResult {
    let relay = relay_with(FakeOpener::default()).await?;
    let response = reqwest::get(relay.url("/v1/config")).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .map(reqwest::header::HeaderValue::as_bytes),
        Some(&b"public, max-age=3600"[..])
    );
    let body: serde_json::Value = response.json().await?;
    let public_key = hex::encode(crate::common::RELAY_PUBLIC);
    let key_config = format!("010020{public_key}000400010001");
    let key_config_bytes = hex::decode(&key_config)?;
    let expected_hash = hex::encode(pingers_crypto::key_config_hash(&key_config_bytes).as_bytes());
    assert_eq!(
        body,
        serde_json::json!({
            "key_id": 1,
            "kem": "X25519",
            "kdf": "HkdfSha256",
            "aead": "Aes128Gcm",
            "public_key": public_key,
            "key_config": key_config,
            "key_config_hash": expected_hash,
            "platforms": ["UnifiedPush"],
            "max_ttl_s": 2_592_000,
            "operator": "Integration Test Relay",
        })
    );
    Ok(())
}

#[tokio::test]
async fn the_push_endpoint_accepts_only_post() -> TestResult {
    let relay = relay_with(FakeOpener::default()).await?;
    let response = reqwest::get(relay.url("/v1/push")).await?;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}
