// SPDX-License-Identifier: AGPL-3.0-only
//! The OHTTP gateway at `POST /ohttp` (design B.12): only from configured relays; one-shot
//! requests (`message/ohttp-req`) are decapsulated and answered like direct ones, chunked
//! exchanges (`message/ohttp-chunked-req`) carry the live channel.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse as _, Response};
use pingers_ohttp::{ChunkedGateway, OhttpError};
use pingers_time::Timer as _;
use tokio::sync::mpsc;

use crate::dispatch::{Reply, answer};
use crate::live::{LIVE_BUFFER, LiveBody, OpenChannel, next_data};
use crate::route::{Channel, Resolved, Route, resolve};
use crate::{LiveRoute, Peer, Shared};

const OHTTP_REQUEST: &str = "message/ohttp-req";
const OHTTP_CHUNKED_REQUEST: &str = "message/ohttp-chunked-req";
const OHTTP_RESPONSE: &str = "message/ohttp-res";
const OHTTP_CHUNKED_RESPONSE: &str = "message/ohttp-chunked-res";

/// The largest one-shot request: a 32 KiB mailbox post (design D) plus headers, credentials
/// and encapsulation, with room to spare.
pub(crate) const MAX_ONE_SHOT: usize = 64 * 1024;
/// The largest chunked preamble: the encapsulation header plus one request-head chunk, which
/// pingers-ohttp caps at 16 KiB.
const MAX_PREAMBLE: usize = 20 * 1024;

/// Why a request body could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyError {
    /// Longer than allowed.
    TooLarge,
    /// Not complete within the request timeout.
    TimedOut,
    /// The connection broke.
    Broken,
}

impl BodyError {
    pub(crate) const fn status(self) -> StatusCode {
        match self {
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::TimedOut => StatusCode::REQUEST_TIMEOUT,
            Self::Broken => StatusCode::BAD_REQUEST,
        }
    }
}

/// Reads a whole body of at most `limit` bytes within the request timeout.
pub(crate) async fn read_body(
    shared: &Shared,
    body: &mut Body,
    limit: usize,
) -> Result<Vec<u8>, BodyError> {
    let deadline = shared.timer.now().saturating_add(shared.request_timeout);
    let mut all = Vec::new();
    loop {
        let next = tokio::select! {
            next = next_data(body) => next,
            () = shared.timer.sleep_until(deadline) => return Err(BodyError::TimedOut),
        };
        let Some(bytes) = next.map_err(|_| BodyError::Broken)? else {
            return Ok(all);
        };
        if bytes.len() > limit.saturating_sub(all.len()) {
            return Err(BodyError::TooLarge);
        }
        all.extend_from_slice(&bytes);
    }
}

fn status(shared: &Shared, channel: Channel, code: StatusCode) -> Response {
    shared.metrics.record(channel, code);
    code.into_response()
}

