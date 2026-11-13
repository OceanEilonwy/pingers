// SPDX-License-Identifier: AGPL-3.0-only
//! Start-up: loading the configuration file and serving until shutdown.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::time::Duration;

use pingers_testkit::ManualClock;
use pingers_time::{Timer as _, UnixSeconds};

use pingers_proto::{WireDecode as _, WireEncode as _};

use super::*;
use crate::error::ServiceAccountError;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn config_text(listen: &str, activated_at: u64) -> String {
    format!(
        r#"
listen = "{listen}"
operator = "Start-up Test"
hash_key = "4242424242424242424242424242424242424242424242424242424242424242"

[[keys]]
key_id = 1
private_key = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a"
activated_at = {activated_at}

[[servers]]
server_id = "s"
credential_hash = "11e91a598f0cdd95086a51f58961207c05755742f65f5e2a4885bbe8721c3985"
pushes_per_minute = 1

[unifiedpush]
"#
    )
}

fn clock() -> ManualClock {
    ManualClock::new(UnixSeconds::new(1_800_000_000))
}

/// A file in the temporary directory, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, contents: &str) -> std::io::Result<Self> {
        let path =
            std::env::temp_dir().join(format!("pingers-push-relay-{}-{name}", std::process::id()));
        std::fs::write(&path, contents)?;
        Ok(Self(path))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        drop(std::fs::remove_file(&self.0));
    }
}

#[test]
fn load_config_reads_and_checks_the_file() -> TestResult {
    let file = TempFile::new("good.toml", &config_text("127.0.0.1:1", 0))?;
    let config = load_config(&file.0)?;
    assert_eq!(config.operator, "Start-up Test");
    let invalid = TempFile::new("bad.toml", "listen = 1")?;
    assert!(matches!(
        load_config(&invalid.0),
        Err(RelayError::Config(ConfigError::Syntax { .. }))
    ));
    Ok(())
}

#[test]
fn load_config_reports_a_missing_file() {
    let missing = std::env::temp_dir().join("pingers-push-relay-no-such-file.toml");
    assert!(matches!(
        load_config(&missing),
        Err(RelayError::ReadFile { path, .. }) if path == missing
    ));
}

#[tokio::test]
async fn serve_refuses_to_start_when_no_key_is_active_yet() -> TestResult {
    let config = RelayConfig::parse(&config_text("127.0.0.1:0", 1_000))?;
    let listener = listen(&config).await?;
    let clock = ManualClock::new(UnixSeconds::new(999));
    let result = serve(config, listener, clock, std::future::ready(())).await;
    assert!(matches!(result, Err(RelayError::NoActiveKey)));
    Ok(())
}

#[tokio::test]
async fn serve_reports_an_unreadable_service_account_file() -> TestResult {
    let text = config_text("127.0.0.1:0", 0).replace(
        "[unifiedpush]",
        "[fcm]\nproject_id = \"p\"\nservice_account_file = \"/nonexistent/pingers-fcm-service-account.json\"",
    );
    let config = RelayConfig::parse(&text)?;
    let listener = listen(&config).await?;
    let result = serve(config, listener, clock(), std::future::ready(())).await;
    assert!(matches!(result, Err(RelayError::ReadFile { .. })));
    Ok(())
}

fn fcm_config(service_account_file: &std::path::Path) -> Result<RelayConfig, ConfigError> {
    RelayConfig::parse(&config_text("127.0.0.1:0", 0).replace(
        "[unifiedpush]",
        &format!(
            "[fcm]\nproject_id = \"p\"\nservice_account_file = \"{}\"",
            service_account_file.display()
        ),
    ))
}

#[tokio::test]
async fn the_dispatcher_loads_the_service_account_file() -> TestResult {
    let file = TempFile::new(
        "good.json",
        &crate::fcm_auth::tests::account_json("https://oauth2.googleapis.com/token")?,
    )?;
    let sender = dispatcher(&fcm_config(&file.0)?, Arc::new(clock()))?;
    assert!(sender.supports(pingers_proto::PushPlatform::Fcm));
    assert!(!sender.supports(pingers_proto::PushPlatform::UnifiedPush));
    Ok(())
}

