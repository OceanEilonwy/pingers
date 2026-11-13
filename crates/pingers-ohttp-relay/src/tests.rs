// SPDX-License-Identifier: AGPL-3.0-only
//! In-process end-to-end tests: a pingers-ohttp client, the relay, and a test gateway that
//! decapsulates with pingers-ohttp, all over real HTTP/2 connections on loopback.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::convert::Infallible;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::body::{Body, Bytes, Frame, Incoming};
use hyper::client::conn::http2::SendRequest;
use hyper::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use pingers_ohttp::{
    BinaryRequest, BinaryResponse, ChunkPosition, ChunkedClient, ChunkedClientStart,
    ChunkedGateway, Gateway, GatewayKey, Header, KeyConfig, OhttpError, OpenedChunk,
    ResponseChunkDecoder, encapsulate_request,
};
use pingers_testkit::seeded_rng;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::config::Config;
use crate::relay::{Relay, serve};

/// How long any single step may take. A relay that buffered a body would stall a step forever.
const STEP: Duration = Duration::from_secs(10);

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read through Debug when a test fails"
)]
enum TestError {
    Ohttp(OhttpError),
    Io(std::io::Error),
    Hyper(hyper::Error),
    Http(hyper::http::Error),
    Timeout,
    Setup(&'static str),
}

impl From<OhttpError> for TestError {
    fn from(error: OhttpError) -> Self {
        Self::Ohttp(error)
    }
}
impl From<std::io::Error> for TestError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<hyper::Error> for TestError {
    fn from(error: hyper::Error) -> Self {
        Self::Hyper(error)
    }
}
impl From<hyper::http::Error> for TestError {
    fn from(error: hyper::http::Error) -> Self {
        Self::Http(error)
    }
}
impl From<tokio::time::error::Elapsed> for TestError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Self::Timeout
    }
}

/// A body fed from a channel: each message is one DATA frame; dropping the sender ends it.
struct ChannelBody(mpsc::UnboundedReceiver<Bytes>);

impl Body for ChannelBody {
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

fn channel() -> (mpsc::UnboundedSender<Bytes>, ChannelBody) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (sender, ChannelBody(receiver))
}

fn send(sender: &mpsc::UnboundedSender<Bytes>, bytes: Vec<u8>) -> Result<(), TestError> {
    sender
        .send(Bytes::from(bytes))
        .map_err(|_| TestError::Setup("body receiver gone"))
}

/// The next DATA frame's bytes, or `None` at the end of the body.
async fn next_data(body: &mut Incoming) -> Result<Option<Bytes>, TestError> {
    loop {
        let frame = tokio::time::timeout(
            STEP,
            poll_fn(|context| Pin::new(&mut *body).poll_frame(context)),
        )
        .await?;
        match frame {
            None => return Ok(None),
            Some(frame) => {
                if let Ok(data) = frame?.into_data() {
                    return Ok(Some(data));
                }
            }
        }
    }
}

async fn read_all(body: &mut Incoming) -> Result<Vec<u8>, TestError> {
    let mut all = Vec::new();
    while let Some(data) = next_data(body).await? {
        all.extend_from_slice(&data);
    }
    Ok(all)
}

/// The test gateway: decapsulates one-shot requests and echoes chunked ones, chunk by chunk,
/// as soon as each request chunk arrives. It records the headers it receives.
struct TestGateway {
    gateway: Gateway,
    config: KeyConfig,
    received_headers: Mutex<Vec<HeaderMap>>,
    echo_tasks: Mutex<JoinSet<()>>,
}

fn response_with(
    content_type: &'static str,
    body: ChannelBody,
) -> Result<Response<ChannelBody>, TestError> {
    Ok(Response::builder()
        .header(CONTENT_TYPE, content_type)
        // Must not reach the client.
        .header("x-gateway-internal", "secret")
        .body(body)?)
}

