// SPDX-License-Identifier: AGPL-3.0-only
//! The outer HTTP layer: HTTP/2 connections (prior knowledge; TLS is terminated in front, as
//! for the reference relay) on two listeners. The public one serves metrics, direct requests
//! and the invite landing page with App Links (`links.rs`); the gateway one serves `POST /ohttp` and the key config only.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{any, get, post};
use axum::{Extension, Router};
use hyper::body::{Frame, SizeHint};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use pingers_ohttp::{BinaryRequest, Header};
use pingers_time::{MonotonicTime, Timer};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::dispatch::{answer, key_config_reply};
use crate::gateway::{MAX_ONE_SHOT, direct_response, post_ohttp, read_body};
use crate::quota_routes::CLIENT_ADDRESS_HEADER;
use crate::route::Channel;
use crate::{Listeners, Peer, Shared};

/// Where the relay fetches the key config it hands to clients.
const OHTTP_CONFIG_PATH: &str = "/v1/ohttp/config";

/// The other rotation slot's key config, on the gateway listener only: what the appliance's
/// setup page pins besides the served key, from the running server (a pin set holds both).
const OHTTP_NEXT_CONFIG_PATH: &str = "/v1/ohttp/config/next";

/// The public listener: metrics and direct requests. The gateway is not here: behind a
/// reverse proxy every client looks like the proxy, so the relay-only rule could not hold.
pub(crate) fn public_router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/ohttp", any(not_found))
        .route("/metrics", get(metrics))
        .route(crate::links::LANDING_PATH, get(landing))
        .route(crate::links::SCRIPT_PATH, get(crate::links::script))
        .route(crate::links::STYLE_PATH, get(crate::links::style))
        .route(crate::links::ASSET_LINKS_PATH, get(asset_links))
        .fallback(direct)
        .with_state(shared)
}

/// The gateway listener: the gateway and its key configs (served and next), nothing else.
pub(crate) fn gateway_router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/ohttp", post(post_ohttp))
        .route(OHTTP_CONFIG_PATH, get(key_config))
        .route(OHTTP_NEXT_CONFIG_PATH, get(next_key_config))
        .fallback(not_found)
        .with_state(shared)
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// `GET /v1/ohttp/config` on the gateway listener, the same answer as on the public one.
async fn key_config(State(shared): State<Arc<Shared>>) -> Response {
    let reply = key_config_reply(&shared);
    shared.metrics.record(Channel::Direct, reply.status);
    direct_response(reply)
}

/// `GET /v1/ohttp/config/next` on the gateway listener: the key config of the slot that is not
/// served, `404` with a single key. Every configured key opens requests, so this is public
/// information ahead of a rotation, as the pins in app builds are.
async fn next_key_config(State(shared): State<Arc<Shared>>) -> Response {
    let reply = match &shared.next_config {
        Some(config) => crate::dispatch::Reply {
            body: config.encoded().to_vec(),
            ..key_config_reply(&shared)
        },
        None => crate::dispatch::Reply::status(StatusCode::NOT_FOUND),
    };
    shared.metrics.record(Channel::Direct, reply.status);
    direct_response(reply)
}

/// `GET /j`.
async fn landing(State(shared): State<Arc<Shared>>) -> Response {
    crate::links::landing(&shared.links.landing)
}

/// `GET /.well-known/assetlinks.json`.
async fn asset_links(State(shared): State<Arc<Shared>>) -> Response {
    crate::links::asset_links(shared.links.asset_links.as_ref())
}

