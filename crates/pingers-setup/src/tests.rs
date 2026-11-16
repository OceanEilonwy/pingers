// SPDX-License-Identifier: AGPL-3.0-only
//! The page served over loopback against fake services: an HTTP/2-only gateway with two keys,
//! a push relay with a next key, and a map host with a catalog. The public names are under
//! `.invalid`, which never resolves, so the public checks say they could not be tried.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use pingers_ohttp::GatewayKey;
use pingers_proto::{
    KeyConfigHash, PushPlatform, PushRelayConfig, PushRelayKeyConfig, PushRelayKeyId,
    ScheduledPushRelayConfig, SetupPayload, TimeToLive, WireEncode as _,
};
use rand_core::SeedableRng as _;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Serves `router` on a loopback port (HTTP/1.1, and HTTP/2 with prior knowledge).
async fn serve_fake(router: Router) -> Result<SocketAddr, std::io::Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(address)
}

fn relay_config(key_id: u8, public: u8) -> Result<PushRelayConfig, pingers_proto::ProtoError> {
    Ok(PushRelayConfig {
        key_config: PushRelayKeyConfig {
            key_id: PushRelayKeyId::new(key_id),
            public_key: [public; 32],
        },
        platforms: vec![PushPlatform::Fcm, PushPlatform::UnifiedPush],
        max_time_to_live: TimeToLive::new(Duration::from_hours(24))?,
        operator: "example.invalid".to_owned(),
    })
}

/// The expected pins, and the setup config pointing at the fakes.
struct Fakes {
    gateway: Vec<KeyConfigHash>,
    push: Vec<KeyConfigHash>,
    config: Config,
}

async fn fakes() -> Result<Fakes, Box<dyn std::error::Error>> {
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(7);
    let served = GatewayKey::generate(1, &mut rng)?.config();
    let next = GatewayKey::generate(2, &mut rng)?.config();
    let gateway_pins = vec![served.hash(), next.hash()];
    let (served, next) = (served.encoded().to_vec(), next.encoded().to_vec());
    let gateway = serve_fake(
        Router::new()
            .route(
                "/v1/ohttp/config",
                get(move || std::future::ready(served.clone())),
            )
            .route(
                "/v1/ohttp/config/next",
                get(move || std::future::ready(next.clone())),
            ),
    )
    .await?;
    let published = relay_config(1, 5)?;
    let upcoming = relay_config(2, 6)?;
    let push_pins = [&published, &upcoming]
        .map(|config| pingers_crypto::key_config_hash(&config.key_config.to_bytes()))
        .to_vec();
    let published = published.encode()?;
    let upcoming = ScheduledPushRelayConfig {
        config: upcoming,
        activates_at: pingers_time::UnixSeconds::new(4_102_444_800),
    }
    .encode()?;
    let push = serve_fake(
        Router::new()
            .route(
                "/v1/config",
                get(move || std::future::ready(published.clone())),
            )
            .route(
                "/v1/config/next",
                get(move || std::future::ready(upcoming.clone())),
            ),
    )
    .await?;
    let maps = serve_fake(Router::new().route(
        "/catalog.json",
        get(|| std::future::ready(r#"{"format":1,"regions":[{"id":"a"},{"id":"b"}]}"#)),
    ))
    .await?;
    let text = crate::config::tests::file()
        .replace("example.org", "example.invalid")
        .replace("http://127.0.0.1:9471", &format!("http://{gateway}"))
        .replace("http://127.0.0.1:9472", &format!("http://{push}"))
        .replace("http://127.0.0.1:9474", &format!("http://{maps}"));
    Ok(Fakes {
        gateway: gateway_pins,
        push: push_pins,
        config: Config::parse(&text)?,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_facts_come_from_the_services_on_loopback() -> TestResult {
    let Fakes {
        gateway,
        push,
        config,
    } = fakes().await?;
    let facts = checks::gather(&config, &Clients::new()?).await;
    assert_eq!(facts.gateway_pins, Ok(gateway));
    let relay = facts.push_relay.map_err(|why| why.clone())?;
    assert_eq!(relay.pins, push);
    assert!(relay.fcm);
    assert_eq!(facts.map_regions, Ok(2));
    // Nothing under .invalid resolves: every public check says it could not be tried, none
    // claims a pass or a failure it did not see.
    assert_eq!(facts.public.len(), 10);
    assert!(
        facts
            .public
            .iter()
            .all(|check| check.state == checks::State::Unknown),
        "{:?}",
        facts.public
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_page_is_served_on_loopback_with_a_working_link() -> TestResult {
    let Fakes {
        gateway,
        push,
        config,
    } = fakes().await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = Arc::new(App::new(config)?);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve(listener, app, async move {
        let _ = stopped.await;
    }));
    let client = reqwest::Client::new();
    let response = client.get(format!("http://{address}/owner")).send().await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let page: serde_json::Value = response.json().await?;
    let link = page
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("items")?.as_array())
        .flatten()
        .find(|item| item.get("kind").and_then(serde_json::Value::as_str) == Some("qr"))
        .and_then(|item| item.get("data")?.as_str())
        .ok_or("no QR code")?
        .to_owned();
    let payload = SetupPayload::from_link(&link)?;
    assert_eq!(payload.gateway_pins, gateway);
    assert_eq!(payload.push_relay_pins, push);
    assert_eq!(payload.server.as_str(), "https://pingers.example.invalid");
    // Only the page is served.
    let other = client.get(format!("http://{address}/")).send().await?;
    assert_eq!(other.status(), reqwest::StatusCode::NOT_FOUND);
    let _ = stop.send(());
    server.await??;
    Ok(())
}
