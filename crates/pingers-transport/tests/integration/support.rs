// SPDX-License-Identifier: AGPL-3.0-only
//! The world the integration tests run in: a test gateway that decapsulates with
//! pingers-ohttp, the real reference relay in front of it (its own source, compiled in), a
//! relay that buffers request bodies (for the split channel), and a client transport on a
//! manual clock. Everything talks real HTTP/2 over loopback.

use std::convert::Infallible;
use std::future::{Future, poll_fn};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::body::{Body, Bytes, Frame, Incoming};
use hyper::header::CONTENT_TYPE;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use pingers_ohttp::{
    BinaryRequest, BinaryResponse, ChunkPosition, ChunkedGateway, Gateway, GatewayKey, KeyConfig,
    OhttpError,
};
use pingers_testkit::seeded_rng;
use pingers_time::{ManualClock, MonotonicTime, Timer, UnixSeconds};
use pingers_transport::{
    KEEP_ALIVE_CHUNK, KeyPin, OhttpConfig, OhttpTransport, Relay, TlsRoots, TransportError,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::config::Config;

/// How long any single step may take in real time; a hang fails the test instead of
/// stalling it.
pub(crate) const STEP: Duration = Duration::from_secs(10);

/// The inner path of the live channel, as pingers-meetup-session would ask for it.
pub(crate) const LIVE_PATH: &str = "/v1/groups/abc/live";

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read through Debug when a test fails"
)]
pub(crate) enum TestError {
    Transport(TransportError),
    Ohttp(OhttpError),
    Io(std::io::Error),
    Hyper(hyper::Error),
    Http(hyper::http::Error),
    Tls(rustls::Error),
    Certificate(rcgen::Error),
    Timeout,
    Setup(&'static str),
}

macro_rules! from {
    ($($source:ty => $variant:ident),* $(,)?) => {
        $(impl From<$source> for TestError {
            fn from(error: $source) -> Self {
                Self::$variant(error)
            }
        })*
    };
}

from!(
    TransportError => Transport,
    OhttpError => Ohttp,
    std::io::Error => Io,
    hyper::Error => Hyper,
    hyper::http::Error => Http,
    rustls::Error => Tls,
    rcgen::Error => Certificate,
);

impl From<tokio::time::error::Elapsed> for TestError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Self::Timeout
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lets real time pass, so loopback I/O can happen while a test polls a condition.
#[expect(
    clippy::disallowed_methods,
    reason = "the network under test is real; only the transport's timers are manual"
)]
async fn real_pause() {
    tokio::time::sleep(Duration::from_millis(10)).await;
}

/// The monotonic time `elapsed` after `clock`'s current reading.
pub(crate) fn after(clock: &ManualClock, elapsed: Duration) -> MonotonicTime {
    Timer::now(clock).saturating_add(elapsed)
}

/// Runs `future` to completion, moving `clock` to each of `deadlines` in turn, but only once
/// the code under test is parked on that deadline. Time never moves while real I/O is still
/// in flight, so a slow machine cannot fire a timeout the test did not mean to.
pub(crate) async fn advancing_through<F: Future>(
    clock: &ManualClock,
    deadlines: &[MonotonicTime],
    future: F,
) -> Result<F::Output, TestError> {
    let mut future = std::pin::pin!(future);
    let mut deadlines = deadlines.iter().copied().peekable();
    tokio::time::timeout(STEP, async {
        loop {
            tokio::select! {
                output = &mut future => return output,
                () = real_pause() => {
                    if let Some(deadline) = deadlines.next_if(|d| clock.is_sleeping_until(*d)) {
                        clock.advance(deadline.saturating_duration_since(Timer::now(clock)));
                    }
                }
            }
        }
    })
    .await
    .map_err(TestError::from)
}

/// Waits (in real time) until `condition` holds.
pub(crate) async fn eventually(mut condition: impl FnMut() -> bool) -> Result<(), TestError> {
    tokio::time::timeout(STEP, async {
        while !condition() {
            real_pause().await;
        }
    })
    .await
    .map_err(TestError::from)
}

/// A body fed from a channel, or one passed through from the other side.
pub(crate) enum TestBody {
    Channel(mpsc::UnboundedReceiver<Bytes>),
    Forwarded {
        body: Incoming,
        _driver: AbortOnDrop,
    },
}