/// `GET /metrics`, for the configured addresses only; to anyone else it does not exist. The
/// address checked is the client's as the trusted proxies report it: behind a reverse proxy on
/// loopback every request would otherwise come from an allowed address.
async fn metrics(
    State(shared): State<Arc<Shared>>,
    Extension(Peer(peer)): Extension<Peer>,
    headers: axum::http::HeaderMap,
) -> Response {
    let client = shared.trusted_proxies.client(peer.ip(), &headers);
    if client.is_some_and(|client| shared.metrics_access.contains(client)) {
        shared.metrics.render().into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Every other request: an identified or public route, answered directly. The address it came
/// from (after trusted proxies) goes with it as `pingers-client-address`, for the limits on
/// failed registration codes; one a client sent itself is dropped.
async fn direct(
    State(shared): State<Arc<Shared>>,
    Extension(Peer(peer)): Extension<Peer>,
    request: Request,
) -> Response {
    let (parts, mut body) = request.into_parts();
    let client = shared.trusted_proxies.client(peer.ip(), &parts.headers);
    let body = match read_body(&shared, &mut body, MAX_ONE_SHOT).await {
        Ok(body) => body,
        Err(error) => {
            shared.metrics.record(Channel::Direct, error.status());
            return error.status().into_response();
        }
    };
    let request = BinaryRequest {
        method: parts.method.as_str().to_owned(),
        path: parts
            .uri
            .path_and_query()
            .map_or("/", |path| path.as_str())
            .to_owned(),
        headers: parts
            .headers
            .iter()
            .filter(|(name, _)| name.as_str() != CLIENT_ADDRESS_HEADER)
            .map(|(name, value)| Header {
                name: name.as_str().to_owned(),
                value: value.as_bytes().to_vec(),
            })
            .chain(client.map(|address| Header {
                name: CLIENT_ADDRESS_HEADER.to_owned(),
                value: address.to_string().into_bytes(),
            }))
            .collect(),
        body,
    };
    let reply = answer(&shared, Channel::Direct, &request).await;
    shared.metrics.record(Channel::Direct, reply.status);
    direct_response(reply)
}

/// The largest request head (all header fields, HPACK-decoded) either listener accepts. A
/// request carries a few short headers and one credential (an RLN proof is 265 bytes before
/// base64).
pub(crate) const MAX_HEADER_LIST: u32 = 16 * 1024;

/// Concurrent requests per public connection: a phone sends a few at a time.
pub(crate) const PUBLIC_STREAMS: u32 = 64;

/// Concurrent requests per gateway connection: a relay multiplexes every device's requests,
/// live channels included, over a few connections.
pub(crate) const GATEWAY_STREAMS: u32 = 4096;

/// How many connections each listener holds, and how long one may stay idle.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConnectionPolicy {
    /// How long a connection may go with no request in progress.
    pub(crate) idle_timeout: Duration,
    /// Connections the public listener holds at once.
    pub(crate) public_limit: NonZeroU32,
    /// Connections the gateway listener holds at once.
    pub(crate) gateway_limit: NonZeroU32,
}

/// One listener's routes and limits.
struct Listener {
    app: Router,
    slots: Arc<Semaphore>,
    streams: u32,
    requests: Requests,
}

/// How long a request on a listener may be in progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Requests {
    /// At most [`OVERDUE_FACTOR`] request timeouts (the public listener: every route answers
    /// within its body timeout and one bounded call).
    Bounded,
    /// As long as they like (the gateway: live channels last as long as a Meetup, and the
    /// relay, not the phone, is the peer).
    MayLast,
}

/// A listener's connection slots.
fn slots(limit: NonZeroU32) -> Arc<Semaphore> {
    let slots = usize::try_from(limit.get()).unwrap_or(Semaphore::MAX_PERMITS);
    Arc::new(Semaphore::new(slots.min(Semaphore::MAX_PERMITS)))
}

/// How many request timeouts a request on the public listener may take in all (reading its
/// body, being answered, and sending the answer) before its connection is dropped.
pub(crate) const OVERDUE_FACTOR: u32 = 3;

