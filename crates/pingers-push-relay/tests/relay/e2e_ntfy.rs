// SPDX-License-Identifier: AGPL-3.0-only
//! End-to-end tests against a real UnifiedPush distributor (ntfy) on loopback. Opt-in: CI has
//! no ntfy. `scripts/e2e-ntfy.sh` starts ntfy and the relay binary and runs these with
//!
//! - `NTFY_URL`: the local ntfy, e.g. `http://127.0.0.1:18080`
//! - `RELAY_URL`: the running `pingers-push-relay` binary, configured with the RFC 7748 key
//!   pair from `common` as key 1 and [`CREDENTIAL`] as a server credential.

use std::time::Duration;

use axum::http::StatusCode;
use pingers_crypto::{PushRelayPublicKey, PushTokenToSeal, push_bind_for_mailbox};
use pingers_proto::{
    MailboxId, PushClass, PushHandle, PushPlatform, PushToken, PushTokenPlaintext, Urgency,
};
use pingers_push_relay::delivery::{Dispatcher, DispatcherSettings, EndpointPolicy};
use pingers_push_relay::keys::KeyId;
use pingers_push_relay::request::PushRequest;
use pingers_testkit::{ManualClock, seeded_rng};
use pingers_time::{Clock, SystemClock, Timer, UnixSeconds};
use pingers_transport::TokioTimer;

use crate::common::{CREDENTIAL, FakeOpener, KEY_ID, RELAY_PUBLIC, RunningRelay, TestResult};

const IGNORE_REASON: &str = "needs a local ntfy; run crates/pingers-push-relay/scripts/e2e-ntfy.sh";

#[expect(
    clippy::disallowed_methods,
    reason = "the opt-in end-to-end test is configured by the script that starts ntfy and the relay"
)]
fn environment(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set ({IGNORE_REASON})"))
}

fn handle_hex(handle: PushHandle) -> String {
    hex::encode(handle.as_bytes())
}

/// Polls ntfy's subscribe API until a message with exactly `payload` appears on `topic`.
async fn wait_for_message(
    ntfy: &str,
    topic: &str,
    payload: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let timer = TokioTimer::new();
    let client = reqwest::Client::new();
    for _ in 0..50 {
        let body = client
            .get(format!("{ntfy}/{topic}/json?poll=1&since=all"))
            .send()
            .await?
            .text()
            .await?;
        for line in body.lines() {
            let event: serde_json::Value = serde_json::from_str(line)?;
            if event.get("message").and_then(serde_json::Value::as_str) == Some(payload) {
                return Ok(event);
            }
        }
        timer.sleep(Duration::from_millis(100)).await;
    }
    Err(format!("no message {payload} on ntfy topic {topic}").into())
}

/// The relay's UnifiedPush path against real ntfy, independent of the HPKE implementation:
/// in-process relay, fake opener, real dispatcher and timer.
#[tokio::test]
#[ignore = "needs a local ntfy; run crates/pingers-push-relay/scripts/e2e-ntfy.sh"]
async fn unifiedpush_push_arrives_on_ntfy() -> TestResult {
    let ntfy = environment("NTFY_URL")?;
    let topic = "upPingersFakeOpener";
    let bind = [0x24; 32];
    let handle = PushHandle::from_bytes(*b"fakeopen");
    let clock = ManualClock::new(UnixSeconds::new(1_800_000_000));
    let mut opener = FakeOpener::default();
    let blob = opener.seal(
        bind,
        9,
        PushTokenPlaintext {
            platform: PushPlatform::UnifiedPush,
            class: PushClass::Standard,
            token: PushToken::from_bytes(format!("{ntfy}/{topic}?up=1").into_bytes()),
            not_after: UnixSeconds::new(1_800_003_600),
        },
    );
    let dispatcher = Dispatcher::new(
        pingers_push_relay::http_client()?,
        TokioTimer::new(),
        DispatcherSettings {
            timeout: Duration::from_secs(5),
            unifiedpush: Some(EndpointPolicy::AllowInsecureLocal),
            fcm: None,
        },
    );
    let relay = RunningRelay::start(opener, dispatcher, clock).await?;
    let body = PushRequest {
        key_id: KeyId::new(KEY_ID),
        blob,
        bind: pingers_crypto::PushBind::from_bytes(bind),
        urgency: Urgency::Quiet,
        handle,
    }
    .encode_json();
    let response = reqwest::Client::new()
        .post(relay.url("/v1/push"))
        .bearer_auth(CREDENTIAL)
        .body(body)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let payload = format!(r#"{{"v":1,"h":"{}"}}"#, handle_hex(handle));
    wait_for_message(&ntfy, topic, &payload).await?;
    Ok(())
}

/// The whole chain: a device seals its ntfy endpoint to the relay key for one mailbox, the
/// "server" forwards blob and bind to the running relay binary, and the content-free push
/// arrives on ntfy. Presenting the blob under another mailbox's bind is refused.
#[tokio::test]
#[ignore = "needs a local ntfy; run crates/pingers-push-relay/scripts/e2e-ntfy.sh"]
async fn sealed_token_push_arrives_on_ntfy_through_the_relay_binary() -> TestResult {
    let ntfy = environment("NTFY_URL")?;
    let relay = environment("RELAY_URL")?;
    let topic = "upPingersSealed";
    let mut rng = seeded_rng(0x5eed);
    let mailbox = MailboxId::from_bytes([0x4d; 16]);
    let bind = push_bind_for_mailbox(&mailbox);
    let handle = PushHandle::from_bytes(*b"sealed!!");
    // The relay binary checks expiry against the real clock.
    let not_after = SystemClock
        .now()
        .checked_add(Duration::from_hours(1))
        .ok_or("clock overflow")?;
    let plaintext = PushTokenPlaintext {
        platform: PushPlatform::UnifiedPush,
        class: PushClass::Location,
        token: PushToken::from_bytes(format!("{ntfy}/{topic}?up=1").into_bytes()),
        not_after,
    };
    let blob = pingers_crypto::seal_push_token(
        &PushRelayPublicKey::new(KEY_ID, RELAY_PUBLIC),
        &PushTokenToSeal {
            bind,
            plaintext: &plaintext,
        },
        &mut rng,
    )
    .map_err(|error| {
        format!("seal_push_token failed: {error} (is pingers-crypto's sealed push implemented?)")
    })?;
    let client = reqwest::Client::new();
    let send = |bind| {
        let body = PushRequest {
            key_id: KeyId::new(KEY_ID),
            blob,
            bind,
            urgency: Urgency::Alert,
            handle,
        }
        .encode_json();
        client
            .post(format!("{relay}/v1/push"))
            .bearer_auth(CREDENTIAL)
            .body(body)
            .send()
    };

    let other_mailbox = push_bind_for_mailbox(&MailboxId::from_bytes([0x4e; 16]));
    assert_eq!(
        send(other_mailbox).await?.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(send(bind).await?.status(), StatusCode::ACCEPTED);
    let payload = format!(r#"{{"v":1,"h":"{}"}}"#, handle_hex(handle));
    let event = wait_for_message(&ntfy, topic, &payload).await?;
    assert_eq!(
        event.get("topic").and_then(serde_json::Value::as_str),
        Some(topic)
    );
    Ok(())
}
