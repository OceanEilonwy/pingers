// SPDX-License-Identifier: AGPL-3.0-only
//! The outer HTTP layer: HTTP/2 connections (prior knowledge; TLS is terminated in front, as
//! for the reference relay) on two listeners. The public one serves metrics and direct
//! requests; the gateway one serves `POST /ohttp` and the key config only.

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
use crate::route::Channel;
use crate::{Listeners, Peer, Shared};

/// Where the relay fetches the key config it hands to clients.
const OHTTP_CONFIG_PATH: &str = "/v1/ohttp/config";

/// The public listener: metrics and direct requests. The gateway is not here: behind a
/// reverse proxy every client looks like the proxy, so the relay-only rule could not hold.
pub(crate) fn public_router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/ohttp", any(not_found))
        .route("/metrics", get(metrics))
        .fallback(direct)
        .with_state(shared)
}

/// The gateway listener: the gateway and its key config, nothing else.
pub(crate) fn gateway_router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/ohttp", post(post_ohttp))
        .route(OHTTP_CONFIG_PATH, get(key_config))
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

/// `GET /metrics`, for the configured addresses only; to anyone else it does not exist.
async fn metrics(
    State(shared): State<Arc<Shared>>,
    Extension(Peer(peer)): Extension<Peer>,
) -> Response {
    if shared.metrics_access.contains(peer.ip()) {
        shared.metrics.render().into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Every other request: an identified or public route, answered directly.
async fn direct(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    let (parts, mut body) = request.into_parts();
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
            .map(|(name, value)| Header {
                name: name.as_str().to_owned(),
                value: value.as_bytes().to_vec(),
            })
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
}

impl Listener {
    fn new(app: Router, limit: NonZeroU32, streams: u32) -> Self {
        let slots = usize::try_from(limit.get()).unwrap_or(Semaphore::MAX_PERMITS);
        Self {
            app,
            slots: Arc::new(Semaphore::new(slots.min(Semaphore::MAX_PERMITS))),
            streams,
        }
    }
}

/// Accepts HTTP/2 connections on both listeners until `shutdown` completes, then drops every
/// connection it owns (live channels included, which would otherwise keep a graceful shutdown
/// waiting for as long as a Meetup).
///
/// Against slow and idle clients: each listener holds at most its configured number of
/// connections (one more is closed on arrival), a connection with no request in progress for
/// the idle timeout is closed (so is one that never completes a request head), request heads
/// are limited to [`MAX_HEADER_LIST`], and request bodies to their route's size within the
/// request timeout (`gateway.rs`).
pub(crate) async fn accept_loop(
    listeners: Listeners,
    shared: &Arc<Shared>,
    shutdown: impl Future<Output = ()>,
) {
    let policy = shared.connections;
    let public = Listener::new(
        public_router(Arc::clone(shared)),
        policy.public_limit,
        PUBLIC_STREAMS,
    );
    let gateway = Listener::new(
        gateway_router(Arc::clone(shared)),
        policy.gateway_limit,
        GATEWAY_STREAMS,
    );
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
        grace: shared.request_timeout,
    };
    connections.spawn(async move {
        let _slot = slot;
        watch.serve(connection).await;
    });
}

/// What is going on on one connection: how many requests are in progress, and since when
/// none has been.
struct Activity {
    state: Mutex<ActivityState>,
    timer: Arc<dyn Timer>,
}

#[derive(Clone, Copy)]
struct ActivityState {
    in_progress: usize,
    idle_since: MonotonicTime,
}

impl Activity {
    fn new(timer: Arc<dyn Timer>) -> Self {
        Self {
            state: Mutex::new(ActivityState {
                in_progress: 0,
                idle_since: timer.now(),
            }),
            timer,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ActivityState> {
        // A counter and a time: nothing a panic could leave half-written.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// When the connection will have been idle for `timeout`, or `None` while a request is
    /// in progress.
    fn idle_deadline(&self, timeout: Duration) -> Option<MonotonicTime> {
        let state = *self.lock();
        (state.in_progress == 0).then(|| state.idle_since.saturating_add(timeout))
    }
}

/// A request in progress, from its head until its response body is finished or dropped.
struct InProgress {
    activity: Arc<Activity>,
}

impl InProgress {
    fn start(activity: Arc<Activity>) -> Self {
        let mut state = activity.lock();
        state.in_progress = state.in_progress.saturating_add(1);
        drop(state);
        Self { activity }
    }
}

impl Drop for InProgress {
    fn drop(&mut self) {
        let mut state = self.activity.lock();
        state.in_progress = state.in_progress.saturating_sub(1);
        if state.in_progress == 0 {
            state.idle_since = self.activity.timer.now();
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

/// Closes a connection once it has been idle for the idle timeout.
struct IdleWatch {
    timer: Arc<dyn Timer>,
    activity: Arc<Activity>,
    idle_timeout: Duration,
    /// How long requests that arrive as the connection is being closed may still take.
    grace: Duration,
}

impl IdleWatch {
    async fn serve(self, connection: Connection) {
        let mut connection = std::pin::pin!(connection);
        loop {
            let deadline = self
                .activity
                .idle_deadline(self.idle_timeout)
                .unwrap_or_else(|| self.timer.now().saturating_add(self.idle_timeout));
            tokio::select! {
                _ = connection.as_mut() => return,
                () = self.timer.sleep_until(deadline) => {}
            }
            let idle_long_enough = self
                .activity
                .idle_deadline(self.idle_timeout)
                .is_some_and(|deadline| self.timer.now() >= deadline);
            if idle_long_enough {
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
