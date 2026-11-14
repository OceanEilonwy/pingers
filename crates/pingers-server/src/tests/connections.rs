// SPDX-License-Identifier: AGPL-3.0-only
//! Slow and idle clients on both listeners: idle connections close on the injected timer, a
//! request in progress (a live channel) keeps its connection, each listener holds a bounded
//! number of connections, and request heads are bounded.

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Method, Request, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use pingers_ohttp::ChunkPosition;
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpStream;

use super::{
    Echo, LOOPBACK, Running, STEP, Setup, TestError, TestResult, configured, connect, open_live,
    read_all, run, send,
};
use crate::live::next_data;

/// Shorter than the public listener's overdue limit (three 30 s request timeouts), so an
/// idle close cannot be mistaken for an overdue one.
const IDLE: Duration = Duration::from_secs(60);

/// A server with the idle timeout [`IDLE`] and the given connection limits.
async fn limited(public: u32, gateway: u32) -> Result<Running, TestError> {
    let mut config = configured(Setup::default())?;
    config.idle_timeout = IDLE;
    config.max_connections = NonZeroU32::new(public).ok_or("limit")?;
    config.gateway_max_connections = NonZeroU32::new(gateway).ok_or("limit")?;
    run(config, Echo).await
}

/// Whether the peer has closed `stream`, waiting up to `wait` of real time for each read.
async fn closed_within(stream: &mut TcpStream, wait: Duration) -> bool {
    let mut buffer = [0_u8; 256];
    loop {
        match tokio::time::timeout(wait, stream.read(&mut buffer)).await {
            Err(_elapsed) => return false,
            Ok(Ok(0) | Err(_)) => return true,
            // The server's HTTP/2 preface or a GOAWAY: keep reading until the close.
            Ok(Ok(_)) => {}
        }
    }
}

/// Advances the server's clock by `step` until `stream` closes, up to `steps` times. The
/// server may take the connection's start time a little after the test has moved the clock,
/// so the steps go well past the idle timeout and the closing grace.
async fn closes_as_time_passes(
    server: &Running,
    stream: &mut TcpStream,
    step: Duration,
    steps: u32,
) -> bool {
    for _ in 0..steps {
        server.clock.advance(step);
        if closed_within(stream, Duration::from_millis(50)).await {
            return true;
        }
    }
    false
}

