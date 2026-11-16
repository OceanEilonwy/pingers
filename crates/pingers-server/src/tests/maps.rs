// SPDX-License-Identifier: AGPL-3.0-only
//! Map downloads through the relay: the server forwards the map host's sealed requests to the
//! configured gateway as they are, only through OHTTP, and to nowhere else.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Router;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::routing::{get, post};
use pingers_ohttp::{BinaryRequest, GatewayKey};

use super::{
    Echo, LOOPBACK, Setup, TestError, TestResult, configured, connect, inner, inner_status,
    oblivious, read_all, request, rng, run, send,
};
use crate::MapHostGateway;

/// What the fake map host gateway received.
type Seen = Arc<Mutex<Vec<Vec<u8>>>>;

/// A stand-in for the map host's gateway: echoes each sealed request back reversed, and
/// serves a real key config.
async fn fake_gateway() -> Result<(SocketAddr, Seen, Vec<u8>), TestError> {
    let seen: Seen = Arc::default();
    let keys = GatewayKey::generate(1, &mut rng(5))?
        .config()
        .encoded()
        .to_vec();
    let served = keys.clone();
    let recorded = Arc::clone(&seen);
    let app = Router::new()
        .route(
            "/ohttp",
            post(move |body: Bytes| async move {
                recorded
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(body.to_vec());
                let mut answer = body.to_vec();
                answer.reverse();
                answer
            }),
        )
        .route("/ohttp-keys", get(move || async move { served }));
    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((address, seen, keys))
}

