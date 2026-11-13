// SPDX-License-Identifier: AGPL-3.0-only
//! The HTTP surface. GET and HEAD only (405 otherwise):
//!
//! - `/catalog.json`: generated ([`crate::catalog`]), `Cache-Control: public, max-age=300`.
//! - `/<stem>.json`, `/<stem>.pmtiles`, `/<stem>.places.tsv` of an allow-listed region: the
//!   file, with a strong entity tag (sha256 of the bytes), `If-None-Match` (304), single byte ranges
//!   (206, or 416 with `Content-Range: bytes */<len>`; several ranges get the whole file),
//!   `If-Range`, and long-lived caching for packs and indexes.
//! - In cache mode, a listed region not on disk yet: 503 with `Retry-After` and
//!   `{"status":"preparing","retry_after":N}` while it is cut.
//! - Anything else: 404 with no body.
//!
//! No cookies are set and no request detail is kept; see [`crate::counts`].

use std::io::{Seek as _, SeekFrom};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{
    ACCEPT_RANGES, ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
    IF_NONE_MATCH, IF_RANGE, RANGE, RETRY_AFTER, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use tokio::io::{AsyncRead, AsyncReadExt as _, ReadBuf, Take};
use tokio_util::io::ReaderStream;

use crate::cache::{Cache, ServingGuard};
use crate::catalog;
use crate::counts::Counts;
use crate::files::{Directory, OpenError, OpenedFile, etag_of};
use crate::range::{RangeAnswer, parse_range};
use crate::region::{AllowList, FileKind, RegionFile};

/// Read size when streaming a file.
const CHUNK: usize = 64 * 1024;

/// What the handlers share.
pub struct Host {
    /// The served directory.
    pub directory: Directory,
    /// Hosted regions.
    pub allow: AllowList,
    /// Aggregate counters.
    pub counts: Arc<Counts>,
    /// Cache mode, if on.
    pub cache: Option<Arc<Cache>>,
    /// `Retry-After` for regions being prepared.
    pub retry_after_seconds: u32,
}

/// The router for a host.
pub fn router(host: Arc<Host>) -> Router {
    Router::new().fallback(handle).with_state(host)
}

async fn handle(
    State(host): State<Arc<Host>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let (response, body_bytes) = respond(&host, &method, &uri, &headers).await;
    host.counts.record(response.status(), body_bytes);
    response
}

async fn respond(
    host: &Arc<Host>,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> (Response, u64) {
    let head = if method == Method::GET {
        false
    } else if method == Method::HEAD {
        true
    } else {
        return (
            (
                StatusCode::METHOD_NOT_ALLOWED,
                [(ALLOW, HeaderValue::from_static("GET, HEAD"))],
            )
                .into_response(),
            0,
        );
    };
    let Some(name) = uri.path().strip_prefix('/') else {
        return not_found();
    };
    if name == "catalog.json" {
        return catalog_response(host, head, headers).await;
    }
    let Some(file) = RegionFile::parse(name) else {
        return not_found();
    };
    if !host.allow.allows(&file.region) {
        return not_found();
    }
    if let Some(cache) = &host.cache {
        if !cache.lists(&file.region) {
            return not_found();
        }
        if cache.is_preparing(&file.region) {
            return preparing(host.retry_after_seconds);
        }
    }
    let directory = host.directory.clone();
    let wanted = file.clone();
    let opened = match tokio::task::spawn_blocking(move || directory.open(&wanted)).await {
        Ok(Ok(opened)) => opened,
        Ok(Err(OpenError::NotFound)) => {
            return match &host.cache {
                Some(cache) => {
                    cache.start_cut(&file.region);
                    preparing(host.retry_after_seconds)
                }
                None => not_found(),
            };
        }
        Ok(Err(OpenError::Io(_))) | Err(_) => {
            return status_only(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let guard = host.cache.as_ref().map(|cache| cache.serving(&file.region));
    file_response(
        Served {
            opened,
            kind: file.kind,
            guard,
        },
        head,
        headers,
    )
}

fn status_only(status: StatusCode) -> (Response, u64) {
    (status.into_response(), 0)
}

fn not_found() -> (Response, u64) {
    status_only(StatusCode::NOT_FOUND)
}

fn preparing(retry_after: u32) -> (Response, u64) {
    let body = format!("{{\"status\":\"preparing\",\"retry_after\":{retry_after}}}");
    (
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                (RETRY_AFTER, HeaderValue::from(retry_after)),
                (CONTENT_TYPE, HeaderValue::from_static("application/json")),
                (CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            body,
        )
            .into_response(),
        0,
    )
}

async fn catalog_response(host: &Arc<Host>, head: bool, headers: &HeaderMap) -> (Response, u64) {
    let state = Arc::clone(host);
    let built = tokio::task::spawn_blocking(move || {
        catalog::build(&state.directory, &state.allow, state.cache.as_deref())
    })
    .await;
    let Ok(Ok(body)) = built else {
        return status_only(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let Ok(etag) = HeaderValue::from_str(&etag_of(&body)) else {
        return status_only(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let common = [
        (CONTENT_TYPE, HeaderValue::from_static("application/json")),
        (
            CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=300"),
        ),
        (X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        (ETAG, etag.clone()),
    ];
    if none_match(headers, &etag) {
        return respond_with(
            StatusCode::NOT_MODIFIED,
            &Headers {
                common: &common,
                extra: &[],
            },
            Body::empty(),
            0,
        );
    }
    let len = u64::try_from(body.len()).unwrap_or(u64::MAX);
    let (body, sent) = if head {
        (Body::empty(), 0)
    } else {
        (Body::from(body), len)
    };
    respond_with(
        StatusCode::OK,
        &Headers {
            common: &common,
            extra: &[(CONTENT_LENGTH, HeaderValue::from(len))],
        },
        body,
        sent,
    )
}

/// A file about to be answered.
struct Served {
    opened: OpenedFile,
    kind: FileKind,
    /// Keeps the region marked as being served while the body streams (cache mode).
    guard: Option<ServingGuard>,
}

fn file_response(served: Served, head: bool, headers: &HeaderMap) -> (Response, u64) {
    let Served {
        opened: OpenedFile {
            mut file,
            len,
            etag,
        },
        kind,
        guard,
    } = served;
    let Ok(etag) = HeaderValue::from_str(&etag) else {
        return status_only(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let common = [
        (CONTENT_TYPE, HeaderValue::from_static(kind.content_type())),
        (
            CACHE_CONTROL,
            HeaderValue::from_static(kind.cache_control()),
        ),
        (ACCEPT_RANGES, HeaderValue::from_static("bytes")),
        (X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        (ETAG, etag.clone()),
    ];
    if none_match(headers, &etag) {
        return respond_with(
            StatusCode::NOT_MODIFIED,
            &Headers {
                common: &common,
                extra: &[],
            },
            Body::empty(),
            0,
        );
    }
    let range = match headers.get(RANGE).and_then(|value| value.to_str().ok()) {
        Some(value) if if_range_holds(headers, &etag) => parse_range(value, len),
        Some(_) | None => RangeAnswer::Full,
    };
    let (status, start, count, content_range) = match range {
        RangeAnswer::Full => (StatusCode::OK, 0, len, None),
        RangeAnswer::Partial(range) => (
            StatusCode::PARTIAL_CONTENT,
            range.start,
            range.len(),
            Some(format!("bytes {}-{}/{len}", range.start, range.end)),
        ),
        RangeAnswer::Unsatisfiable => {
            let Ok(content_range) = HeaderValue::from_str(&format!("bytes */{len}")) else {
                return status_only(StatusCode::INTERNAL_SERVER_ERROR);
            };
            return respond_with(
                StatusCode::RANGE_NOT_SATISFIABLE,
                &Headers {
                    common: &common,
                    extra: &[(CONTENT_RANGE, content_range)],
                },
                Body::empty(),
                0,
            );
        }
    };
    let mut extra = vec![(CONTENT_LENGTH, HeaderValue::from(count))];
    if let Some(content_range) = content_range {
        let Ok(value) = HeaderValue::from_str(&content_range) else {
            return status_only(StatusCode::INTERNAL_SERVER_ERROR);
        };
        extra.push((CONTENT_RANGE, value));
    }
    if head {
        return respond_with(
            status,
            &Headers {
                common: &common,
                extra: &extra,
            },
            Body::empty(),
            0,
        );
    }
    if file.seek(SeekFrom::Start(start)).is_err() {
        return status_only(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let reader = Guarded {
        inner: tokio::fs::File::from_std(file).take(count),
        _guard: guard,
    };
    let body = Body::from_stream(ReaderStream::with_capacity(reader, CHUNK));
    respond_with(
        status,
        &Headers {
            common: &common,
            extra: &extra,
        },
        body,
        count,
    )
}

/// The headers of an answer: those every answer for the resource carries, and per-answer ones.
struct Headers<'a> {
    common: &'a [(HeaderName, HeaderValue)],
    extra: &'a [(HeaderName, HeaderValue)],
}

fn respond_with(
    status: StatusCode,
    headers: &Headers<'_>,
    body: Body,
    body_bytes: u64,
) -> (Response, u64) {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let map = response.headers_mut();
    for (name, value) in headers.common.iter().chain(headers.extra) {
        map.insert(name.clone(), value.clone());
    }
    (response, body_bytes)
}

/// Whether `If-None-Match` names `etag` (weak comparison, as RFC 9110 §13.1.2 says) or is `*`.
fn none_match(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    let Some(etag) = etag.to_str().ok() else {
        return false;
    };
    headers.get_all(IF_NONE_MATCH).iter().any(|value| {
        value.to_str().is_ok_and(|list| {
            list.split(',').map(str::trim).any(|candidate| {
                candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
            })
        })
    })
}

/// Whether a `Range` should be honoured under `If-Range`: no `If-Range`, or one that is
/// exactly our strong entity tag. A date or any other tag means "send the whole file".
fn if_range_holds(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    headers
        .get(IF_RANGE)
        .is_none_or(|value| value.as_bytes() == etag.as_bytes())
}

/// A file reader that keeps a [`ServingGuard`] alive until the body is finished or dropped.
struct Guarded {
    inner: Take<tokio::fs::File>,
    _guard: Option<ServingGuard>,
}

impl AsyncRead for Guarded {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(HeaderName, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(name.clone(), HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn if_none_match_compares_weakly_and_takes_lists() {
        let etag = HeaderValue::from_static("\"abc\"");
        assert!(none_match(&headers(&[(IF_NONE_MATCH, "\"abc\"")]), &etag));
        assert!(none_match(&headers(&[(IF_NONE_MATCH, "W/\"abc\"")]), &etag));
        assert!(none_match(
            &headers(&[(IF_NONE_MATCH, "\"x\", \"abc\"")]),
            &etag
        ));
        assert!(none_match(&headers(&[(IF_NONE_MATCH, "*")]), &etag));
        assert!(!none_match(&headers(&[(IF_NONE_MATCH, "\"x\"")]), &etag));
        assert!(!none_match(&HeaderMap::new(), &etag));
    }

    #[test]
    fn if_range_compares_strongly() {
        let etag = HeaderValue::from_static("\"abc\"");
        assert!(if_range_holds(&HeaderMap::new(), &etag));
        assert!(if_range_holds(&headers(&[(IF_RANGE, "\"abc\"")]), &etag));
        assert!(!if_range_holds(&headers(&[(IF_RANGE, "W/\"abc\"")]), &etag));
        assert!(!if_range_holds(
            &headers(&[(IF_RANGE, "Wed, 21 Oct 2015 07:28:00 GMT")]),
            &etag
        ));
    }
}
