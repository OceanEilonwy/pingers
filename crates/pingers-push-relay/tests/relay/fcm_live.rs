// SPDX-License-Identifier: AGPL-3.0-only
//! Opt-in live test against Google: mints a real access token from the operator's service
//! account and sends one content-free push to a real device. Ignored by default; CI never runs
//! it. It prints nothing of the key or the token, only the delivery outcome on failure.
//!
//! ```sh
//! PINGERS_FCM_SERVICE_ACCOUNT=/path/to/service-account.json \
//! PINGERS_FCM_DEVICE_TOKEN=<registration token from the app> \
//! PINGERS_FCM_PROJECT_ID=pingers-a6b31 \
//!   cargo test -p pingers-push-relay --test relay fcm_live -- --ignored
//! ```
//!
//! `PINGERS_FCM_PROJECT_ID` defaults to `pingers-a6b31`. The device should receive a data
//! message `{"v":"1","h":"0123456789abcdef"}` at high priority.

use std::sync::Arc;
use std::time::Duration;

use pingers_proto::{PushHandle, PushPlatform, PushToken};
use pingers_push_relay::delivery::{
    DeliveryOutcome, Dispatcher, DispatcherSettings, FcmSettings, Priority, PushMessage,
    PushSender as _,
};
use pingers_push_relay::fcm_auth::{FcmAuth, ServiceAccount};
use pingers_time::SystemClock;
use pingers_transport::TokioTimer;
use reqwest::Url;
use zeroize::Zeroizing;

use crate::common::TestResult;

const IGNORE_REASON: &str = "sends a real push through Google; set PINGERS_FCM_SERVICE_ACCOUNT and PINGERS_FCM_DEVICE_TOKEN";

#[expect(
    clippy::disallowed_methods,
    reason = "the opt-in live test is configured by the person running it"
)]
fn environment(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[tokio::test]
#[ignore = "sends a real push through Google; set PINGERS_FCM_SERVICE_ACCOUNT and PINGERS_FCM_DEVICE_TOKEN"]
async fn fcm_live_mints_a_token_and_delivers_a_push() -> TestResult {
    let path = environment("PINGERS_FCM_SERVICE_ACCOUNT").ok_or(IGNORE_REASON)?;
    let device_token = environment("PINGERS_FCM_DEVICE_TOKEN").ok_or(IGNORE_REASON)?;
    let project_id =
        environment("PINGERS_FCM_PROJECT_ID").unwrap_or_else(|| "pingers-a6b31".to_owned());

    let text = Zeroizing::new(std::fs::read_to_string(&path)?);
    let account = ServiceAccount::from_json(&text)?;
    let auth = FcmAuth::new(account, Arc::new(SystemClock));
    let client = pingers_push_relay::public_http_client()?;
    // Mint once on its own, so a credentials problem is reported as such.
    auth.authorization(&client).await?;

    let settings = FcmSettings::new(
        &project_id,
        &Url::parse("https://fcm.googleapis.com/")?,
        auth,
    )
    .ok_or("invalid project id")?;
    let dispatcher = Dispatcher::new(
        client,
        TokioTimer::new(),
        DispatcherSettings {
            timeout: Duration::from_secs(20),
            unifiedpush: None,
            fcm: Some(settings),
        },
    );
    let outcome = dispatcher
        .send(
            PushPlatform::Fcm,
            &PushToken::from_bytes(device_token.into_bytes()),
            &PushMessage {
                handle: PushHandle::from_bytes([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]),
                priority: Priority::High,
                time_to_live: Duration::from_secs(60),
            },
        )
        .await;
    assert_eq!(outcome, DeliveryOutcome::Delivered);
    Ok(())
}
