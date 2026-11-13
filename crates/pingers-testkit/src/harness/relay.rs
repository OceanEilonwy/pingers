// SPDX-License-Identifier: AGPL-3.0-only
//! A forward-only OHTTP relay for the harness, behaving like pingers-ohttp-relay: one gateway,
//! requests rebuilt with only the body and its media type, responses with only the status,
//! the OHTTP media type and `cache-control: no-store`, bodies streamed frame by frame in both
//! directions (so chunked exchanges are full duplex), nothing logged.
//!
//! TODO(wave 2 follow-up): pingers-ohttp-relay is binary-only on main, so the harness cannot
//! call it. Once it exposes `serve(config, listener, .., shutdown)` as a library, replace
//! this module with that call; the harness API does not change.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
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

const OHTTP_REQUEST: &str = "message/ohttp-req";
const OHTTP_CHUNKED_REQUEST: &str = "message/ohttp-chunked-req";
const OHTTP_RESPONSE: &str = "message/ohttp-res";
const OHTTP_CHUNKED_RESPONSE: &str = "message/ohttp-chunked-res";
const OHTTP_KEYS: &str = "application/ohttp-keys";
const RELAY_PATH: &str = "/ohttp";
const KEY_CONFIG_PATH: &str = "/v1/ohttp/config";

/// A body passed through from the other side, or none.
enum PassBody {
    Stream(Incoming),
    Empty,
}

impl Body for PassBody {
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

struct Forwarder {
    gateway: Uri,
    key_config: Uri,
    client: Client<HttpConnector, PassBody>,
}

fn status(code: StatusCode) -> Response<PassBody> {
    let mut response = Response::new(PassBody::Empty);
    *response.status_mut() = code;
    response
}

impl Forwarder {
    async fn handle(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<PassBody>, Infallible> {
        let response = match (request.uri().path(), request.method()) {
            (RELAY_PATH, &Method::POST) => self.forward_encapsulated(request).await,
            (KEY_CONFIG_PATH, &Method::GET) => {
                let mut upstream = Request::new(PassBody::Empty);
                upstream.uri_mut().clone_from(&self.key_config);
                self.send(upstream, &[OHTTP_KEYS]).await
            }
            (RELAY_PATH | KEY_CONFIG_PATH, _) => status(StatusCode::METHOD_NOT_ALLOWED),
            _ => status(StatusCode::NOT_FOUND),
        };
        Ok(response)
    }

    async fn forward_encapsulated(&self, request: Request<Incoming>) -> Response<PassBody> {
        let media_type = match request.headers().get(CONTENT_TYPE).map(HeaderValue::to_str) {
            Some(Ok(OHTTP_REQUEST)) => OHTTP_REQUEST,
            Some(Ok(OHTTP_CHUNKED_REQUEST)) => OHTTP_CHUNKED_REQUEST,
            _ => return status(StatusCode::UNSUPPORTED_MEDIA_TYPE),
        };
        let mut upstream = Request::new(PassBody::Stream(request.into_body()));
        *upstream.method_mut() = Method::POST;
        upstream.uri_mut().clone_from(&self.gateway);
        upstream
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(media_type));
        self.send(upstream, &[OHTTP_RESPONSE, OHTTP_CHUNKED_RESPONSE])
            .await
    }

    async fn send(&self, upstream: Request<PassBody>, media_types: &[&str]) -> Response<PassBody> {
        let Ok(gateway_response) = self.client.request(upstream).await else {
            return status(StatusCode::BAD_GATEWAY);
        };
        let code = gateway_response.status();
        let media_type = gateway_response
            .headers()
            .get(CONTENT_TYPE)
            .filter(|value| value.to_str().is_ok_and(|text| media_types.contains(&text)))
            .cloned();
        let mut response = Response::new(PassBody::Stream(gateway_response.into_body()));
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

/// Relays to the gateway at `gateway` until `shutdown` completes, then drops its connections.
pub(super) async fn serve(
    listener: TcpListener,
    gateway: SocketAddr,
    shutdown: impl Future<Output = ()>,
) {
    let (Ok(gateway_uri), Ok(key_config)) = (
        format!("http://{gateway}{RELAY_PATH}").parse::<Uri>(),
        format!("http://{gateway}{KEY_CONFIG_PATH}").parse::<Uri>(),
    ) else {
        return;
    };
    let mut connector = HttpConnector::new();
    connector.set_nodelay(true);
    let forwarder = Arc::new(Forwarder {
        gateway: gateway_uri,
        key_config,
        client: Client::builder(TokioExecutor::new())
            .http2_only(true)
            .build(connector),
    });
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _address)) = accepted else {
                    continue;
                };
                let forwarder = Arc::clone(&forwarder);
                connections.spawn(async move {
                    let service = hyper::service::service_fn(move |request| {
                        Arc::clone(&forwarder).handle(request)
                    });
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
