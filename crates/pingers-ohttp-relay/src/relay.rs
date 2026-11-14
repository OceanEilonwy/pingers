// SPDX-License-Identifier: AGPL-3.0-only
//! Forwarding: one route for encapsulated requests, one for the key config, nothing else.
//!
//! Each forwarded request is rebuilt from scratch, so nothing the client sent reaches the
//! gateway except the body and its media type: no client address, `Forwarded`, cookies,
//! user agent or anything else. Bodies are passed through frame by frame in both directions
//! as they arrive, which over HTTP/2 gives the full-duplex streams chunked OHTTP needs.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::config::Config;

/// RFC 9458 request and response media types, and draft-08's chunked ones.
const OHTTP_REQUEST: &str = "message/ohttp-req";
const OHTTP_CHUNKED_REQUEST: &str = "message/ohttp-chunked-req";
const OHTTP_RESPONSE: &str = "message/ohttp-res";
const OHTTP_CHUNKED_RESPONSE: &str = "message/ohttp-chunked-res";
const OHTTP_KEYS: &str = "application/ohttp-keys";

/// A body the relay sends: a stream passed through, or nothing (for its own error replies).
pub(crate) enum RelayBody {
    /// Frames from the other side, forwarded as they arrive.
    Stream(Incoming),
    /// No body.
    Empty,
}

impl Body for RelayBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        match self.get_mut() {
            Self::Stream(incoming) => Pin::new(incoming).poll_frame(context),
            Self::Empty => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Stream(incoming) => incoming.is_end_stream(),
            Self::Empty => true,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Stream(incoming) => incoming.size_hint(),
            Self::Empty => SizeHint::with_exact(0),
        }
    }
}

/// The forwarding state shared by every connection.
pub struct Relay {
    config: Config,
    client: Client<HttpConnector, RelayBody>,
}

fn status(code: StatusCode) -> Response<RelayBody> {
    let mut response = Response::new(RelayBody::Empty);
    *response.status_mut() = code;
    response
}

impl Relay {
    /// A relay forwarding as `config` says. It connects to the gateway only when a request
    /// arrives.
    #[must_use]
    pub fn new(config: Config) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        connector.set_connect_timeout(Some(config.response_timeout));
        let client = Client::builder(TokioExecutor::new())
            .http2_only(true)
            .build(connector);
        Self { config, client }
    }

    /// Answers one client request. Never fails: problems become status codes, and nothing
    /// about them is logged.
    pub(crate) async fn handle(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<RelayBody>, Infallible> {
        let path = request.uri().path();
        let response = if path == self.config.relay_path {
            if request.method() == Method::POST {
                self.forward_encapsulated(request).await
            } else {
                status(StatusCode::METHOD_NOT_ALLOWED)
            }
        } else if path == self.config.key_config_path {
            match (
                request.method() == Method::GET,
                &self.config.key_config_upstream,
            ) {
                (true, Some(target)) => self.forward_key_config(target.clone()).await,
                (true, None) => status(StatusCode::NOT_FOUND),
                (false, _) => status(StatusCode::METHOD_NOT_ALLOWED),
            }
        } else {
            status(StatusCode::NOT_FOUND)
        };
        Ok(response)
    }

    async fn forward_encapsulated(&self, request: Request<Incoming>) -> Response<RelayBody> {
        let media_type = match request.headers().get(CONTENT_TYPE).map(HeaderValue::to_str) {
            Some(Ok(OHTTP_REQUEST)) => OHTTP_REQUEST,
            Some(Ok(OHTTP_CHUNKED_REQUEST)) => OHTTP_CHUNKED_REQUEST,
            _ => return status(StatusCode::UNSUPPORTED_MEDIA_TYPE),
        };
        let mut upstream = Request::new(RelayBody::Stream(request.into_body()));
        *upstream.method_mut() = Method::POST;
        upstream.uri_mut().clone_from(&self.config.gateway);
        upstream
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(media_type));
        self.send(upstream, &[OHTTP_RESPONSE, OHTTP_CHUNKED_RESPONSE])
            .await
    }

    async fn forward_key_config(&self, target: Uri) -> Response<RelayBody> {
        let mut upstream = Request::new(RelayBody::Empty);
        *upstream.uri_mut() = target;
        self.send(upstream, &[OHTTP_KEYS]).await
    }

    /// Sends a rebuilt request to the gateway and rebuilds its response the same way: the
    /// status, the media type if it is one of `media_types`, and the body as it streams.
    async fn send(
        &self,
        upstream: Request<RelayBody>,
        media_types: &[&str],
    ) -> Response<RelayBody> {
        let sent =
            tokio::time::timeout(self.config.response_timeout, self.client.request(upstream));
        let gateway_response = match sent.await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return status(StatusCode::BAD_GATEWAY),
            Err(_) => return status(StatusCode::GATEWAY_TIMEOUT),
        };
        let code = gateway_response.status();
        let media_type = gateway_response
            .headers()
            .get(CONTENT_TYPE)
            .filter(|value| value.to_str().is_ok_and(|text| media_types.contains(&text)))
            .cloned();
        let mut response = Response::new(RelayBody::Stream(gateway_response.into_body()));
        *response.status_mut() = code;
        if let Some(media_type) = media_type {
            response.headers_mut().insert(CONTENT_TYPE, media_type);
        }
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

/// Accepts HTTP/2 connections (prior knowledge, no TLS) until `shutdown` completes, then
/// stops accepting and drops the connections it owns.
///
/// # Errors
///
/// None today: a failed accept affects only that connection. The `Result` leaves room for
/// listener failures that should stop the relay.
pub async fn serve(
    listener: TcpListener,
    relay: Arc<Relay>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                // A failed accept (a client that left, or too many open files) affects only
                // that connection.
                let Ok((stream, _address)) = accepted else {
                    continue;
                };
                // Live frames are small; without this each one waits on Nagle and delayed
                // acknowledgements (about 40 ms per frame). The upstream connector sets it too.
                let _ = stream.set_nodelay(true);
                // The client address is dropped here and never used again.
                let relay = Arc::clone(&relay);
                connections.spawn(async move {
                    let service = hyper::service::service_fn(move |request| {
                        Arc::clone(&relay).handle(request)
                    });
                    // A broken connection only ends that connection; it is not logged. No
                    // `date` header: the relay adds nothing to responses but `no-store`.
                    let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .auto_date_header(false)
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
    Ok(())
}
