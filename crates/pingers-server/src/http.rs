// SPDX-License-Identifier: AGPL-3.0-only
//! The outer HTTP layer: HTTP/2 connections (prior knowledge; TLS is terminated in front, as
//! for the reference relay), the gateway route, metrics, and direct requests.

use std::future::Future;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use pingers_ohttp::{BinaryRequest, Header};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::dispatch::answer;
use crate::gateway::{MAX_ONE_SHOT, direct_response, post_ohttp, read_body};
use crate::route::Channel;
use crate::{Peer, Shared};

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/ohttp", post(post_ohttp))
        .route("/metrics", get(metrics))
        .fallback(direct)
        .with_state(shared)
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
    let reply = answer(&shared, Channel::Direct, &request);
    shared.metrics.record(Channel::Direct, reply.status);
    direct_response(reply)
}

/// Accepts HTTP/2 connections until `shutdown` completes, then drops every connection it owns
/// (live channels included, which would otherwise keep a graceful shutdown waiting for as long
/// as a Meetup).
pub(crate) async fn accept_loop(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()>,
) {
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                // A failed accept (a client that left, too many open files) affects only that
                // connection, and is not logged.
                let Ok((stream, peer)) = accepted else {
                    continue;
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
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
}
