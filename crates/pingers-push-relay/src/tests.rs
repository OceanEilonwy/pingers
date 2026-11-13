// SPDX-License-Identifier: AGPL-3.0-only
//! Start-up: loading the configuration file and serving until shutdown.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::time::Duration;

use pingers_time::Timer as _;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn config_text(listen: &str, activated_at: u64) -> String {
    format!(
        r#"
listen = "{listen}"
operator = "Start-up Test"

[[keys]]
key_id = 1
private_key = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a"
public_key = "8520f0098930a754748b7ddcb43ef75c0dbf3a0d26381af4eba4a98eaa9b4e6a"
activated_at = {activated_at}

[[servers]]
server_id = "s"
credential = "0123456789abcdef0123456789abcdef"
pushes_per_minute = 1

[unifiedpush]
"#
    )
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
    let config = RelayConfig::parse(&config_text("127.0.0.1:0", u64::MAX))?;
    let result = serve(config, std::future::ready(())).await;
    assert!(matches!(result, Err(RelayError::NoActiveKey)));
    Ok(())
}

#[tokio::test]
async fn serve_reports_an_unreadable_fcm_token_file() -> TestResult {
    let text = config_text("127.0.0.1:0", 0).replace(
        "[unifiedpush]",
        "[fcm]\nproject_id = \"p\"\naccess_token_file = \"/nonexistent/pingers-fcm-token\"",
    );
    let result = serve(RelayConfig::parse(&text)?, std::future::ready(())).await;
    assert!(matches!(result, Err(RelayError::ReadFile { .. })));
    Ok(())
}

#[tokio::test]
async fn serve_reports_an_address_it_cannot_listen_on() -> TestResult {
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = taken.local_addr()?;
    let config = RelayConfig::parse(&config_text(&address.to_string(), 0))?;
    let result = serve(config, std::future::ready(())).await;
    assert!(matches!(result, Err(RelayError::Listen { address: failed, .. }) if failed == address));
    Ok(())
}

#[tokio::test]
async fn serve_publishes_the_key_config_until_shutdown() -> TestResult {
    // Find a free port, release it, and let serve bind it.
    let address = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await?
        .local_addr()?;
    let mut config = RelayConfig::parse(&config_text(&address.to_string(), 0))?;
    config.logging = Logging::Counts {
        interval: Duration::from_secs(3600),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve(config, async {
        drop(stopped.await);
    }));
    let timer = TokioTimer::new();
    let url = format!("http://{address}/v1/config");
    let mut published = None;
    for _ in 0..100 {
        if let Ok(response) = reqwest::get(&url).await {
            published = Some(response.json::<serde_json::Value>().await?);
            break;
        }
        timer.sleep(Duration::from_millis(20)).await;
    }
    let published = published.ok_or("the relay never answered")?;
    assert_eq!(
        published
            .get("operator")
            .and_then(serde_json::Value::as_str),
        Some("Start-up Test")
    );
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