impl TestGateway {
    async fn handle(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<ChannelBody>, TestError> {
        if let Ok(mut headers) = self.received_headers.lock() {
            headers.push(request.headers().clone());
        }
        let content_type = request.headers().get(CONTENT_TYPE).cloned();
        let mut body = request.into_body();
        let (sender, response_body) = channel();
        match content_type.as_ref().and_then(|value| value.to_str().ok()) {
            Some("message/ohttp-req") => {
                let message = read_all(&mut body).await?;
                let decapsulated = self.gateway.decapsulate(&message)?;
                let reply = BinaryResponse {
                    status: pingers_ohttp::StatusCode::new(200)?,
                    headers: Vec::new(),
                    body: [b"seen ".as_slice(), decapsulated.request.path.as_bytes()].concat(),
                };
                send(
                    &sender,
                    decapsulated.responder.seal(&reply, &mut seeded_rng(100))?,
                )?;
                response_with("message/ohttp-res", response_body)
            }
            Some("message/ohttp-chunked-req") => {
                let mut rng = seeded_rng(101);
                let mut received = Vec::new();
                let mut accepted = loop {
                    let data = next_data(&mut body)
                        .await?
                        .ok_or(TestError::Setup("request ended before its head"))?;
                    received.extend_from_slice(&data);
                    match ChunkedGateway::accept(&self.gateway, &received, &mut rng) {
                        Ok(accepted) => break accepted,
                        Err(OhttpError::Truncated) => {}
                        Err(other) => return Err(other.into()),
                    }
                };
                send(&sender, accepted.response_preamble())?;
                if let Ok(mut tasks) = self.echo_tasks.lock() {
                    tasks.spawn(async move {
                        let _ = echo(&mut accepted, &mut body, &sender).await;
                    });
                }
                response_with("message/ohttp-chunked-res", response_body)
            }
            _ => {
                send(&sender, self.config.encoded().to_vec())?;
                response_with("application/ohttp-keys", response_body)
            }
        }
    }
}

/// Echoes each request chunk back as a response chunk the moment it opens.
async fn echo(
    accepted: &mut ChunkedGateway,
    body: &mut Incoming,
    sender: &mpsc::UnboundedSender<Bytes>,
) -> Result<(), TestError> {
    let mut decoder = accepted.request_decoder();
    let mut pending = decoder.feed(&[])?;
    loop {
        for chunk in pending.drain(..) {
            let reply = [b"echo ".as_slice(), &chunk.payload].concat();
            send(
                sender,
                accepted.seal_chunk(&reply, ChunkPosition::NonFinal)?,
            )?;
        }
        match next_data(body).await? {
            Some(data) => pending = decoder.feed(&data)?,
            None => break,
        }
    }
    let last = decoder
        .finish()?
        .ok_or(TestError::Setup("no final chunk"))?;
    let reply = [b"done ".as_slice(), &last.payload].concat();
    send(sender, accepted.seal_chunk(&reply, ChunkPosition::Final)?)?;
    Ok(())
}

/// Serves `handler` over HTTP/2 on a fresh loopback port until the returned set is dropped.
async fn listen<F, Fut, B>(handler: F) -> Result<(SocketAddr, JoinSet<()>), TestError>
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Response<B>, Infallible>> + Send + 'static,
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let mut connections = JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            let handler = handler.clone();
            connections.spawn(async move {
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), hyper::service::service_fn(handler))
                    .await;
            });
        }
    });
    Ok((address, tasks))
}

struct World {
    relay_address: SocketAddr,
    gateway: Arc<TestGateway>,
    config: KeyConfig,
    _tasks: Vec<JoinSet<()>>,
}

async fn world() -> Result<World, TestError> {
    let key = GatewayKey::generate(1, &mut seeded_rng(1))?;
    let config = key.config();
    let gateway = Arc::new(TestGateway {
        gateway: Gateway::new(vec![key]),
        config: config.clone(),
        received_headers: Mutex::new(Vec::new()),
        echo_tasks: Mutex::new(JoinSet::new()),
    });
    let shared = Arc::clone(&gateway);
    let (gateway_address, gateway_tasks) = listen(move |request| {
        let shared = Arc::clone(&shared);
        async move {
            // A gateway failure is a 500, so tests see it rather than a reset.
            Ok::<_, Infallible>(shared.handle(request).await.unwrap_or_else(|_| {
                let (_, body) = channel();
                let mut response = Response::new(body);
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                response
            }))
        }
    })
    .await?;

    let relay_config = Config::parse(&format!(
        "listen = \"127.0.0.1:0\"\n\
         gateway = \"http://{gateway_address}/ohttp\"\n\
         gateway_key_config = \"http://{gateway_address}/v1/ohttp/config\"\n\
         response_timeout_seconds = 5\n"
    ))
    .map_err(|_| TestError::Setup("relay config"))?;
    let listener = TcpListener::bind(relay_config.listen).await?;
    let relay_address = listener.local_addr()?;
    let relay = Arc::new(Relay::new(relay_config));
    let mut relay_tasks = JoinSet::new();
    relay_tasks.spawn(async move {
        let _ = serve(listener, relay, std::future::pending()).await;
    });
    Ok(World {
        relay_address,
        gateway,
        config,
        _tasks: vec![gateway_tasks, relay_tasks],
    })
}

