// SPDX-License-Identifier: AGPL-3.0-only
//! The HTTP surface (design D, "Push relay API"):
//!
//! - `GET /v1/config`: [`PushRelayConfig`] as CBOR (`application/cbor`), no auth, cacheable.
//!   503 if no key is active.
//! - `GET /v1/config/next`: [`ScheduledPushRelayConfig`]: the config of the key scheduled to
//!   activate next and its activation time, so members and build-time pin sets can take up its
//!   hash before it is published, and devices switch to it on time (two-slot rotation). 404 if
//!   none is scheduled.
//! - `POST /v1/push`: `Authorization: Bearer <server credential>`, body [`PushRequest`] as CBOR.
//!   Answers 202, 400, 401, 410 (delete the blob), 422, 429, 500, 501, 502 or 504, with no
//!   body. With [`PushSenders::DirectOnly`] (`push_direct_only = true`), a request a reverse
//!   proxy forwarded gets 404 whatever its method, as if there were no such path.
//! - `GET /status`: JSON for an operator's status panel, never cached: `service`, `version`,
//!   `status` (`ok` or `no_active_key`), `operator`, `platforms`, `published_key_id`,
//!   `next_key_id` and `next_key_activates_at` (Unix seconds; `null` when none). No secrets and
//!   no counts.
//!
//! The CBOR bodies are pingers-proto types; the design (D) says bodies are CBOR unless noted.
//!
//! [`PushRelayConfig`]: pingers_proto::PushRelayConfig
//! [`PushRequest`]: pingers_proto::PushRequest
//! [`ScheduledPushRelayConfig`]: pingers_proto::ScheduledPushRelayConfig

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pingers_proto::{PushPlatform, PushRelayKeyId, WireEncode as _};
use pingers_time::{Clock, UnixSeconds};

use crate::delivery::PushSender;
use crate::opener::TokenOpener;
use crate::service::{PushOutcome, Relay};

/// Larger than any valid push request (about 1.1 kB), small enough that nobody can make the
/// relay buffer much.
const MAX_BODY: usize = 4096;

/// Who `POST /v1/push` is served to (`push_direct_only`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PushSenders {
    /// Any caller with a credential, directly or through a reverse proxy: a relay that servers
    /// on other hosts push to, through the TLS front in front of it. The default.
    #[default]
    Anywhere,
    /// Only callers that connect directly, such as a server on the same machine over
    /// loopback: a request a reverse proxy forwarded gets 404, as if there were no such path.
    /// For a relay whose public name is only for devices (`GET /v1/config`), as on an
    /// appliance, where no proxy in front hides the path.
    DirectOnly,
}

/// The headers a reverse proxy adds to every request it forwards: Caddy's `reverse_proxy` (as
/// monokulo-nodo and the dev stack run it) sets `X-Forwarded-For`, `X-Forwarded-Proto` and
/// `X-Forwarded-Host`, replacing whatever the client sent under those names; other proxies add
/// `Forwarded` (RFC 7239) or `Via` (RFC 9110 §7.6.3) instead.
const PROXY_HEADERS: [&str; 5] = [
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "forwarded",
    "via",
];

/// Whether a request came through a reverse proxy: it carries a header a proxy adds.
///
/// With [`PushSenders::DirectOnly`] the relay listens where only the machine's processes (on
/// an appliance: the proxy and the packages granted the port) connect, so a request from
/// outside arrives through the proxy, which adds these headers itself: a client cannot have
/// them left out by sending fewer. A request with none of them connected directly. A client
/// that adds one only refuses itself.
#[must_use]
pub fn through_proxy(headers: &HeaderMap) -> bool {
    PROXY_HEADERS.iter().any(|name| headers.contains_key(*name))
}

/// `404` for a forwarded request, before the route (or its `405`) answers.
async fn direct_only(request: Request, next: Next) -> Response {
    if through_proxy(request.headers()) {
        StatusCode::NOT_FOUND.into_response()
    } else {
        next.run(request).await
    }
}

/// The router for a relay.
pub fn router<O: TokenOpener, S: PushSender, C: Clock + 'static>(
    relay: Arc<Relay<O, S, C>>,
    senders: PushSenders,
) -> Router {
    let push_route = post(push::<O, S, C>);
    let push_route = match senders {
        PushSenders::Anywhere => push_route,
        PushSenders::DirectOnly => push_route.layer(axum::middleware::from_fn(direct_only)),
    };
    Router::new()
        .route("/v1/config", get(config::<O, S, C>))
        .route("/v1/config/next", get(next_config::<O, S, C>))
        .route("/v1/push", push_route)
        .route("/status", get(status::<O, S, C>))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(relay)
}