#[tokio::test]
async fn serve_reports_a_malformed_service_account_file_by_field() -> TestResult {
    let file = TempFile::new(
        "bad.json",
        r#"{"client_email":"","private_key":"x","token_uri":"https://oauth2.googleapis.com/token"}"#,
    )?;
    let config = fcm_config(&file.0)?;
    let listener = listen(&config).await?;
    let result = serve(config, listener, clock(), std::future::ready(())).await;
    assert!(matches!(
        result,
        Err(RelayError::ServiceAccount {
            source: ServiceAccountError::NoClientEmail,
            ..
        })
    ));
    Ok(())
}

#[tokio::test]
async fn listen_reports_an_address_it_cannot_bind() -> TestResult {
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = taken.local_addr()?;
    let config = RelayConfig::parse(&config_text(&address.to_string(), 0))?;
    let result = listen(&config).await;
    assert!(matches!(result, Err(RelayError::Listen { address: failed, .. }) if failed == address));
    Ok(())
}

#[tokio::test]
async fn serve_publishes_the_key_config_until_shutdown() -> TestResult {
    let mut config = RelayConfig::parse(&config_text("127.0.0.1:0", 0))?;
    let listener = listen(&config).await?;
    let address = listener.local_addr()?;
    config.logging = Logging::Counts {
        interval: Duration::from_secs(3600),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve(config, listener, clock(), async {
        drop(stopped.await);
    }));
    let timer = TokioTimer::new();
    let url = format!("http://{address}/v1/config");
    let mut published = None;
    for _ in 0..100 {
        if let Ok(response) = reqwest::get(&url).await {
            published = Some(pingers_proto::PushRelayConfig::decode(
                &response.bytes().await?,
            )?);
            break;
        }
        timer.sleep(Duration::from_millis(20)).await;
    }
    let published = published.ok_or("the relay never answered")?;
    assert_eq!(published.operator, "Start-up Test");
    stop.send(()).map_err(|()| "the server stopped early")?;
    let result = server.await?;
    assert!(result.is_ok());
    Ok(())
}

#[tokio::test]
async fn the_http_client_does_not_follow_redirects() -> TestResult {
    let router = axum::Router::new().fallback(|| async {
        (
            axum::http::StatusCode::TEMPORARY_REDIRECT,
            [(axum::http::header::LOCATION, "/again")],
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        drop(axum::serve(listener, router).await);
    });
    let response = http_client()?
        .post(format!("http://{address}/up"))
        .send()
        .await;
    server.abort();
    assert_eq!(
        response?.status(),
        axum::http::StatusCode::TEMPORARY_REDIRECT
    );
    Ok(())
}

/// A capturing sender, as an in-process harness would inject through `serve_with`.
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl PushSender for Capture {
    fn supports(&self, _platform: pingers_proto::PushPlatform) -> bool {
        true
    }

    fn send(
        &self,
        _platform: pingers_proto::PushPlatform,
        _token: &pingers_proto::PushToken,
        message: &delivery::PushMessage,
    ) -> impl Future<Output = delivery::DeliveryOutcome> + Send {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message.payload());
        std::future::ready(delivery::DeliveryOutcome::Delivered)
    }
}

#[tokio::test]
async fn serve_with_uses_the_injected_sender_and_clock() -> TestResult {
    let config = RelayConfig::parse(&config_text("127.0.0.1:0", 4_000_000_000))?;
    let listener = listen(&config).await?;
    let address = listener.local_addr()?;
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve_with(
        config,
        Outside {
            sender: Capture(std::sync::Arc::clone(&captured)),
            clock: ManualClock::new(UnixSeconds::new(4_000_000_000)),
        },
        listener,
        async {
            drop(stopped.await);
        },
    ));
    // The real opener refuses this garbage blob (422), so nothing reaches the sender; the
    // injected clock made key 1 (active from 2096) usable, or serve_with would have
    // refused to start.
    let body = pingers_proto::PushRequest {
        key_id: pingers_proto::PushRelayKeyId::new(1),
        blob: pingers_proto::SealedPushBlob::from_bytes([0; 320]),
        bind: pingers_crypto::PushBind::from_bytes([0; 32]),
        urgency: pingers_proto::Urgency::Alert,
        handle: pingers_proto::PushHandle::from_bytes([0; 8]),
    }
    .encode()?;
    let response = reqwest::Client::new()
        .post(format!("http://{address}/v1/push"))
        .bearer_auth("0123456789abcdef0123456789abcdef")
        .body(body)
        .send()
        .await?;
    assert_ne!(response.status(), axum::http::StatusCode::ACCEPTED);
    assert!(
        captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    stop.send(()).map_err(|()| "the server stopped early")?;
    assert!(server.await?.is_ok());
    Ok(())
}