fn ohttp_response(media_type: &'static str, body: Body) -> Response {
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(media_type));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `POST /ohttp`.
pub(crate) async fn post_ohttp(
    State(shared): State<Arc<Shared>>,
    Extension(Peer(peer)): Extension<Peer>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let media_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let channel = match media_type {
        Some(OHTTP_CHUNKED_REQUEST) => Channel::ObliviousStream,
        _ => Channel::Oblivious,
    };
    if !relay_allowed(&shared, peer) {
        // A client that reached the gateway directly would have shown it its address.
        return status(&shared, channel, StatusCode::FORBIDDEN);
    }
    match media_type {
        Some(OHTTP_REQUEST) => one_shot(&shared, body).await,
        Some(OHTTP_CHUNKED_REQUEST) => chunked(&shared, body).await,
        _ => status(&shared, channel, StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

fn relay_allowed(shared: &Shared, peer: SocketAddr) -> bool {
    shared.relays.allows(peer.ip())
}

async fn one_shot(shared: &Arc<Shared>, mut body: Body) -> Response {
    let message = match read_body(shared, &mut body, MAX_ONE_SHOT).await {
        Ok(message) => message,
        Err(error) => return status(shared, Channel::Oblivious, error.status()),
    };
    // RFC 9458 §5.2: a request the gateway cannot open is a 400 to the relay.
    let Ok(decapsulated) = shared.gateway.decapsulate(&message) else {
        return status(shared, Channel::Oblivious, StatusCode::BAD_REQUEST);
    };
    let reply = answer(shared, Channel::Oblivious, &decapsulated.request).await;
    let inner_status = reply.status;
    let sealed = reply
        .into_binary()
        .and_then(|response| shared.with_rng(|rng| decapsulated.responder.seal(&response, rng)));
    match sealed {
        Ok(sealed) => {
            shared.metrics.record(Channel::Oblivious, inner_status);
            ohttp_response(OHTTP_RESPONSE, Body::from(sealed))
        }
        Err(_) => status(
            shared,
            Channel::Oblivious,
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

/// Reads until the encapsulation header and the request head have arrived.
async fn accept(shared: &Shared, body: &mut Body) -> Result<ChunkedGateway, StatusCode> {
    let deadline = shared.timer.now().saturating_add(shared.request_timeout);
    let mut preamble: Vec<u8> = Vec::new();
    loop {
        let next = tokio::select! {
            next = next_data(body) => next,
            () = shared.timer.sleep_until(deadline) => return Err(StatusCode::REQUEST_TIMEOUT),
        };
        let bytes: Bytes = next
            .map_err(|_| StatusCode::BAD_REQUEST)?
            .ok_or(StatusCode::BAD_REQUEST)?;
        if bytes.len() > MAX_PREAMBLE.saturating_sub(preamble.len()) {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        preamble.extend_from_slice(&bytes);
        match shared.with_rng(|rng| ChunkedGateway::accept(&shared.gateway, &preamble, rng)) {
            Ok(gateway) => return Ok(gateway),
            Err(OhttpError::Truncated) => {}
            Err(_) => return Err(StatusCode::BAD_REQUEST),
        }
    }
}

async fn chunked(shared: &Arc<Shared>, mut body: Body) -> Response {
    let gateway = match accept(shared, &mut body).await {
        Ok(gateway) => gateway,
        Err(code) => return status(shared, Channel::ObliviousStream, code),
    };
    let head = gateway.request();
    // A chunked response has no inner status, so anything but an open channel ends at once;
    // the counters still say why.
    let refused = match resolve(&head.method, &head.path) {
        Resolved::Found(Route::Live) => None,
        Resolved::Found(_) => Some(StatusCode::MISDIRECTED_REQUEST),
        Resolved::MethodNotAllowed => Some(StatusCode::METHOD_NOT_ALLOWED),
        Resolved::NotFound => Some(StatusCode::NOT_FOUND),
    };
    let live_body = if let Some(code) = refused {
        shared.metrics.record(Channel::ObliviousStream, code);
        LiveBody::closed(gateway)
    } else {
        let (inbound, inbound_receiver) = mpsc::channel(LIVE_BUFFER);
        let opened = match &shared.live {
            LiveRoute::Groups => {
                crate::live_groups::open(shared, head.clone(), inbound_receiver).await
            }
            LiveRoute::Custom(channel) => channel
                .open(head, inbound_receiver)
                .map_err(|_| StatusCode::NOT_IMPLEMENTED),
        };
        match opened {
            Ok(outbound) => {
                shared
                    .metrics
                    .record(Channel::ObliviousStream, StatusCode::OK);
                LiveBody::open(
                    gateway,
                    body,
                    OpenChannel {
                        inbound,
                        outbound,
                        timer: Arc::clone(&shared.timer),
                    },
                )
            }
            Err(code) => {
                shared.metrics.record(Channel::ObliviousStream, code);
                LiveBody::closed(gateway)
            }
        }
    };
    ohttp_response(OHTTP_CHUNKED_RESPONSE, Body::new(live_body))
}

/// A reply sent directly.
pub(crate) fn direct_response(reply: Reply) -> Response {
    let mut response = Response::new(Body::from(reply.body));
    *response.status_mut() = reply.status;
    if let Some(content_type) = reply.content_type {
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
    response
}