/// The status a push outcome is answered with.
#[must_use]
pub const fn status_of(outcome: PushOutcome) -> StatusCode {
    match outcome {
        PushOutcome::Accepted => StatusCode::ACCEPTED,
        PushOutcome::TokenGone => StatusCode::GONE,
        PushOutcome::DoesNotOpen => StatusCode::UNPROCESSABLE_ENTITY,
        PushOutcome::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        PushOutcome::Unauthorized => StatusCode::UNAUTHORIZED,
        PushOutcome::BadRequest => StatusCode::BAD_REQUEST,
        PushOutcome::PlatformUnsupported => StatusCode::NOT_IMPLEMENTED,
        PushOutcome::UpstreamFailed => StatusCode::BAD_GATEWAY,
        PushOutcome::UpstreamTimedOut => StatusCode::GATEWAY_TIMEOUT,
        PushOutcome::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn config<O: TokenOpener, S: PushSender, C: Clock>(
    State(relay): State<Arc<Relay<O, S, C>>>,
) -> Response {
    match relay.published_config() {
        Some(config) => cbor_response(config.encode()),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn next_config<O: TokenOpener, S: PushSender, C: Clock>(
    State(relay): State<Arc<Relay<O, S, C>>>,
) -> Response {
    match relay.next_config() {
        Some(config) => cbor_response(config.encode()),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn status<O: TokenOpener, S: PushSender, C: Clock>(
    State(relay): State<Arc<Relay<O, S, C>>>,
) -> Response {
    let status = relay.status();
    let body = serde_json::json!({
        "service": "pingers-push-relay",
        "version": env!("CARGO_PKG_VERSION"),
        "status": if status.published_key.is_some() { "ok" } else { "no_active_key" },
        "operator": status.operator,
        "platforms": status.platforms.iter().filter_map(|platform| platform_name(*platform)).collect::<Vec<_>>(),
        "published_key_id": status.published_key.map(PushRelayKeyId::get),
        "next_key_id": status.next_key.map(PushRelayKeyId::get),
        "next_key_activates_at": status.next_key_activates_at.map(UnixSeconds::get),
    });
    (
        [
            (CACHE_CONTROL, "no-store"),
            (CONTENT_TYPE, "application/json"),
        ],
        body.to_string(),
    )
        .into_response()
}

fn platform_name(platform: PushPlatform) -> Option<&'static str> {
    if platform == PushPlatform::Fcm {
        Some("fcm")
    } else if platform == PushPlatform::UnifiedPush {
        Some("unifiedpush")
    } else {
        None
    }
}

/// A config body, cacheable for an hour: a rotation is announced long before it happens.
fn cbor_response(encoded: Result<Vec<u8>, pingers_proto::ProtoError>) -> Response {
    match encoded {
        Ok(body) => (
            [
                (CACHE_CONTROL, "public, max-age=3600"),
                (CONTENT_TYPE, "application/cbor"),
            ],
            body,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// The credential in `Authorization: Bearer <credential>` (scheme case-insensitive).
fn bearer(headers: &HeaderMap) -> Option<&[u8]> {
    let value = headers.get(AUTHORIZATION)?.as_bytes();
    let (scheme, credential) = value.split_at_checked(7)?;
    scheme
        .eq_ignore_ascii_case(b"bearer ")
        .then_some(credential)
}

async fn push<O: TokenOpener, S: PushSender, C: Clock>(
    State(relay): State<Arc<Relay<O, S, C>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let outcome = relay.push(bearer(&headers), &body).await;
    let status = status_of(outcome);
    if outcome == PushOutcome::Unauthorized {
        (status, [(WWW_AUTHENTICATE, "Bearer")]).into_response()
    } else {
        status.into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers_with(authorization: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(value) = HeaderValue::from_str(authorization) {
            headers.insert(AUTHORIZATION, value);
        }
        headers
    }

    #[test]
    fn any_header_a_proxy_adds_marks_a_forwarded_request() {
        assert!(!through_proxy(&HeaderMap::new()));
        // What pingers-server sends on loopback is not a proxy's mark.
        let mut direct = headers_with("Bearer abc");
        direct.insert(CONTENT_TYPE, HeaderValue::from_static("application/cbor"));
        direct.insert("host", HeaderValue::from_static("127.0.0.1:9472"));
        assert!(!through_proxy(&direct));
        for name in PROXY_HEADERS {
            let mut forwarded = headers_with("Bearer abc");
            forwarded.insert(name, HeaderValue::from_static("x"));
            assert!(through_proxy(&forwarded), "{name}");
        }
    }

    #[test]
    fn bearer_reads_the_credential_after_the_scheme() {
        assert_eq!(bearer(&headers_with("Bearer abc")), Some(&b"abc"[..]));
        assert_eq!(bearer(&headers_with("bearer abc")), Some(&b"abc"[..]));
        assert_eq!(bearer(&headers_with("Basic abcdefgh")), None);
        assert_eq!(bearer(&headers_with("Bearer")), None);
        assert_eq!(bearer(&HeaderMap::new()), None);
    }

    #[test]
    fn every_outcome_has_the_documented_status() {
        let table = [
            (PushOutcome::Accepted, 202),
            (PushOutcome::TokenGone, 410),
            (PushOutcome::DoesNotOpen, 422),
            (PushOutcome::RateLimited, 429),
            (PushOutcome::Unauthorized, 401),
            (PushOutcome::BadRequest, 400),
            (PushOutcome::PlatformUnsupported, 501),
            (PushOutcome::UpstreamFailed, 502),
            (PushOutcome::UpstreamTimedOut, 504),
            (PushOutcome::Internal, 500),
        ];
        for (outcome, status) in table {
            assert_eq!(status_of(outcome).as_u16(), status, "{outcome:?}");
        }
    }
}