async fn connect(
    address: SocketAddr,
) -> Result<(SendRequest<ChannelBody>, JoinSet<()>), TestError> {
    let stream = TcpStream::connect(address).await?;
    let (sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream)).await?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let _ = connection.await;
    });
    Ok((sender, tasks))
}

fn post(
    path: &str,
    content_type: &str,
    body: ChannelBody,
) -> Result<Request<ChannelBody>, TestError> {
    Ok(Request::builder()
        .method(Method::POST)
        .uri(format!("http://relay{path}"))
        .header(CONTENT_TYPE, content_type)
        .header("user-agent", "pingers-test/1.0")
        .header("cookie", "session=identifying")
        .header("x-forwarded-for", "192.0.2.7")
        .header("forwarded", "for=192.0.2.7")
        .header("authorization", "Bearer outer-credential")
        .body(body)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_shot_request_goes_through_with_only_body_and_media_type() -> Result<(), TestError> {
    let world = world().await?;
    let (mut client, _connection) = connect(world.relay_address).await?;
    let request = BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/board/sync".to_owned(),
        headers: vec![Header {
            name: "authorization".to_owned(),
            value: b"inner-token".to_vec(),
        }],
        body: vec![1, 2, 3],
    };
    let encapsulated = encapsulate_request(&world.config, &request, &mut seeded_rng(2))?;
    let (sender, body) = channel();
    send(&sender, encapsulated.message)?;
    drop(sender);
    let response = tokio::time::timeout(
        STEP,
        client.send_request(post("/ohttp", "message/ohttp-req", body)?),
    )
    .await??;
    assert_eq!(response.status(), StatusCode::OK);
    let mut names: Vec<&str> = response
        .headers()
        .keys()
        .map(hyper::header::HeaderName::as_str)
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["cache-control", "content-type"]);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&HeaderValue::from_static("message/ohttp-res"))
    );
    let mut body = response.into_body();
    let opened = encapsulated
        .response_context
        .open(&read_all(&mut body).await?)?;
    assert_eq!(opened.body, b"seen /v1/board/sync");

    let received = world
        .gateway
        .received_headers
        .lock()
        .map_err(|_| TestError::Setup("lock"))?
        .clone();
    assert_eq!(received.len(), 1);
    let names: Vec<&str> = received
        .iter()
        .flat_map(|headers| headers.keys().map(hyper::header::HeaderName::as_str))
        .collect();
    assert_eq!(names, ["content-type"]);
    Ok(())
}

