// SPDX-License-Identifier: AGPL-3.0-only
//! The TLS front, and the harness server forwarding map requests to a map host's gateway.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::net::{IpAddr, Ipv4Addr};

use axum::Router;
use axum::routing::get;
use pingers_ohttp::{BinaryRequest, GatewayKey, KeyConfig, encapsulate_request};

use super::super::{HarnessConfig, InProcessHarness, TlsFront};
use crate::seeded_rng;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A plain HTTP server answering `GET /ohttp-keys` with `keys`, on loopback.
async fn serve_keys(keys: Vec<u8>) -> Result<std::net::SocketAddr, std::io::Error> {
    let app = Router::new().route("/ohttp-keys", get(move || async move { keys }));
    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(address)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tls_front_serves_https_with_its_ca_until_dropped() -> TestResult {
    let target = serve_keys(b"behind the front".to_vec()).await?;
    let front = TlsFront::start(LOOPBACK, target).await?;
    let ca = reqwest::Certificate::from_pem(front.ca_pem().as_bytes())?;
    let client = reqwest::Client::builder()
        .tls_certs_only([ca])
        .http2_prior_knowledge()
        .build()?;
    let url = format!("https://{}/ohttp-keys", front.address());
    let body = client.get(&url).send().await?.bytes().await?;
    assert_eq!(body.as_ref(), b"behind the front");
    // A client that does not trust the front's CA is refused.
    let stranger = reqwest::Client::builder().http2_prior_knowledge().build()?;
    assert!(stranger.get(&url).send().await.is_err());
    let address = front.address();
    drop(front);
    // Once dropped, nothing listens there any more.
    let mut gone = false;
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(address).await.is_err() {
            gone = true;
            break;
        }
        tokio::task::yield_now().await;
        #[expect(
            clippy::disallowed_methods,
            reason = "waits for the front's task, on a real runtime, to see its stop signal"
        )]
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        gone,
        "the front still accepts connections after it was dropped"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_harness_server_forwards_map_requests_to_the_configured_gateway() -> TestResult {
    let keys = GatewayKey::generate(3, &mut seeded_rng(9))?
        .config()
        .encoded()
        .to_vec();
    let gateway = serve_keys(keys.clone()).await?;
    let harness = InProcessHarness::start(&HarnessConfig {
        map_host_gateway: Some(format!("http://{gateway}")),
        ..HarnessConfig::default()
    })?;
    let relay = harness.endpoints().ohttp_relay;
    let client = reqwest::Client::builder().http2_prior_knowledge().build()?;
    let server_keys = KeyConfig::decode(
        &client
            .get(format!("http://{relay}/v1/ohttp/config"))
            .send()
            .await?
            .bytes()
            .await?,
    )?;
    let sealed = encapsulate_request(
        &server_keys,
        &BinaryRequest {
            method: "GET".to_owned(),
            path: "/v1/maps/ohttp-keys".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        },
        &mut seeded_rng(10),
    )?;
    let answer = client
        .post(format!("http://{relay}/ohttp"))
        .header("content-type", "message/ohttp-req")
        .body(sealed.message)
        .send()
        .await?
        .bytes()
        .await?;
    let opened = sealed.response_context.open(&answer)?;
    assert_eq!(opened.status.get(), 200);
    assert_eq!(opened.body, keys);
    harness.shutdown();
    Ok(())
}