/// Accepts HTTP/2 connections on both listeners until `shutdown` completes, then drops every
/// connection it owns (live channels included, which would otherwise keep a graceful shutdown
/// waiting for as long as a Meetup).
///
/// Against slow and idle clients: each listener holds at most its configured number of
/// connections (one more is closed on arrival), a connection with no request in progress for
/// the idle timeout is closed (so is one that never completes a request head), a public
/// connection whose oldest request has taken [`OVERDUE_FACTOR`] request timeouts is dropped
/// (a client that stops reading its answer), request heads are limited to
/// [`MAX_HEADER_LIST`], and request bodies to their route's size within the request timeout
/// (`gateway.rs`).
pub(crate) async fn accept_loop(
    listeners: Listeners,
    shared: &Arc<Shared>,
    shutdown: impl Future<Output = ()>,
) {
    let policy = shared.connections;
    let public = Listener {
        app: public_router(Arc::clone(shared)),
        slots: slots(policy.public_limit),
        streams: PUBLIC_STREAMS,
        requests: Requests::Bounded,
    };
    let gateway = Listener {
        app: gateway_router(Arc::clone(shared)),
        slots: slots(policy.gateway_limit),
        streams: GATEWAY_STREAMS,
        requests: Requests::MayLast,
    };
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listeners.public.accept() => {
                spawn_connection(&mut connections, accepted, &public, shared);
            }
            accepted = listeners.gateway.accept() => {
                spawn_connection(&mut connections, accepted, &gateway, shared);
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
}

/// Serves one accepted connection on `listener`, owned by `connections`, if the listener has
/// room for it; otherwise closes it at once.
fn spawn_connection(
    connections: &mut JoinSet<()>,
    accepted: std::io::Result<(TcpStream, SocketAddr)>,
    listener: &Listener,
    shared: &Shared,
) {
    // A failed accept (a client that left, too many open files) affects only that connection,
    // and is not logged.
    let Ok((stream, peer)) = accepted else {
        return;
    };
    let Ok(slot) = Arc::clone(&listener.slots).try_acquire_owned() else {
        // Full: dropping the stream closes it.
        return;
    };
    // Live frames are small and latency matters more than packet count.
    let _ = stream.set_nodelay(true);
    let activity = Arc::new(Activity::new(Arc::clone(&shared.timer)));
    // The address is used for the relay and metrics checks, and nowhere else.
    let service = TowerToHyperService::new(
        listener
            .app
            .clone()
            .layer(axum::middleware::from_fn(track_activity))
            .layer(Extension(Peer(peer)))
            .layer(Extension(Arc::clone(&activity))),
    );
    let mut builder = hyper::server::conn::http2::Builder::new(TokioExecutor::new());
    builder
        .max_header_list_size(MAX_HEADER_LIST)
        .max_concurrent_streams(listener.streams);
    let connection = builder.serve_connection(TokioIo::new(stream), service);
    let watch = IdleWatch {
        timer: Arc::clone(&shared.timer),
        activity,
        idle_timeout: shared.connections.idle_timeout,
        overdue_after: match listener.requests {
            Requests::Bounded => Some(shared.request_timeout.saturating_mul(OVERDUE_FACTOR)),
            Requests::MayLast => None,
        },
        grace: shared.request_timeout,
    };
    connections.spawn(async move {
        let _slot = slot;
        watch.serve(connection).await;
    });
}

/// What is going on on one connection: which requests are in progress since when, and since
/// when none has been.
struct Activity {
    state: Mutex<ActivityState>,
    timer: Arc<dyn Timer>,
}

struct ActivityState {
    /// Requests in progress by number; numbers grow with time, so the first is the oldest.
    requests: BTreeMap<u64, MonotonicTime>,
    next: u64,
    idle_since: MonotonicTime,
}

/// When a connection is due to be closed, if nothing changes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Deadlines {
    /// Idle long enough to be closed gracefully; `None` while a request is in progress.
    idle: Option<MonotonicTime>,
    /// Its oldest request overdue, so the connection is dropped; `None` without a request or
    /// on a listener with no such limit.
    overdue: Option<MonotonicTime>,
}