fn map_request(body: &[u8]) -> BinaryRequest {
    BinaryRequest {
        body: body.to_vec(),
        ..inner("POST", "/v1/maps/ohttp")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn map_requests_are_forwarded_sealed_to_the_map_host_gateway() -> TestResult {
    let (gateway, seen, keys) = fake_gateway().await?;
    let mut config = configured(Setup::default())?;
    config.map_host_gateway = MapHostGateway::from_settings(
        Some("https://maps.example"),
        Some(format!("http://{gateway}")),
    )?;
    let server = run(config, Echo).await?;
    let mut relay = connect(server.gateway, LOOPBACK).await?;

    let (status, answer) = oblivious(&mut relay, &server.config, &map_request(b"sealed")).await?;
    assert_eq!(status, StatusCode::OK);
    let answer = answer.ok_or("no answer")?;
    assert_eq!(answer.status.get(), 200);
    assert_eq!(answer.body, b"delaes");
    assert_eq!(
        answer
            .headers
            .iter()
            .find(|header| header.name == "content-type")
            .map(|header| header.value.as_slice()),
        Some(b"message/ohttp-res".as_slice())
    );
    assert_eq!(
        *seen.lock().unwrap_or_else(PoisonError::into_inner),
        vec![b"sealed".to_vec()]
    );

    let (_, key_answer) = oblivious(
        &mut relay,
        &server.config,
        &inner("GET", "/v1/maps/ohttp-keys"),
    )
    .await?;
    let key_answer = key_answer.ok_or("no key answer")?;
    assert_eq!(key_answer.status.get(), 200);
    assert_eq!(key_answer.body, keys);

    // An empty body is no request.
    let (_, empty) = oblivious(&mut relay, &server.config, &map_request(b"")).await?;
    assert_eq!(inner_status(empty.as_ref()), Some(400));

    // Sent direct, it would show the server the device's address, and make the server an
    // open proxy: refused.
    let mut direct = connect(server.address, LOOPBACK).await?;
    let response = send(
        &mut direct,
        request(
            axum::http::Method::GET,
            "/v1/maps/ohttp-keys",
            None,
            axum::body::Body::empty(),
        )?,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    let _ = read_all(response).await?;
    assert_eq!(seen.lock().unwrap_or_else(PoisonError::into_inner).len(), 1);
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_reachable_map_host_nothing_is_forwarded() -> TestResult {
    // No map host: nothing to forward to.
    let server = run(configured(Setup::default())?, Echo).await?;
    let mut relay = connect(server.gateway, LOOPBACK).await?;
    let (_, none) = oblivious(&mut relay, &server.config, &map_request(b"x")).await?;
    assert_eq!(inner_status(none.as_ref()), Some(404));
    let (_, keys) = oblivious(
        &mut relay,
        &server.config,
        &inner("GET", "/v1/maps/ohttp-keys"),
    )
    .await?;
    assert_eq!(inner_status(keys.as_ref()), Some(404));
    server.stop().await?;

    // A map host that does not answer, or answers with something that is not a key config.
    let closed = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let address = closed.local_addr()?;
    drop(closed);
    let mut config = configured(Setup::default())?;
    config.map_host_gateway = MapHostGateway::from_settings(
        Some("https://maps.example"),
        Some(format!("http://{address}")),
    )?;
    let server = run(config, Echo).await?;
    let mut relay = connect(server.gateway, LOOPBACK).await?;
    let (_, down) = oblivious(&mut relay, &server.config, &map_request(b"x")).await?;
    assert_eq!(inner_status(down.as_ref()), Some(502));
    let (_, keys) = oblivious(
        &mut relay,
        &server.config,
        &inner("GET", "/v1/maps/ohttp-keys"),
    )
    .await?;
    assert_eq!(inner_status(keys.as_ref()), Some(502));
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gateway_answering_garbage_keys_is_not_passed_on() -> TestResult {
    let app = Router::new().route("/ohttp-keys", get(|| async { "not a key config" }));
    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let mut config = configured(Setup::default())?;
    config.map_host_gateway = MapHostGateway::from_settings(
        Some("https://maps.example"),
        Some(format!("http://{address}")),
    )?;
    let server = run(config, Echo).await?;
    let mut relay = connect(server.gateway, LOOPBACK).await?;
    let (_, keys) = oblivious(
        &mut relay,
        &server.config,
        &inner("GET", "/v1/maps/ohttp-keys"),
    )
    .await?;
    assert_eq!(inner_status(keys.as_ref()), Some(502));
    // A gateway answering anything but 200 to a request is unreachable too.
    let (_, refused) = oblivious(&mut relay, &server.config, &map_request(b"x")).await?;
    assert_eq!(inner_status(refused.as_ref()), Some(502));
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answers_up_to_the_map_hosts_largest_are_forwarded() -> TestResult {
    // The fake gateway answers with as many bytes as the request's body names.
    let app = Router::new().route(
        "/ohttp",
        post(|body: Bytes| async move {
            let length = std::str::from_utf8(&body)
                .ok()
                .and_then(|text| text.parse::<usize>().ok())
                .unwrap_or(0);
            vec![7_u8; length]
        }),
    );
    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let mut config = configured(Setup::default())?;
    config.map_host_gateway = MapHostGateway::from_settings(
        Some("https://maps.example"),
        Some(format!("http://{address}")),
    )?;
    let server = run(config, Echo).await?;
    let mut relay = connect(server.gateway, LOOPBACK).await?;
    // 1 MiB and 4 KiB: the map host's largest padded answer and its encapsulation, with room.
    let (_, largest) = oblivious(&mut relay, &server.config, &map_request(b"1052672")).await?;
    let largest = largest.ok_or("no answer")?;
    assert_eq!(largest.status.get(), 200);
    assert_eq!(largest.body.len(), 1_052_672);
    let (_, over) = oblivious(&mut relay, &server.config, &map_request(b"1052673")).await?;
    assert_eq!(inner_status(over.as_ref()), Some(502));
    server.stop().await
}

/// The key config route needs no credential, so it does not reach the map host each time: the
/// config is kept for `KEY_CACHE_TTL` and fetched again after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_map_host_key_config_is_cached() -> TestResult {
    let fetches = Arc::new(Mutex::new(0_usize));
    let keys = GatewayKey::generate(1, &mut rng(6))?
        .config()
        .encoded()
        .to_vec();
    let counted = Arc::clone(&fetches);
    let app = Router::new().route(
        "/ohttp-keys",
        get(move || {
            *counted.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            let served = keys.clone();
            async move { served }
        }),
    );
    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let mut config = configured(Setup::default())?;
    config.map_host_gateway = MapHostGateway::from_settings(
        Some("https://maps.example"),
        Some(format!("http://{address}")),
    )?;
    let server = run(config, Echo).await?;
    let ask = inner("GET", "/v1/maps/ohttp-keys");
    let count = || *fetches.lock().unwrap_or_else(PoisonError::into_inner);
    for _ in 0..3 {
        let mut relay = connect(server.gateway, LOOPBACK).await?;
        let (_, answer) = oblivious(&mut relay, &server.config, &ask).await?;
        assert_eq!(inner_status(answer.as_ref()), Some(200));
    }
    assert_eq!(count(), 1);
    server.clock.advance(crate::map_relay::KEY_CACHE_TTL);
    let mut relay = connect(server.gateway, LOOPBACK).await?;
    let (_, answer) = oblivious(&mut relay, &server.config, &ask).await?;
    assert_eq!(inner_status(answer.as_ref()), Some(200));
    assert_eq!(count(), 2);
    server.stop().await
}
