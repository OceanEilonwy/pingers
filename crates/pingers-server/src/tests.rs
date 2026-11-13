// SPDX-License-Identifier: AGPL-3.0-only
//! The server over real HTTP/2 connections on loopback: the relay-only rule, routing by
//! channel, the gateway's refusals, timeouts on the injected timer, metrics, live channels
//! opened directly at the gateway, and shutdown.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes, HttpBody};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, Method, Request, Response, StatusCode};
use hyper::body::{Frame, Incoming};
use hyper::client::conn::http2::SendRequest;
use hyper_util::rt::{TokioExecutor, TokioIo};
use pingers_ohttp::{
    BinaryRequest, BinaryResponse, ChunkPosition, ChunkedClient, GatewayKey, KeyConfig,
    OpenedChunk, encapsulate_request,
};
use pingers_time::{Clock as _, ManualClock, UnixSeconds};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;
use tokio::net::TcpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::live::next_data;
use crate::{
    AddressList, CountsReport, DeliveryLive, LiveChannel, LiveFrame, LiveRefused, Logging, Metrics,
    OhttpKeys, Outside, RelayAllowList, RngSeed, ServerConfig, serve_with,
};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// How long any single network step may take before the test fails.
const STEP: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// Another loopback address, standing in for "not the relay" or "the relay".
const OTHER_LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));

fn rng(seed: u64) -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(seed)
}

/// Echoes every frame back: the channel's outbound stream is its inbound stream.
struct Echo;

impl LiveChannel for Echo {
    fn open(
        &self,
        _request: &BinaryRequest,
        inbound: mpsc::Receiver<LiveFrame>,
    ) -> Result<mpsc::Receiver<LiveFrame>, LiveRefused> {
        Ok(inbound)
    }
}

struct Running {
    address: SocketAddr,
    config: KeyConfig,
    clock: ManualClock,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Running {
    async fn stop(mut self) -> TestResult {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(STEP, &mut self.task).await??;
        Ok(())
    }
}

struct Setup {
    relay: IpAddr,
    metrics: Vec<IpAddr>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            relay: LOOPBACK,
            metrics: Vec::new(),
        }
    }
}

async fn start(setup: Setup, live: impl LiveChannel) -> Result<Running, TestError> {
    let key = GatewayKey::generate(1, &mut rng(1))?;
    let config = key.config();
    let server_config = ServerConfig {
        listen: SocketAddr::new(LOOPBACK, 0),
        logging: Logging::Off,
        request_timeout: REQUEST_TIMEOUT,
        ohttp_keys: OhttpKeys::new(vec![key])?,
        relays: RelayAllowList::new(vec![setup.relay])?,
        metrics: AddressList::new(setup.metrics),
    };
    let listener = tokio::net::TcpListener::bind(server_config.listen).await?;
    let address = listener.local_addr()?;
    let clock = ManualClock::new(UnixSeconds::new(1_800_000_000));
    let outside = Outside {
        clock: clock.clone(),
        timer: clock.clone(),
        seed: RngSeed::from_bytes(Zeroizing::new([9; 32])),
        live,
    };
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(serve_with(server_config, outside, listener, async move {
        let _ = stopped.await;
    }));
    Ok(Running {
        address,
        config,
        clock,
        stop: Some(stop),
        task,
    })
}

/// A body fed from a channel: each message is one DATA frame; dropping the sender ends it.
struct ChannelBody(mpsc::UnboundedReceiver<Bytes>);

impl HttpBody for ChannelBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.get_mut()
            .0
            .poll_recv(context)
            .map(|message| message.map(|bytes| Ok(Frame::data(bytes))))
    }
}

fn channel() -> (mpsc::UnboundedSender<Bytes>, Body) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (sender, Body::new(ChannelBody(receiver)))
}

/// An HTTP/2 connection to `server` from the loopback address `from`.
async fn connect(server: SocketAddr, from: IpAddr) -> Result<SendRequest<Body>, TestError> {
    let socket = TcpSocket::new_v4()?;
    socket.bind(SocketAddr::new(from, 0))?;
    let stream = socket.connect(server).await?;
    let (sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(sender)
}

fn request(
    method: Method,
    path: &str,
    content_type: Option<&str>,
    body: Body,
) -> Result<Request<Body>, TestError> {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("http://server{path}"));
    if let Some(content_type) = content_type {
        request = request.header(CONTENT_TYPE, content_type);
    }
    Ok(request.body(body)?)
}

