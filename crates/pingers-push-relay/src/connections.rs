// SPDX-License-Identifier: AGPL-3.0-only
//! The accept loop, bounded against slow and idle clients: at most
//! [`ConnectionLimits::max_connections`] connections at once (one more is closed at once), a
//! connection that sends no complete request head for [`ConnectionLimits::idle_timeout`] is
//! closed (on a new connection and between keep-alive requests alike), and a request whose body
//! or answer takes longer than [`ConnectionLimits::request_timeout`] is answered 408 and its
//! connection closed. The relay is reached only by Pingers servers with short requests, so
//! none of this touches real traffic.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

/// The defaults; each can be set in the configuration file.
pub const DEFAULT_MAX_CONNECTIONS: u32 = 256;
/// The default idle timeout, in seconds.
pub const DEFAULT_IDLE_TIMEOUT_S: u64 = 30;

/// How many connections the relay holds, and how long one may wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionLimits {
    /// Connections held at once; one more is closed as soon as it is accepted.
    pub max_connections: usize,
    /// How long a connection may go without sending a complete request head.
    pub idle_timeout: Duration,
    /// How long one request may take, from its head to the end of its answer (reading the body
    /// and waiting for the push service included).
    pub request_timeout: Duration,
}

/// Serves `app` on `listener` within `limits` until `shutdown`, then lets each connection
/// finish the request it is on (up to one request timeout) and closes it.
pub(crate) async fn serve(
    listener: TcpListener,
    app: Router,
    limits: ConnectionLimits,
    shutdown: impl Future<Output = ()>,
) {
    let slots = Arc::new(Semaphore::new(
        limits.max_connections.clamp(1, Semaphore::MAX_PERMITS),
    ));
    let (stop, stopping) = watch::channel(false);
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                // A failed accept (a client that left, too many open files) affects only that
                // connection.
                let Ok((stream, _)) = accepted else { continue };
                let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
                    // Full: dropping the stream closes it.
                    continue;
                };
                let app = app.clone();
                let stopping = stopping.clone();
                connections.spawn(async move {
                    serve_one(stream, app, limits, stopping).await;
                    drop(slot);
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    let _ = stop.send(true);
    let drained = async { while connections.join_next().await.is_some() {} };
    if tokio::time::timeout(limits.request_timeout, drained)
        .await
        .is_err()
    {
        connections.shutdown().await;
    }
}

async fn serve_one(
    stream: tokio::net::TcpStream,
    app: Router,
    limits: ConnectionLimits,
    mut stopping: watch::Receiver<bool>,
) {
    let inner = TowerToHyperService::new(app);
    let request_timeout = limits.request_timeout;
    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
        let answer = hyper::service::Service::call(&inner, request);
        async move {
            Ok::<_, std::convert::Infallible>(
                match tokio::time::timeout(request_timeout, answer).await {
                    Ok(Ok(response)) => response,
                    Ok(Err(never)) => match never {},
                    Err(_) => timed_out(),
                },
            )
        }
    });
    // HTTP/1.1 only: servers reach the relay with one short request at a time, and an HTTP/2
    // connection with no stream open could be kept alive for ever with pings.
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.idle_timeout);
    let connection = builder.serve_connection(TokioIo::new(stream), service);
    let mut connection = std::pin::pin!(connection);
    tokio::select! {
        _ = connection.as_mut() => return,
        _ = stopping.wait_for(|stop| *stop) => {}
    }
    connection.as_mut().graceful_shutdown();
    let _ = connection.await;
}

/// The answer to a request that took too long; the connection closes after it.
fn timed_out() -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::REQUEST_TIMEOUT;
    response.headers_mut().insert(
        header::CONNECTION,
        header::HeaderValue::from_static("close"),
    );
    response
}
