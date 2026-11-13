// SPDX-License-Identifier: AGPL-3.0-only
//! The outer HTTP layer: HTTP/2 connections (prior knowledge; TLS is terminated in front, as
//! for the reference relay) on two listeners. The public one serves metrics and direct
//! requests; the gateway one serves `POST /ohttp` and the key config only.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{any, get, post};
use axum::{Extension, Router};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use pingers_ohttp::{BinaryRequest, Header};
use tokio::net::TcpStream;
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

/// Accepts HTTP/2 connections on both listeners until `shutdown` completes, then drops every
/// connection it owns (live channels included, which would otherwise keep a graceful shutdown
/// waiting for as long as a Meetup).
pub(crate) async fn accept_loop(
    listeners: Listeners,
    shared: &Arc<Shared>,
    shutdown: impl Future<Output = ()>,
) {
    let public = public_router(Arc::clone(shared));
    let gateway = gateway_router(Arc::clone(shared));
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listeners.public.accept() => {
                spawn_connection(&mut connections, accepted, &public);
            }
            accepted = listeners.gateway.accept() => {
                spawn_connection(&mut connections, accepted, &gateway);
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
}

/// Serves one accepted connection with `app`, owned by `connections`.
fn spawn_connection(
    connections: &mut JoinSet<()>,
    accepted: std::io::Result<(TcpStream, SocketAddr)>,
    app: &Router,
) {
    // A failed accept (a client that left, too many open files) affects only that connection,
    // and is not logged.
    let Ok((stream, peer)) = accepted else {
        return;
    };
    // Live frames are small and latency matters more than packet count.
    let _ = stream.set_nodelay(true);
    // The address is used for the relay and metrics checks, and nowhere else.
    let service = TowerToHyperService::new(app.clone().layer(Extension(Peer(peer))));
    connections.spawn(async move {
        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
            .serve_connection(TokioIo::new(stream), service)
            .await;
    });
}