impl Body for TestBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        match self.get_mut() {
            Self::Channel(receiver) => receiver
                .poll_recv(context)
                .map(|message| message.map(|bytes| Ok(Frame::data(bytes)))),
            Self::Forwarded { body, .. } => Pin::new(body).poll_frame(context),
        }
    }
}

fn full(bytes: Vec<u8>) -> TestBody {
    let (sender, receiver) = mpsc::unbounded_channel();
    let _ = sender.send(Bytes::from(bytes));
    TestBody::Channel(receiver)
}

async fn next_data(body: &mut Incoming) -> Result<Option<Bytes>, TestError> {
    loop {
        match poll_fn(|context| Pin::new(&mut *body).poll_frame(context)).await {
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

/// Serves `handler` over HTTP/2 (prior knowledge) on `listener` until the set is dropped.
pub(crate) fn serve_h2<F, Fut>(listener: TcpListener, handler: F) -> JoinSet<()>
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Response<TestBody>, Infallible>> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let mut connections = JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            let _ = stream.set_nodelay(true);
            let handler = handler.clone();
            connections.spawn(async move {
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), hyper::service::service_fn(handler))
                    .await;
            });
        }
    });
    tasks
}

fn status(code: StatusCode) -> Response<TestBody> {
    let (_, body) = mpsc::unbounded_channel();
    let mut response = Response::new(TestBody::Channel(body));
    *response.status_mut() = code;
    response
}

/// What the gateway saw of one chunked exchange.
#[derive(Clone, Debug, Default)]
pub(crate) struct ExchangeRecord {
    /// The key id, suite and encapsulated key: different for every fresh HPKE context.
    pub(crate) encapsulation: Vec<u8>,
    /// Frames received, in order (keep-alives excluded).
    pub(crate) frames: Vec<Vec<u8>>,
    /// Keep-alive chunks received.
    pub(crate) keep_alives: usize,
    /// The final chunk arrived (the client closed cleanly).
    pub(crate) finished: bool,
    /// The gateway's side of the exchange is over.
    pub(crate) ended: bool,
}

/// Everything the gateway saw.
#[derive(Default)]
pub(crate) struct Seen {
    /// Every HTTP request that reached the gateway, by path.
    pub(crate) paths: Vec<String>,
    /// The encapsulated key of each one-shot request.
    pub(crate) one_shot_encapsulations: Vec<Vec<u8>>,
    /// Inner paths of decapsulated one-shot requests.
    pub(crate) inner_paths: Vec<String>,
    pub(crate) exchanges: Vec<ExchangeRecord>,
}

/// The test gateway: answers one-shot requests with `seen <path>`, echoes live frames as
/// `echo <frame>`, answers keep-alives with keep-alives, and lets the test push frames down
/// the newest live exchange.
pub(crate) struct TestGateway {
    gateway: Gateway,
    pub(crate) config: KeyConfig,
    seen: Mutex<Seen>,
    /// Push handle of the newest live exchange.
    downlink: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    tasks: Mutex<JoinSet<()>>,
}

/// One live exchange as the gateway runs it.
struct LiveExchange {
    accepted: ChunkedGateway,
    body: Incoming,
    sender: mpsc::UnboundedSender<Bytes>,
    pushed: mpsc::UnboundedReceiver<Vec<u8>>,
    index: usize,
}

/// The encapsulation header and key at the start of every request (7 + 32 bytes).
const ENCAPSULATION: usize = 39;

