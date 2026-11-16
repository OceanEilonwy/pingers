// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask pins --relay --push` end to end, against a local HTTP server that answers as
//! the OHTTP relay and the push relay do: the live lines must equal the file mode's for the
//! same keys.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use pingers_crypto::PushRelayPrivateKey;
use pingers_ohttp::GatewayKey;
use pingers_proto::{
    PushPlatform, PushRelayConfig, PushRelayKeyConfig, PushRelayKeyId, TimeToLive, WireEncode as _,
};
use tokio::net::TcpListener;
use zeroize::Zeroizing;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const KEY_A: [u8; 32] = [0x77; 32];
const KEY_B: [u8; 32] = [0x11; 32];

/// What the fake services answer, by path.
type Routes = HashMap<&'static str, (StatusCode, Vec<u8>)>;

fn gateway_config(secret: [u8; 32]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(GatewayKey::from_bytes(1, Zeroizing::new(secret))?
        .config()
        .encoded()
        .to_vec())
}

fn push_relay_config(key_id: u8, secret: [u8; 32]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let public_key = PushRelayPrivateKey::from_bytes(Zeroizing::new(secret)).public_key(key_id)?;
    Ok(PushRelayConfig {
        key_config: PushRelayKeyConfig {
            key_id: PushRelayKeyId::new(key_id),
            public_key: *public_key.public_key(),
        },
        platforms: vec![PushPlatform::UnifiedPush],
        max_time_to_live: TimeToLive::new(Duration::from_secs(3600))?,
        operator: "Test relay".to_owned(),
    }
    .encode()?)
}

/// Both services answering normally, with a next push relay key scheduled.
fn healthy() -> Result<Routes, Box<dyn std::error::Error>> {
    Ok(HashMap::from([
        ("/v1/ohttp/config", (StatusCode::OK, gateway_config(KEY_A)?)),
        ("/v1/config", (StatusCode::OK, push_relay_config(1, KEY_A)?)),
        (
            "/v1/config/next",
            (StatusCode::OK, push_relay_config(2, KEY_B)?),
        ),
    ]))
}

/// Serves `routes` until the returned handle is aborted; anything else is a 404.
async fn serve(listener: TcpListener, routes: Arc<Routes>) {
    while let Ok((stream, _peer)) = listener.accept().await {
        let routes = Arc::clone(&routes);
        let service = hyper::service::service_fn(move |request: Request<Incoming>| {
            let (status, body) = routes
                .get(request.uri().path())
                .cloned()
                .unwrap_or((StatusCode::NOT_FOUND, Vec::new()));
            let mut response = Response::new(Full::new(Bytes::from(body)));
            *response.status_mut() = status;
            async move { Ok::<_, Infallible>(response) }
        });
        let _closed = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await;
    }
}

/// Runs `cargo xtask pins --relay http://<server>/ohttp --push http://<server>` against
/// `routes`.
fn live(routes: Routes) -> Result<Output, Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(serve(listener, Arc::new(routes)));
        let output = tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_xtask"))
                .args(["pins", "--relay"])
                .arg(format!("http://{address}/ohttp"))
                .arg("--push")
                .arg(format!("http://{address}"))
                .output()
        })
        .await;
        server.abort();
        Ok(output??)
    })
}

/// `cargo xtask pins <server.toml> <push-relay.toml>` for the same keys.
fn from_files(relay_keys: &[(u8, [u8; 32], u64)]) -> Result<String, Box<dyn std::error::Error>> {
    // Tests in this binary run in parallel, so each call writes into its own directory: two
    // writers of one fixed file name let a reader see a half-written file.
    static CALL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let call = CALL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("xtask-pins-live-{}-{call}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let server = directory.join("server.toml");
    std::fs::write(
        &server,
        format!(
            "listen = \"0.0.0.0:1\"\nserver_id = \"https://pingers.test\"\n\
             [ohttp]\nrelays = [\"127.0.0.1\"]\n[[ohttp.keys]]\nkey_id = 1\nsecret = \"{}\"\n",
            hex::encode(KEY_A)
        ),
    )?;
    let relay = directory.join("push-relay.toml");
    let mut keys = String::new();
    for (key_id, secret, activated_at) in relay_keys {
        writeln!(
            keys,
            "[[keys]]\nkey_id = {key_id}\nprivate_key = \"{}\"\nactivated_at = {activated_at}",
            hex::encode(secret)
        )?;
    }
    std::fs::write(&relay, keys)?;
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("pins")
        .arg(&server)
        .arg(&relay)
        .output()?;
    assert!(output.status.success());
    Ok(String::from_utf8(output.stdout)?)
}

#[test]
fn live_pins_equal_the_file_mode_for_the_same_keys() -> TestResult {
    let output = live(healthy()?)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = from_files(&[(1, KEY_A, 0), (2, KEY_B, 4_102_444_800)])?;
    assert_eq!(String::from_utf8(output.stdout)?, expected);
    Ok(())
}

#[test]
fn without_a_next_key_only_the_published_one_is_pinned() -> TestResult {
    let mut routes = healthy()?;
    routes.remove("/v1/config/next");
    let output = live(routes)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)?,
        from_files(&[(1, KEY_A, 0)])?
    );
    Ok(())
}

#[test]
fn a_failing_or_malformed_answer_fails_and_names_the_url() -> TestResult {
    let mut unavailable = healthy()?;
    unavailable.insert(
        "/v1/ohttp/config",
        (StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
    );
    let mut garbage = healthy()?;
    garbage.insert("/v1/config", (StatusCode::OK, b"not cbor".to_vec()));
    let mut no_published = healthy()?;
    no_published.remove("/v1/config");
    let mut oversized = healthy()?;
    oversized.insert("/v1/config/next", (StatusCode::OK, vec![0; 4097]));
    for (routes, expected) in [
        (unavailable, "/v1/ohttp/config: answered 503"),
        (garbage, "/v1/config: not a valid push relay config"),
        (no_published, "/v1/config: no published key (404)"),
        (oversized, "/v1/config/next: body over 4096 bytes"),
    ] {
        let output = live(routes)?;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains(expected), "{stderr}");
    }
    Ok(())
}

#[test]
fn a_gateway_answer_that_is_not_a_key_config_fails() -> TestResult {
    let mut routes = healthy()?;
    routes.insert("/v1/ohttp/config", (StatusCode::OK, vec![1, 0, 0x20]));
    let output = live(routes)?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("not a valid OHTTP key config"));
    Ok(())
}

#[test]
fn live_mode_needs_both_urls() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["pins", "--relay", "https://relay.test"])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("usage"));
    Ok(())
}