/// `GET /v1/params` on `client`.
async fn params(
    client: &mut hyper::client::conn::http2::SendRequest<Body>,
) -> Result<StatusCode, TestError> {
    let request = Request::builder()
        .method(Method::GET)
        .uri("http://server/v1/params")
        .body(Body::empty())?;
    let response = send(client, request).await?;
    let status = response.status();
    let _body = read_all(response).await?;
    Ok(status)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_that_sends_nothing_is_closed_after_the_idle_timeout() -> TestResult {
    let server = limited(8, 8).await?;
    for address in [server.address, server.gateway] {
        let mut silent = TcpStream::connect(address).await?;
        server
            .clock
            .advance(IDLE.saturating_sub(Duration::from_secs(1)));
        assert!(!closed_within(&mut silent, Duration::from_millis(200)).await);
        assert!(closes_as_time_passes(&server, &mut silent, Duration::from_secs(5), 100).await);
    }
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_connection_closes_but_a_live_channel_keeps_its_own_open() -> TestResult {
    let server = limited(8, 8).await?;
    // A client that made one request and then went quiet is closed.
    let mut client = connect(server.address, LOOPBACK).await?;
    assert_eq!(params(&mut client).await?, StatusCode::OK);
    // The request is over, so the idle timeout runs from its end: closed well before the
    // overdue limit (90 s) could have applied.
    server.clock.advance(IDLE);
    let mut closed = false;
    for _ in 0..20 {
        if tokio::time::timeout(Duration::from_millis(100), async {
            while !client.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok()
        {
            closed = true;
            break;
        }
        server.clock.advance(Duration::from_secs(1));
    }
    assert!(closed, "the idle connection stayed open");

    // A live channel is a request in progress for as long as it lasts.
    let (sender, mut live, mut body) = open_live(&server, "/v1/groups/00/live").await?;
    let mut decoder = live.response_decoder();
    for round in 0..3_u8 {
        server.clock.advance(IDLE);
        let frame = vec![round; 8];
        sender.send(Bytes::from(
            live.seal_chunk(&frame, ChunkPosition::NonFinal)?,
        ))?;
        let echoed = tokio::time::timeout(STEP, async {
            loop {
                let Some(bytes) = next_data(&mut body).await? else {
                    return Err::<Vec<u8>, TestError>("the channel ended".into());
                };
                for chunk in decoder.feed(&bytes)? {
                    if chunk.payload == frame {
                        return Ok(chunk.payload);
                    }
                }
            }
        })
        .await??;
        assert_eq!(echoed, frame);
    }
    server.stop().await
}

/// Connects to `address` and proves the connection is served with one request.
async fn served(
    address: SocketAddr,
) -> Result<hyper::client::conn::http2::SendRequest<Body>, TestError> {
    let mut client = connect(address, LOOPBACK).await?;
    assert_eq!(params(&mut client).await?, StatusCode::OK);
    Ok(client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_listener_holds_a_bounded_number_of_connections() -> TestResult {
    let server = limited(2, 1).await?;
    let first = served(server.address).await?;
    let _second = served(server.address).await?;
    let mut third = TcpStream::connect(server.address).await?;
    assert!(closed_within(&mut third, STEP).await);
    // The gateway's limit is its own.
    let gateway = connect(server.gateway, LOOPBACK).await?;
    let mut over = TcpStream::connect(server.gateway).await?;
    assert!(closed_within(&mut over, STEP).await);
    drop(gateway);
    // A closed connection frees its place.
    drop(first);
    let again = tokio::time::timeout(STEP, async {
        loop {
            if let Ok(client) = served(server.address).await {
                return client;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(again.is_ok(), "the freed place was not reused");
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_request_head_is_refused() -> TestResult {
    let server = limited(8, 8).await?;
    let mut client = connect(server.address, LOOPBACK).await?;
    let request = Request::builder()
        .method(Method::GET)
        .uri("http://server/v1/params")
        .header("x-padding", "x".repeat(32 * 1024))
        .body(Body::empty())?;
    let refused = match send(&mut client, request).await {
        Err(_) => true,
        Ok(response) => response.status() == StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
    };
    assert!(refused);
    // A head within the limit (a few KiB) is served on a fresh connection, its answer still
    // sized up front.
    let mut fresh = connect(server.address, LOOPBACK).await?;
    let request = Request::builder()
        .method(Method::GET)
        .uri("http://server/v1/params")
        .header("x-padding", "x".repeat(4 * 1024))
        .body(Body::empty())?;
    let response = send(&mut fresh, request).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .contains_key(axum::http::header::CONTENT_LENGTH)
    );
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_public_client_that_stops_reading_its_answer_is_dropped() -> TestResult {
    let server = limited(8, 8).await?;
    // A client that grants no flow-control credit: the answer's head arrives, its body never
    // can, and the request stays in progress.
    let stream = TcpStream::connect(server.address).await?;
    let (mut client, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .initial_stream_window_size(0)
        .handshake(TokioIo::new(stream))
        .await?;
    let driver = tokio::spawn(connection);
    let request = Request::builder()
        .method(Method::GET)
        .uri("http://server/v1/params")
        .body(Body::empty())?;
    let response = send(&mut client, request).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let limit = super::REQUEST_TIMEOUT.saturating_mul(crate::http::OVERDUE_FACTOR);
    server
        .clock
        .advance(limit.saturating_sub(Duration::from_secs(1)));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), async {
            while !client.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err(),
        "dropped before its time"
    );
    let dropped = tokio::time::timeout(STEP, async {
        loop {
            if client.is_closed() {
                return;
            }
            server.clock.advance(Duration::from_secs(1));
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(dropped.is_ok(), "the stalled connection stayed open");
    drop(response);
    driver.abort();
    server.stop().await
}