async fn send(
    client: &mut SendRequest<Body>,
    request: Request<Body>,
) -> Result<Response<Incoming>, TestError> {
    Ok(tokio::time::timeout(STEP, client.send_request(request)).await??)
}

async fn read_all(response: Response<Incoming>) -> Result<Vec<u8>, TestError> {
    let mut body = Body::new(response.into_body());
    let mut all = Vec::new();
    while let Some(bytes) = tokio::time::timeout(STEP, next_data(&mut body)).await?? {
        all.extend_from_slice(&bytes);
    }
    Ok(all)
}

fn inner(method: &str, path: &str) -> BinaryRequest {
    BinaryRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

/// Sends `request` through OHTTP from `client` and returns the outer status and, on 200, the
/// opened inner response.
async fn oblivious(
    client: &mut SendRequest<Body>,
    config: &KeyConfig,
    inner_request: &BinaryRequest,
) -> Result<(StatusCode, Option<BinaryResponse>), TestError> {
    let encapsulated = encapsulate_request(config, inner_request, &mut rng(2))?;
    let response = send(
        client,
        request(
            Method::POST,
            "/ohttp",
            Some("message/ohttp-req"),
            Body::from(encapsulated.message),
        )?,
    )
    .await?;
    let status = response.status();
    if status != StatusCode::OK {
        return Ok((status, None));
    }
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .map(HeaderValue::as_bytes),
        Some(b"message/ohttp-res".as_slice())
    );
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .map(HeaderValue::as_bytes),
        Some(b"no-store".as_slice())
    );
    let sealed = read_all(response).await?;
    Ok((status, Some(encapsulated.response_context.open(&sealed)?)))
}