impl TestGateway {
    pub(crate) fn seen(&self) -> MutexGuard<'_, Seen> {
        lock(&self.seen)
    }

    /// Sends `frame` down the newest live exchange.
    pub(crate) fn push(&self, frame: &[u8]) -> Result<(), TestError> {
        lock(&self.downlink)
            .as_ref()
            .ok_or(TestError::Setup("no live exchange"))?
            .send(frame.to_vec())
            .map_err(|_| TestError::Setup("live exchange gone"))
    }

    async fn handle(self: Arc<Self>, request: Request<Incoming>) -> Response<TestBody> {
        let path = request.uri().path().to_owned();
        self.seen().paths.push(path.clone());
        let content_type = request
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let result = match content_type.as_deref() {
            Some("message/ohttp-req") => self.one_shot(request.into_body()).await,
            Some("message/ohttp-chunked-req") => {
                Arc::clone(&self).chunked(request.into_body()).await
            }
            _ if path == "/v1/ohttp/config" => Ok(Response::builder()
                .header(CONTENT_TYPE, "application/ohttp-keys")
                .body(full(self.config.encoded().to_vec()))
                .unwrap_or_else(|_| status(StatusCode::INTERNAL_SERVER_ERROR))),
            _ => Ok(status(StatusCode::NOT_FOUND)),
        };
        result.unwrap_or_else(|_| status(StatusCode::INTERNAL_SERVER_ERROR))
    }

    async fn one_shot(&self, mut body: Incoming) -> Result<Response<TestBody>, TestError> {
        let message = read_all(&mut body).await?;
        let decapsulated = self.gateway.decapsulate(&message)?;
        let inner = decapsulated.request;
        {
            let mut seen = self.seen();
            seen.one_shot_encapsulations
                .push(message.get(..ENCAPSULATION).unwrap_or_default().to_vec());
            seen.inner_paths.push(inner.path.clone());
        }
        // A split-channel uplink frame: echo it down the live exchange.
        if inner.path == LIVE_PATH {
            if let Some(exchange) = self.seen().exchanges.last_mut() {
                exchange.frames.push(inner.body.clone());
            }
            self.push(&[b"echo ".as_slice(), &inner.body].concat())?;
        }
        let reply = BinaryResponse {
            status: pingers_ohttp::StatusCode::new(200)?,
            headers: Vec::new(),
            body: [
                b"seen ".as_slice(),
                inner.path.as_bytes(),
                b" ",
                &inner.body,
            ]
            .concat(),
        };
        let sealed = decapsulated.responder.seal(&reply, &mut seeded_rng(100))?;
        Ok(Response::builder()
            .header(CONTENT_TYPE, "message/ohttp-res")
            .body(full(sealed))?)
    }

    async fn chunked(self: Arc<Self>, mut body: Incoming) -> Result<Response<TestBody>, TestError> {
        let mut rng = seeded_rng(101);
        let mut received = Vec::new();
        let accepted = loop {
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
        if accepted.request().path != LIVE_PATH {
            return Err(TestError::Setup("unexpected live path"));
        }
        let (sender, response_body) = mpsc::unbounded_channel();
        sender
            .send(Bytes::from(accepted.response_preamble()))
            .map_err(|_| TestError::Setup("response body gone"))?;
        let (push, pushed) = mpsc::unbounded_channel();
        *lock(&self.downlink) = Some(push);
        let index = {
            let mut seen = self.seen();
            seen.exchanges.push(ExchangeRecord {
                encapsulation: received.get(..ENCAPSULATION).unwrap_or_default().to_vec(),
                ..ExchangeRecord::default()
            });
            seen.exchanges.len().saturating_sub(1)
        };
        let gateway = Arc::clone(&self);
        lock(&self.tasks).spawn(async move {
            let exchange = LiveExchange {
                accepted,
                body,
                sender,
                pushed,
                index,
            };
            let _ = gateway.live(exchange).await;
            if let Some(record) = gateway.seen().exchanges.get_mut(index) {
                record.ended = true;
            }
        });
        Ok(Response::builder()
            .header(CONTENT_TYPE, "message/ohttp-chunked-res")
            .body(TestBody::Channel(response_body))?)
    }

    /// Runs one live exchange: request chunks in, echoes and pushed frames out.
    async fn live(&self, exchange: LiveExchange) -> Result<(), TestError> {
        let LiveExchange {
            mut accepted,
            mut body,
            sender,
            mut pushed,
            index,
        } = exchange;
        let mut decoder = Some(accepted.request_decoder());
        let mut chunks = decoder
            .as_mut()
            .ok_or(TestError::Setup("decoder"))?
            .feed(&[])?;
        loop {
            for chunk in chunks.drain(..) {
                let reply = if chunk.payload == KEEP_ALIVE_CHUNK {
                    if let Some(exchange) = self.seen().exchanges.get_mut(index) {
                        exchange.keep_alives = exchange.keep_alives.saturating_add(1);
                    }
                    KEEP_ALIVE_CHUNK.to_vec()
                } else {
                    if let Some(exchange) = self.seen().exchanges.get_mut(index) {
                        exchange.frames.push(chunk.payload.clone());
                    }
                    [b"echo ".as_slice(), &chunk.payload].concat()
                };
                send(
                    &sender,
                    accepted.seal_chunk(&reply, ChunkPosition::NonFinal)?,
                )?;
            }
            tokio::select! {
                data = next_data(&mut body), if decoder.is_some() => {
                    if let Some(data) = data? {
                        if let Some(decoder) = decoder.as_mut() {
                            chunks = decoder.feed(&data)?;
                        }
                    } else {
                        let last = decoder
                            .take()
                            .ok_or(TestError::Setup("decoder"))?
                            .finish()?;
                        let mut seen = self.seen();
                        let exchange = seen
                            .exchanges
                            .get_mut(index)
                            .ok_or(TestError::Setup("exchange"))?;
                        exchange.finished = last.is_some();
                        // A request that ends right after its head is a split-channel
                        // downlink; one that ends later is a client closing the channel.
                        let split = exchange.frames.is_empty() && exchange.keep_alives == 0;
                        drop(seen);
                        if !split {
                            send(&sender, accepted.seal_chunk(&[], ChunkPosition::Final)?)?;
                            return Ok(());
                        }
                    }
                }
                frame = pushed.recv() => {
                    if let Some(frame) = frame {
                        send(&sender, accepted.seal_chunk(&frame, ChunkPosition::NonFinal)?)?;
                    } else {
                        // Ended by the test, or replaced by a newer exchange.
                        send(&sender, accepted.seal_chunk(&[], ChunkPosition::Final)?)?;
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Ends the newest live exchange from the gateway side.
    pub(crate) fn end_live(&self) {
        *lock(&self.downlink) = None;
    }
}

fn send(sender: &mpsc::UnboundedSender<Bytes>, bytes: Vec<u8>) -> Result<(), TestError> {
    sender
        .send(Bytes::from(bytes))
        .map_err(|_| TestError::Setup("response body gone"))
}

/// The real reference relay, running until dropped.
pub(crate) struct RunningRelay {
    pub(crate) address: SocketAddr,
    _tasks: JoinSet<()>,
}

/// Starts the reference relay in front of the gateway at `gateway`, on `address`.
pub(crate) async fn start_relay(
    address: SocketAddr,
    gateway: SocketAddr,
) -> Result<RunningRelay, TestError> {
    let config = Config::parse(&format!(
        "listen = \"{address}\"\n\
         gateway = \"http://{gateway}/ohttp\"\n\
         gateway_key_config = \"http://{gateway}/v1/ohttp/config\"\n\
         response_timeout_seconds = 5\n"
    ))
    .map_err(|_| TestError::Setup("relay config"))?;
    let listener = TcpListener::bind(config.listen).await?;
    let address = listener.local_addr()?;
    let relay = Arc::new(crate::relay::Relay::new(config));
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let _ = crate::relay::serve(listener, relay, std::future::pending()).await;
    });
    Ok(RunningRelay {
        address,
        _tasks: tasks,
    })
}

/// A relay that reads each request body to the end before forwarding it, as a relay that
/// cannot stream full duplex does; responses still stream.
pub(crate) async fn start_buffering_relay(gateway: SocketAddr) -> Result<RunningRelay, TestError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let tasks = serve_h2(listener, move |request: Request<Incoming>| async move {
        Ok(forward_buffered(gateway, request)
            .await
            .unwrap_or_else(|_| status(StatusCode::BAD_GATEWAY)))
    });
    Ok(RunningRelay {
        address,
        _tasks: tasks,
    })
}

async fn forward_buffered(
    gateway: SocketAddr,
    request: Request<Incoming>,
) -> Result<Response<TestBody>, TestError> {
    let (parts, mut body) = request.into_parts();
    let whole = read_all(&mut body).await?;
    let stream = TcpStream::connect(gateway).await?;
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream)).await?;
    // The connection lives as long as the response body streams through it.
    let driver = tokio::spawn(connection);
    let mut upstream = Request::new(full(whole));
    *upstream.method_mut() = parts.method;
    *upstream.uri_mut() = format!("http://{gateway}{}", parts.uri.path())
        .parse()
        .map_err(|_| TestError::Setup("uri"))?;
    if let Some(content_type) = parts.headers.get(CONTENT_TYPE) {
        upstream
            .headers_mut()
            .insert(CONTENT_TYPE, content_type.clone());
    }
    let response = sender.send_request(upstream).await?;
    let (parts, body) = response.into_parts();
    Ok(Response::from_parts(
        parts,
        TestBody::Forwarded {
            body,
            _driver: AbortOnDrop(driver),
        },
    ))
}