async fn next_chunks(
    body: &mut Incoming,
    decoder: &mut ResponseChunkDecoder,
) -> Result<Vec<OpenedChunk>, TestError> {
    loop {
        let data = next_data(body)
            .await?
            .ok_or(TestError::Setup("response ended early"))?;
        let chunks = decoder.feed(&data)?;
        if !chunks.is_empty() {
            return Ok(chunks);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_exchange_is_full_duplex_through_the_relay() -> Result<(), TestError> {
    let world = world().await?;
    let (mut client, _connection) = connect(world.relay_address).await?;
    let live = BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/groups/0011/live".to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let ChunkedClientStart {
        preamble,
        client: mut chunked,
    } = ChunkedClient::start(&world.config, &live, &mut seeded_rng(3))?;
    let (sender, body) = channel();
    send(&sender, preamble)?;
    // The response headers arrive while the request body is still open.
    let response = tokio::time::timeout(
        STEP,
        client.send_request(post("/ohttp", "message/ohttp-chunked-req", body)?),
    )
    .await??;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&HeaderValue::from_static("message/ohttp-chunked-res"))
    );
    let mut downlink = response.into_body();
    let mut decoder = chunked.response_decoder();

    // Each round sends one chunk and waits for its echo before sending the next: this only
    // completes if the relay forwards both bodies incrementally and concurrently.
    for round in 0_u8..20 {
        let frame = vec![round; 256];
        send(
            &sender,
            chunked.seal_chunk(&frame, ChunkPosition::NonFinal)?,
        )?;
        let echoed = next_chunks(&mut downlink, &mut decoder).await?;
        let expected = [b"echo ".as_slice(), &frame].concat();
        assert_eq!(
            echoed,
            [OpenedChunk {
                payload: expected,
                position: ChunkPosition::NonFinal,
            }],
            "round {round}"
        );
    }
    // Two chunks in one write come back as two echoes.
    let mut both = chunked.seal_chunk(b"a", ChunkPosition::NonFinal)?;
    both.extend(chunked.seal_chunk(b"b", ChunkPosition::NonFinal)?);
    send(&sender, both)?;
    let mut echoes = next_chunks(&mut downlink, &mut decoder).await?;
    if echoes.len() < 2 {
        echoes.extend(next_chunks(&mut downlink, &mut decoder).await?);
    }
    let payloads: Vec<&[u8]> = echoes
        .iter()
        .map(|chunk| chunk.payload.as_slice())
        .collect();
    assert_eq!(payloads, [b"echo a".as_slice(), b"echo b"]);

    send(&sender, chunked.seal_chunk(b"bye", ChunkPosition::Final)?)?;
    drop(sender);
    while let Some(data) = next_data(&mut downlink).await? {
        assert!(decoder.feed(&data)?.is_empty());
    }
    assert_eq!(
        decoder.finish()?,
        Some(OpenedChunk {
            payload: b"done bye".to_vec(),
            position: ChunkPosition::Final,
        })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_config_route_forwards_to_the_configured_url() -> Result<(), TestError> {
    let world = world().await?;
    let (mut client, _connection) = connect(world.relay_address).await?;
    let (_, body) = channel();
    let request = Request::builder()
        .method(Method::GET)
        .uri("http://relay/v1/ohttp/config")
        .body(body)?;
    let response = tokio::time::timeout(STEP, client.send_request(request)).await??;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/ohttp-keys"))
    );
    let mut body = response.into_body();
    let fetched = KeyConfig::decode(&read_all(&mut body).await?)?;
    pingers_ohttp::check_pin(&fetched, &world.config.hash())?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anything_else_is_refused_without_reaching_the_gateway() -> Result<(), TestError> {
    let world = world().await?;
    let (mut client, _connection) = connect(world.relay_address).await?;
    let cases = [
        (
            Method::POST,
            "/ohttp",
            Some("application/json"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            Method::POST,
            "/ohttp",
            None,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (Method::GET, "/ohttp", None, StatusCode::METHOD_NOT_ALLOWED),
        (
            Method::POST,
            "/v1/ohttp/config",
            None,
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            Method::POST,
            "/v1/board/sync",
            Some("message/ohttp-req"),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (method, path, content_type, expected) in cases {
        let (_, body) = channel();
        let mut request = Request::builder()
            .method(method)
            .uri(format!("http://relay{path}"));
        if let Some(content_type) = content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }
        let response =
            tokio::time::timeout(STEP, client.send_request(request.body(body)?)).await??;
        assert_eq!(response.status(), expected, "{path} {content_type:?}");
    }
    assert!(
        world
            .gateway
            .received_headers
            .lock()
            .map_err(|_| TestError::Setup("lock"))?
            .is_empty()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_gateway_is_a_bad_gateway() -> Result<(), TestError> {
    // A port that was just free: nothing listens there.
    let free = TcpListener::bind("127.0.0.1:0").await?.local_addr()?;
    let config = Config::parse(&format!(
        "listen = \"127.0.0.1:0\"\ngateway = \"http://{free}/ohttp\"\nresponse_timeout_seconds = 5\n"
    ))
    .map_err(|_| TestError::Setup("relay config"))?;
    let listener = TcpListener::bind(config.listen).await?;
    let address = listener.local_addr()?;
    let mut relay_tasks = JoinSet::new();
    let relay = Arc::new(Relay::new(config));
    relay_tasks.spawn(async move {
        let _ = serve(listener, relay, std::future::pending()).await;
    });
    let (mut client, _connection) = connect(address).await?;
    let (sender, body) = channel();
    send(&sender, vec![0; 10])?;
    drop(sender);
    let response = tokio::time::timeout(
        STEP,
        client.send_request(post("/ohttp", "message/ohttp-req", body)?),
    )
    .await??;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_returns_when_shutdown_completes() -> Result<(), TestError> {
    let config =
        Config::parse("listen = \"127.0.0.1:0\"\ngateway = \"http://127.0.0.1:1/ohttp\"\n")
            .map_err(|_| TestError::Setup("relay config"))?;
    let listener = TcpListener::bind(config.listen).await?;
    tokio::time::timeout(
        STEP,
        serve(listener, Arc::new(Relay::new(config)), async {}),
    )
    .await??;
    Ok(())
}