fn inner_status(response: Option<&BinaryResponse>) -> Option<u16> {
    response.map(|response| response.status.get())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_post_to_the_gateway_from_a_non_relay_is_refused() -> TestResult {
    let server = start(
        Setup {
            relay: OTHER_LOOPBACK,
            ..Setup::default()
        },
        Echo,
    )
    .await?;
    let config_request = inner("GET", "/v1/ohttp/config");

    let mut stranger = connect(server.address, LOOPBACK).await?;
    let (status, opened) = oblivious(&mut stranger, &server.config, &config_request).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(opened.is_none());
    let chunked = send(
        &mut stranger,
        request(
            Method::POST,
            "/ohttp",
            Some("message/ohttp-chunked-req"),
            Body::from(vec![1, 2, 3]),
        )?,
    )
    .await?;
    assert_eq!(chunked.status(), StatusCode::FORBIDDEN);

    let mut relay = connect(server.address, OTHER_LOOPBACK).await?;
    let (status, opened) = oblivious(&mut relay, &server.config, &config_request).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        opened.map(|response| response.body),
        Some(server.config.encoded().to_vec())
    );
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_requests_are_routed_by_their_privacy_class() -> TestResult {
    let server = start(Setup::default(), Echo).await?;
    let mut relay = connect(server.address, LOOPBACK).await?;
    let cases = [
        // Anonymous: belongs here, its crate is not wired yet.
        (inner("POST", "/v1/board/sync"), 501),
        // Identified: names the account, so it goes direct.
        (inner("POST", "/v1/accounts"), 421),
        // The live channel only runs chunked.
        (inner("POST", "/v1/groups/g1/live"), 421),
        (inner("GET", "/v1/nothing"), 404),
        (inner("PUT", "/v1/board/sync"), 405),
    ];
    for (request, expected) in cases {
        let (status, opened) = oblivious(&mut relay, &server.config, &request).await?;
        assert_eq!(status, StatusCode::OK, "{}", request.path);
        assert_eq!(
            inner_status(opened.as_ref()),
            Some(expected),
            "{}",
            request.path
        );
    }
    let (_, opened) = oblivious(
        &mut relay,
        &server.config,
        &inner("GET", "/v1/ohttp/config"),
    )
    .await?;
    let opened = opened.ok_or("no inner response")?;
    assert_eq!(opened.status.get(), 200);
    assert_eq!(opened.body, server.config.encoded());
    assert_eq!(
        opened
            .headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_slice()))
            .collect::<Vec<_>>(),
        [("content-type", b"application/ohttp-keys".as_slice())]
    );
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_requests_are_routed_by_their_privacy_class() -> TestResult {
    let server = start(Setup::default(), Echo).await?;
    let mut client = connect(server.address, LOOPBACK).await?;

    let config = send(
        &mut client,
        request(Method::GET, "/v1/ohttp/config", None, Body::empty())?,
    )
    .await?;
    assert_eq!(config.status(), StatusCode::OK);
    assert_eq!(
        config
            .headers()
            .get(CONTENT_TYPE)
            .map(HeaderValue::as_bytes),
        Some(b"application/ohttp-keys".as_slice())
    );
    assert_eq!(read_all(config).await?, server.config.encoded());

    let cases = [
        // Anonymous routes sent direct would reveal the caller's address.
        (
            Method::POST,
            "/v1/board/sync",
            StatusCode::MISDIRECTED_REQUEST,
        ),
        (
            Method::GET,
            "/v1/rln/leaves?after=0",
            StatusCode::MISDIRECTED_REQUEST,
        ),
        (
            Method::POST,
            "/v1/groups/g1/live",
            StatusCode::MISDIRECTED_REQUEST,
        ),
        // Identified: belongs here, its crate is not wired yet.
        (Method::POST, "/v1/accounts", StatusCode::NOT_IMPLEMENTED),
        (Method::GET, "/v1/params", StatusCode::NOT_IMPLEMENTED),
        (Method::GET, "/v1/nothing", StatusCode::NOT_FOUND),
        (Method::GET, "/ohttp", StatusCode::METHOD_NOT_ALLOWED),
        (
            Method::DELETE,
            "/v1/accounts",
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ];
    for (method, path, expected) in cases {
        let response = send(
            &mut client,
            request(method, path, None, Body::from(vec![1]))?,
        )
        .await?;
        assert_eq!(response.status(), expected, "{path}");
    }
    let oversized = send(
        &mut client,
        request(
            Method::POST,
            "/v1/accounts",
            None,
            Body::from(vec![0; 64 * 1024 + 1]),
        )?,
    )
    .await?;
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gateway_refuses_what_it_cannot_open() -> TestResult {
    let server = start(Setup::default(), Echo).await?;
    let mut relay = connect(server.address, LOOPBACK).await?;
    let one_shot = encapsulate_request(&server.config, &inner("GET", "/v1/params"), &mut rng(3))?;
    let mut largest = one_shot.message.clone();
    largest.resize(64 * 1024, 0);
    let mut too_large = largest.clone();
    too_large.push(0);
    let cases: [(Option<&str>, Vec<u8>, StatusCode); 6] = [
        (
            None,
            one_shot.message.clone(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            Some("text/plain"),
            one_shot.message,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            Some("message/ohttp-req"),
            vec![1, 0, 32],
            StatusCode::BAD_REQUEST,
        ),
        // At the limit it is read (and fails to open); one byte over, it is not read at all.
        (Some("message/ohttp-req"), largest, StatusCode::BAD_REQUEST),
        (
            Some("message/ohttp-req"),
            too_large,
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        (
            Some("message/ohttp-chunked-req"),
            vec![9; 64],
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (content_type, body, expected) in cases {
        let response = send(
            &mut relay,
            request(Method::POST, "/ohttp", content_type, Body::from(body))?,
        )
        .await?;
        assert_eq!(response.status(), expected, "{content_type:?}");
    }
    // A chunked body that ends before its request head is complete.
    let start = ChunkedClient::start(
        &server.config,
        &inner("POST", "/v1/groups/g1/live"),
        &mut rng(4),
    )?;
    let cut = start
        .preamble
        .get(..start.preamble.len().saturating_sub(1))
        .ok_or("short")?;
    let response = send(
        &mut relay,
        request(
            Method::POST,
            "/ohttp",
            Some("message/ohttp-chunked-req"),
            Body::from(cut.to_vec()),
        )?,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // A request head chunk announced as almost 16 KiB, and more bytes than the preamble limit
    // before it completes.
    let header = start.preamble.get(..39).ok_or("short")?;
    let filler = vec![0; 21 * 1024];
    let response = send(
        &mut relay,
        request(
            Method::POST,
            "/ohttp",
            Some("message/ohttp-chunked-req"),
            Body::from([header, &[0x7f, 0xff], &filler].concat()),
        )?,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    server.stop().await
}

/// Sends the request with a body that never ends, then advances the injected clock until the
/// server gives up.
async fn stalls_until_the_request_timeout(
    server: &Running,
    content_type: Option<&'static str>,
    path: &'static str,
) -> Result<StatusCode, TestError> {
    let mut client = connect(server.address, LOOPBACK).await?;
    let (sender, body) = channel();
    sender.send(Bytes::from_static(&[1, 2, 3]))?;
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("http://server{path}"));
    if let Some(content_type) = content_type {
        request = request.header(CONTENT_TYPE, content_type);
    }
    let mut pending = tokio::spawn(client.send_request(request.body(body)?));
    // Just short of the timeout, nothing happens.
    server
        .clock
        .advance(REQUEST_TIMEOUT.saturating_sub(Duration::from_secs(1)));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut pending)
            .await
            .is_err()
    );
    for _ in 0..100 {
        server.clock.advance(REQUEST_TIMEOUT);
        if let Ok(response) = tokio::time::timeout(Duration::from_millis(50), &mut pending).await {
            drop(sender);
            return Ok(response??.status());
        }
    }
    Err("the server never timed out".into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_request_body_times_out_on_the_injected_timer() -> TestResult {
    let server = start(Setup::default(), Echo).await?;
    for (content_type, path) in [
        (Some("message/ohttp-req"), "/ohttp"),
        (Some("message/ohttp-chunked-req"), "/ohttp"),
        (None, "/v1/accounts"),
    ] {
        let status = stalls_until_the_request_timeout(&server, content_type, path).await?;
        assert_eq!(
            status,
            StatusCode::REQUEST_TIMEOUT,
            "{content_type:?} {path}"
        );
    }
    server.stop().await
}

/// Opens a chunked exchange to `path` directly at the gateway; returns the frame sender, the
/// client half and the response body.
async fn open_live(
    server: &Running,
    path: &str,
) -> Result<(mpsc::UnboundedSender<Bytes>, ChunkedClient, Body), TestError> {
    let mut relay = connect(server.address, LOOPBACK).await?;
    let start = ChunkedClient::start(&server.config, &inner("POST", path), &mut rng(5))?;
    let (sender, body) = channel();
    sender.send(Bytes::from(start.preamble))?;
    let response = send(
        &mut relay,
        request(
            Method::POST,
            "/ohttp",
            Some("message/ohttp-chunked-req"),
            body,
        )?,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .map(HeaderValue::as_bytes),
        Some(b"message/ohttp-chunked-res".as_slice())
    );
    Ok((sender, start.client, Body::new(response.into_body())))
}

/// Reads the rest of a response and checks it is only the final, empty chunk.
async fn ends_with_nothing_but_the_final_chunk(
    client: &ChunkedClient,
    mut body: Body,
) -> TestResult {
    let mut decoder = client.response_decoder();
    while let Some(bytes) = tokio::time::timeout(STEP, next_data(&mut body)).await?? {
        assert!(decoder.feed(&bytes)?.is_empty());
    }
    assert_eq!(
        decoder.finish()?,
        Some(OpenedChunk {
            payload: Vec::new(),
            position: ChunkPosition::Final,
        })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_channels_echo_frame_by_frame_and_end_with_the_member() -> TestResult {
    let server = start(Setup::default(), Echo).await?;
    let (sender, mut client, mut body) = open_live(&server, "/v1/groups/g1/live?sub=s1").await?;
    let mut decoder = client.response_decoder();
    for round in 1_u8..=3 {
        sender.send(Bytes::from(
            client.seal_chunk(&[round; 40], ChunkPosition::NonFinal)?,
        ))?;
        let mut opened = Vec::new();
        while opened.is_empty() {
            let bytes = tokio::time::timeout(STEP, next_data(&mut body))
                .await??
                .ok_or("ended early")?;
            opened = decoder.feed(&bytes)?;
        }
        assert_eq!(
            opened,
            [OpenedChunk {
                payload: vec![round; 40],
                position: ChunkPosition::NonFinal,
            }]
        );
    }
    // The member's last frame comes back, then the exchange ends.
    sender.send(Bytes::from(
        client.seal_chunk(b"bye", ChunkPosition::Final)?,
    ))?;
    drop(sender);
    let mut rest = Vec::new();
    while let Some(bytes) = tokio::time::timeout(STEP, next_data(&mut body)).await?? {
        rest.extend(decoder.feed(&bytes)?);
    }
    assert_eq!(
        rest,
        [OpenedChunk {
            payload: b"bye".to_vec(),
            position: ChunkPosition::NonFinal,
        }]
    );
    assert_eq!(
        decoder.finish()?,
        Some(OpenedChunk {
            payload: Vec::new(),
            position: ChunkPosition::Final,
        })
    );
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_channel_that_cannot_open_ends_at_once() -> TestResult {
    // The production channel, until pingers-delivery lands.
    let server = start(Setup::default(), DeliveryLive).await?;
    let (_sender, client, body) = open_live(&server, "/v1/groups/g1/live").await?;
    ends_with_nothing_but_the_final_chunk(&client, body).await?;
    // A chunked exchange naming anything but the live channel, likewise.
    for path in ["/v1/board/sync", "/v1/nothing"] {
        let (_sender, client, body) = open_live(&server, path).await?;
        ends_with_nothing_but_the_final_chunk(&client, body).await?;
    }
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_count_outcomes_for_allowed_addresses_only() -> TestResult {
    let server = start(
        Setup {
            metrics: vec![OTHER_LOOPBACK],
            ..Setup::default()
        },
        DeliveryLive,
    )
    .await?;
    let mut client = connect(server.address, LOOPBACK).await?;
    send(
        &mut client,
        request(Method::GET, "/v1/ohttp/config", None, Body::empty())?,
    )
    .await?;
    send(
        &mut client,
        request(Method::POST, "/v1/board/sync", None, Body::empty())?,
    )
    .await?;
    oblivious(
        &mut client,
        &server.config,
        &inner("POST", "/v1/board/sync"),
    )
    .await?;
    let (_sender, live_client, body) = open_live(&server, "/v1/groups/g1/live").await?;
    ends_with_nothing_but_the_final_chunk(&live_client, body).await?;
    let (_sender, live_client, body) = open_live(&server, "/v1/board/sync").await?;
    ends_with_nothing_but_the_final_chunk(&live_client, body).await?;

    let hidden = send(
        &mut client,
        request(Method::GET, "/metrics", None, Body::empty())?,
    )
    .await?;
    assert_eq!(hidden.status(), StatusCode::NOT_FOUND);

    let mut operator = connect(server.address, OTHER_LOOPBACK).await?;
    let metrics = send(
        &mut operator,
        request(Method::GET, "/metrics", None, Body::empty())?,
    )
    .await?;
    assert_eq!(metrics.status(), StatusCode::OK);
    let text = String::from_utf8(read_all(metrics).await?)?;
    for expected in [
        "{channel=\"direct\",outcome=\"ok\"} 1",
        "{channel=\"direct\",outcome=\"client_error\"} 1",
        "{channel=\"ohttp\",outcome=\"not_implemented\"} 1",
        "{channel=\"ohttp_chunked\",outcome=\"not_implemented\"} 1",
        "{channel=\"ohttp_chunked\",outcome=\"client_error\"} 1",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_ends_open_live_channels() -> TestResult {
    let server = start(Setup::default(), Echo).await?;
    let (_sender, _client, _body) = open_live(&server, "/v1/groups/g1/live").await?;
    // The member's stream is still open; stopping must not wait for it.
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_counts_report_writes_totals_once_per_interval() -> TestResult {
    let clock = ManualClock::new(UnixSeconds::new(1_000));
    let metrics = Metrics::default();
    metrics.record(crate::route::Channel::Direct, StatusCode::OK);
    let written = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&written);
    let interval = Duration::from_secs(60);
    let report = CountsReport {
        metrics: &metrics,
        clock: &clock,
        timer: &clock,
        interval,
    };
    let run = report.run(move |text| {
        if let Ok(mut written) = sink.lock() {
            written.push(text.to_owned());
        }
    });
    let mut run = std::pin::pin!(run);
    let count = || written.lock().map_or(0, |written| written.len());
    // Nothing before the first interval has passed.
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut run)
            .await
            .is_err()
    );
    clock.advance(interval.saturating_sub(Duration::from_secs(1)));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut run)
            .await
            .is_err()
    );
    assert_eq!(count(), 0);
    clock.advance(Duration::from_secs(1));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut run)
            .await
            .is_err()
    );
    assert_eq!(count(), 1);
    clock.advance(interval);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut run)
            .await
            .is_err()
    );
    let written = written.lock().map_err(|_| "lock")?.clone();
    assert_eq!(written.len(), 2);
    let first = written.first().ok_or("no report")?;
    assert!(
        first.starts_with("pingers-server counts at 1060\n# TYPE"),
        "{first}"
    );
    assert!(
        first.contains("{channel=\"direct\",outcome=\"ok\"} 1"),
        "{first}"
    );
    assert_eq!(clock.now().get(), 1_120);
    Ok(())
}

#[test]
fn live_frames_print_only_their_length() {
    assert_eq!(
        format!("{:?}", LiveFrame::new(vec![0xab; 5])),
        "LiveFrame { length: 5 }"
    );
}