impl Activity {
    fn new(timer: Arc<dyn Timer>) -> Self {
        Self {
            state: Mutex::new(ActivityState {
                requests: BTreeMap::new(),
                next: 0,
                idle_since: timer.now(),
            }),
            timer,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ActivityState> {
        // A map and two numbers, each changed in one step: nothing a panic could leave
        // half-written.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn deadlines(&self, idle_timeout: Duration, overdue_after: Option<Duration>) -> Deadlines {
        let state = self.lock();
        let oldest = state.requests.values().next().copied();
        Deadlines {
            idle: oldest
                .is_none()
                .then(|| state.idle_since.saturating_add(idle_timeout)),
            overdue: oldest
                .zip(overdue_after)
                .map(|(start, after)| start.saturating_add(after)),
        }
    }
}

/// A request in progress, from its head until its response body is finished or dropped.
struct InProgress {
    activity: Arc<Activity>,
    number: u64,
}

impl InProgress {
    fn start(activity: Arc<Activity>) -> Self {
        let now = activity.timer.now();
        let mut state = activity.lock();
        let number = state.next;
        state.next = state.next.wrapping_add(1);
        state.requests.insert(number, now);
        drop(state);
        Self { activity, number }
    }
}

impl Drop for InProgress {
    fn drop(&mut self) {
        let now = self.activity.timer.now();
        let mut state = self.activity.lock();
        state.requests.remove(&self.number);
        if state.requests.is_empty() {
            state.idle_since = now;
        }
    }
}

/// Counts the request as in progress until its response body ends: a live channel's
/// response lasts as long as the channel, and keeps its connection open.
async fn track_activity(
    Extension(activity): Extension<Arc<Activity>>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let in_progress = InProgress::start(activity);
    let response = next.run(request).await;
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(TrackedBody {
            inner: body,
            _in_progress: in_progress,
        }),
    )
}

/// A response body that keeps its request counted as in progress until it ends.
struct TrackedBody {
    inner: Body,
    _in_progress: InProgress,
}

impl HttpBody for TrackedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        Pin::new(&mut self.get_mut().inner).poll_frame(context)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Closes a connection once it has been idle for the idle timeout, or drops it once a
/// request on it is overdue.
struct IdleWatch {
    timer: Arc<dyn Timer>,
    activity: Arc<Activity>,
    idle_timeout: Duration,
    /// How long a request may be in progress; `None` where requests may last (the gateway's
    /// live channels).
    overdue_after: Option<Duration>,
    /// How long requests that arrive as the connection is being closed may still take.
    grace: Duration,
}

impl IdleWatch {
    async fn serve(self, connection: Connection) {
        let mut connection = std::pin::pin!(connection);
        loop {
            let due = self
                .activity
                .deadlines(self.idle_timeout, self.overdue_after);
            let wake = [due.idle, due.overdue]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or_else(|| self.timer.now().saturating_add(self.idle_timeout));
            tokio::select! {
                _ = connection.as_mut() => return,
                () = self.timer.sleep_until(wake) => {}
            }
            let now = self.timer.now();
            let due = self
                .activity
                .deadlines(self.idle_timeout, self.overdue_after);
            if due.overdue.is_some_and(|deadline| now >= deadline) {
                // A client that stopped reading its response (and so granting HTTP/2
                // flow-control credit) would hold the stream for ever: drop the connection.
                return;
            }
            if due.idle.is_some_and(|deadline| now >= deadline) {
                break;
            }
        }
        // GOAWAY: the client may retry anything the server did not start on elsewhere.
        connection.as_mut().graceful_shutdown();
        tokio::select! {
            _ = connection.as_mut() => {}
            () = self.timer.sleep(self.grace) => {}
        }
    }
}

/// One served HTTP/2 connection.
type Connection = hyper::server::conn::http2::Connection<
    TokioIo<TcpStream>,
    TowerToHyperService<Router>,
    TokioExecutor,
>;