/// Owns a connection driver: it lives exactly as long as the body passed through it.
pub(crate) struct AbortOnDrop(tokio::task::JoinHandle<Result<(), hyper::Error>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A gateway behind the real relay, and a client transport on a manual clock.
pub(crate) struct World {
    pub(crate) gateway: Arc<TestGateway>,
    pub(crate) gateway_address: SocketAddr,
    pub(crate) relay: Option<RunningRelay>,
    pub(crate) clock: ManualClock,
    _gateway_tasks: JoinSet<()>,
}

pub(crate) async fn world() -> Result<World, TestError> {
    let key = GatewayKey::generate(1, &mut seeded_rng(1))?;
    let config = key.config();
    let gateway = Arc::new(TestGateway {
        gateway: Gateway::new(vec![key]),
        config,
        seen: Mutex::new(Seen::default()),
        downlink: Mutex::new(None),
        tasks: Mutex::new(JoinSet::new()),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let gateway_address = listener.local_addr()?;
    let shared = Arc::clone(&gateway);
    let gateway_tasks = serve_h2(listener, move |request| {
        let shared = Arc::clone(&shared);
        async move { Ok::<_, Infallible>(shared.handle(request).await) }
    });
    let relay = start_relay(
        "127.0.0.1:0"
            .parse()
            .map_err(|_| TestError::Setup("address"))?,
        gateway_address,
    )
    .await?;
    Ok(World {
        gateway,
        gateway_address,
        relay: Some(relay),
        clock: ManualClock::new(UnixSeconds::new(1_800_000_000)),
        _gateway_tasks: gateway_tasks,
    })
}

/// A relay entry for a relay listening on `address`.
pub(crate) fn relay_at(address: SocketAddr) -> Result<Relay, TestError> {
    Ok(Relay::new(
        &format!("http://{address}/ohttp"),
        &format!("http://{address}/v1/ohttp/config"),
    )?)
}

/// An address nothing listens on.
pub(crate) async fn closed_address() -> Result<SocketAddr, TestError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    drop(listener);
    Ok(address)
}

impl World {
    pub(crate) fn relay_address(&self) -> Result<SocketAddr, TestError> {
        self.relay
            .as_ref()
            .map(|relay| relay.address)
            .ok_or(TestError::Setup("relay stopped"))
    }

    /// A client over `relays`, pinned to `pin`, on the world's clock.
    pub(crate) fn client_with(
        &self,
        relays: Vec<Relay>,
        pin: KeyPin,
    ) -> Result<OhttpTransport, TestError> {
        let config = OhttpConfig::new(relays, pin, TlsRoots::from_der(&[])?);
        Ok(OhttpTransport::new(
            config,
            Arc::new(self.clock.clone()),
            Box::new(seeded_rng(7)),
        )?)
    }

    /// A client over the world's relay, pinned to the gateway's real key.
    pub(crate) fn client(&self) -> Result<OhttpTransport, TestError> {
        self.client_with(
            vec![relay_at(self.relay_address()?)?],
            KeyPin::Pinned(self.gateway.config.hash()),
        )
    }
}

/// An anonymous inner request.
pub(crate) fn request(path: &str, body: &[u8]) -> BinaryRequest {
    BinaryRequest {
        method: "POST".to_owned(),
        path: path.to_owned(),
        headers: vec![pingers_ohttp::Header {
            name: "authorization".to_owned(),
            value: b"PrivateToken token=abc".to_vec(),
        }],
        body: body.to_vec(),
    }
}

/// The live channel's inner request head.
pub(crate) fn live_request() -> BinaryRequest {
    BinaryRequest {
        method: "POST".to_owned(),
        path: LIVE_PATH.to_owned(),
        headers: vec![pingers_ohttp::Header {
            name: "authorization".to_owned(),
            value: b"group-auth".to_vec(),
        }],
        body: Vec::new(),
    }
}
