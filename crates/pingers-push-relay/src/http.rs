// SPDX-License-Identifier: AGPL-3.0-only
//! The HTTP surface (design D, "Push relay API"):
//!
//! - `GET /v1/config`: [`PushRelayConfig`] as CBOR (`application/cbor`), no auth, cacheable.
//!   503 if no key is active.
//! - `GET /v1/config/next`: the same for the key scheduled to activate next, so members and
//!   build-time pin sets can take up its hash before it is published (two-slot rotation). 404
//!   if none is scheduled.
//! - `POST /v1/push`: `Authorization: Bearer <server credential>`, body [`PushRequest`] as CBOR.
//!   Answers 202, 400, 401, 410 (delete the blob), 422, 429, 500, 501, 502 or 504, with no
//!   body.
//! - `GET /status`: JSON for an operator's status panel, never cached: `service`, `version`,
//!   `status` (`ok` or `no_active_key`), `operator`, `platforms`, `published_key_id`,
//!   `next_key_id` and `next_key_activates_at` (Unix seconds; `null` when none). No secrets and
//!   no counts.
//!
//! The CBOR bodies are pingers-proto types; the design (D) says bodies are CBOR unless noted.
//!
//! [`PushRelayConfig`]: pingers_proto::PushRelayConfig
//! [`PushRequest`]: pingers_proto::PushRequest

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pingers_proto::{PushPlatform, PushRelayConfig, PushRelayKeyId, WireEncode as _};
use pingers_time::{Clock, UnixSeconds};

use crate::delivery::PushSender;
use crate::opener::TokenOpener;
use crate::service::{PushOutcome, Relay};

/// Larger than any valid push request (about 1.1 kB), small enough that nobody can make the
/// relay buffer much.
const MAX_BODY: usize = 4096;

/// The router for a relay.
pub fn router<O: TokenOpener, S: PushSender, C: Clock + 'static>(
    relay: Arc<Relay<O, S, C>>,
) -> Router {
    Router::new()
        .route("/v1/config", get(config::<O, S, C>))
        .route("/v1/config/next", get(next_config::<O, S, C>))
        .route("/v1/push", post(push::<O, S, C>))
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
        Some(config) => config_response(&config),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn next_config<O: TokenOpener, S: PushSender, C: Clock>(
    State(relay): State<Arc<Relay<O, S, C>>>,
) -> Response {
    match relay.next_config() {
        Some(config) => config_response(&config),
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

fn config_response(config: &PushRelayConfig) -> Response {
    match config.encode() {
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
