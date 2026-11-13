// SPDX-License-Identifier: AGPL-3.0-only
//! The HTTP surface (design D, "Push relay API"):
//!
//! - `GET /v1/config`: [`PushRelayConfig`] as CBOR (`application/cbor`), no auth, cacheable.
//! - `POST /v1/push`: `Authorization: Bearer <server credential>`, body [`PushRequest`] as CBOR.
//!   Answers 202, 400, 401, 410 (delete the blob), 422, 429, 500, 501, 502 or 504, with no
//!   body.
//!
//! Both bodies are pingers-proto types; the design (D) says bodies are CBOR unless noted.
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
use pingers_proto::WireEncode as _;
use pingers_time::Clock;

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
        .route("/v1/push", post(push::<O, S, C>))
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
    let Some(config) = relay.published_config() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
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
