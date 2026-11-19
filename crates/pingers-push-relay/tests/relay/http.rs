// SPDX-License-Identifier: AGPL-3.0-only
//! The HTTP API end to end in-process: real router, real dispatcher, a fake opener (real HPKE
//! is covered by the ntfy end-to-end test) and a loopback distributor.

use std::time::Duration;

use axum::http::StatusCode;
use pingers_proto::{
    PushBind, PushClass, PushHandle, PushPlatform, PushRelayConfig, PushRelayKeyConfig,
    PushRelayKeyId, PushRequest, PushToken, PushTokenPlaintext, SealedPushBlob, Urgency,
    WireDecode as _, WireEncode as _,
};
use pingers_push_relay::delivery::{Dispatcher, DispatcherSettings, EndpointPolicy};
use pingers_push_relay::http::PushSenders;
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
        key_id: PushRelayKeyId::new(KEY_ID),
        blob: *blob,
        bind: PushBind::from_bytes(bind),
        urgency: Urgency::Alert,
        handle: PushHandle::from_bytes(HANDLE),
    }
    .encode()
    .unwrap_or_default()
}

async fn relay_with(opener: FakeOpener) -> Result<RunningRelay, Box<dyn std::error::Error>> {
    relay_for(opener, PushSenders::Anywhere).await
}

async fn relay_for(
    opener: FakeOpener,
    senders: PushSenders,
) -> Result<RunningRelay, Box<dyn std::error::Error>> {
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
    RunningRelay::start_for(opener, dispatcher, clock, senders).await
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

/// `push_direct_only`, as on the appliance: a push the web proxy forwarded (Caddy's headers,
/// or another proxy's) gets 404 whatever its credential or method, and sends nothing, as the
/// proxy's own rule used to answer; the server on loopback, sending none of them, is served.
/// Devices still read the key config through the proxy.
#[tokio::test]
async fn a_direct_only_relay_refuses_forwarded_pushes_with_404() -> TestResult {
    let distributor = Distributor::start(StatusCode::CREATED).await?;
    let mut opener = FakeOpener::default();
    let blob = opener.seal(BIND, 1, plaintext(&distributor.endpoint("upTopic")));
    let relay = relay_for(opener, PushSenders::DirectOnly).await?;
    let client = reqwest::Client::new();
    let caddy = [
        ("x-forwarded-for", "198.51.100.7"),
        ("x-forwarded-proto", "https"),
        ("x-forwarded-host", "push.example.org"),
    ];
    let mut forwarded = vec![caddy.to_vec()];
    for other in [
        ("x-forwarded-for", "127.0.0.1"),
        ("forwarded", "for=198.51.100.7;proto=https"),
        ("via", "1.1 router"),
    ] {
        forwarded.push(vec![other]);
    }
    for headers in &forwarded {
        for credential in [Some(CREDENTIAL), None] {
            let mut builder = client
                .post(relay.url("/v1/push"))
                .body(request(&blob, BIND));
            for (name, value) in headers {
                builder = builder.header(*name, *value);
            }
            if let Some(credential) = credential {
                builder = builder.bearer_auth(credential);
            }
            let response = builder.send().await?;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{headers:?}");
        }
        let mut get = client.get(relay.url("/v1/push"));
        for (name, value) in headers {
            get = get.header(*name, *value);
        }
        assert_eq!(get.send().await?.status(), StatusCode::NOT_FOUND);
    }
    assert!(distributor.received().is_empty());
    let mut config = client.get(relay.url("/v1/config"));
    for (name, value) in caddy {
        config = config.header(name, value);
    }
    assert_eq!(config.send().await?.status(), StatusCode::OK);
    // pingers-server on loopback: no proxy headers.
    let response = post(&relay, Some(CREDENTIAL), request(&blob, BIND)).await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(distributor.received().len(), 1);
    Ok(())
}

/// By default a relay takes pushes through a reverse proxy: servers on other hosts reach a
/// standalone relay through the TLS front in front of it.
#[tokio::test]
async fn by_default_pushes_through_a_proxy_are_served() -> TestResult {
    let distributor = Distributor::start(StatusCode::CREATED).await?;
    let mut opener = FakeOpener::default();
    let blob = opener.seal(BIND, 1, plaintext(&distributor.endpoint("upTopic")));
    let relay = relay_with(opener).await?;
    let response = reqwest::Client::new()
        .post(relay.url("/v1/push"))
        .header("x-forwarded-for", "198.51.100.7")
        .bearer_auth(CREDENTIAL)
        .body(request(&blob, BIND))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
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
    // A push request whose bind is one byte short.
    let mut short_bind = request(&SealedPushBlob::from_bytes([1; 320]), BIND);
    if let Some(position) = short_bind.windows(2).position(|pair| pair == [0x58, 0x20]) {
        short_bind.splice(position..position.saturating_add(3), [0x58, 0x1f]);
    }
    let response = post(&relay, Some(CREDENTIAL), short_bind).await?;
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
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .map(reqwest::header::HeaderValue::as_bytes),
        Some(&b"application/cbor"[..])
    );
    let config = PushRelayConfig::decode(&response.bytes().await?)?;
    assert_eq!(
        config.key_config,
        PushRelayKeyConfig {
            key_id: PushRelayKeyId::new(KEY_ID),
            public_key: crate::common::RELAY_PUBLIC,
        }
    );
    // What devices pin: SHA-256 of the RFC 9458 encoding.
    let mut encoded = vec![KEY_ID, 0x00, 0x20];
    encoded.extend_from_slice(&crate::common::RELAY_PUBLIC);
    encoded.extend_from_slice(&[0x00, 0x04, 0x00, 0x01, 0x00, 0x01]);
    assert_eq!(config.key_config.to_bytes().as_slice(), encoded.as_slice());
    assert_eq!(config.platforms, vec![PushPlatform::UnifiedPush]);
    assert_eq!(
        config.max_time_to_live.duration(),
        Duration::from_hours(30 * 24)
    );
    assert_eq!(config.operator, "Integration Test Relay");
    Ok(())
}

#[tokio::test]
async fn the_push_endpoint_accepts_only_post() -> TestResult {
    let relay = relay_with(FakeOpener::default()).await?;
    let response = reqwest::get(relay.url("/v1/push")).await?;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn without_a_scheduled_key_there_is_no_next_config() -> TestResult {
    let relay = relay_with(FakeOpener::default()).await?;
    let response = reqwest::get(relay.url("/v1/config/next")).await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn the_status_is_uncached_json_without_counts() -> TestResult {
    let relay = relay_with(FakeOpener::default()).await?;
    let response = reqwest::get(relay.url("/status")).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .map(reqwest::header::HeaderValue::as_bytes),
        Some(&b"no-store"[..])
    );
    let status: serde_json::Value = serde_json::from_slice(&response.bytes().await?)?;
    assert_eq!(
        status,
        serde_json::json!({
            "service": "pingers-push-relay",
            "version": env!("CARGO_PKG_VERSION"),
            "status": "ok",
            "operator": "Integration Test Relay",
            "platforms": ["unifiedpush"],
            "published_key_id": KEY_ID,
            "next_key_id": null,
            "next_key_activates_at": null,
        })
    );
    Ok(())
}
