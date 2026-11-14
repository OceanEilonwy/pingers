// SPDX-License-Identifier: AGPL-3.0-only
//! The map host's own OHTTP gateway (design B.12, "Downloads through the privacy relay").
//!
//! A phone with "Download maps through privacy relay" on sends each map request encapsulated
//! to this gateway's key. It travels through the user's OHTTP relay to the Pingers server,
//! whose gateway forwards the still-encapsulated bytes here (`POST /v1/maps/ohttp`). So:
//!
//! - the relay sees the phone's address and sizes, not the request;
//! - the Pingers server sees that some anonymous client fetched map data, and sizes, not which
//!   file: the request is sealed to this host's key, not the server's;
//! - this host sees which file, from the server's address, not the phone's.
//!
//! Routes:
//!
//! - `POST /ohttp` (`message/ohttp-req`): the inner request is a `GET` or `HEAD` of any path
//!   the host serves, answered exactly as a direct request would be ([`crate::http`]),
//!   sealed as `message/ohttp-res`. The encoded inner response is padded with zeros to a
//!   power of two of at least 1 KiB, or to the `pingers-pad` length the client asks for if
//!   that is larger (so every piece of a download has the same size); at most
//!   [`MAX_PADDED`]. A body that would not fit is answered `413` inside.
//! - `GET /ohttp-keys`: the key config (`application/ohttp-keys`).
//!
//! The key lives in `ohttp_key_file` (64 hex digits); the host makes one on first start.

use std::io::{self, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse as _, Response};
use pingers_ohttp::{BinaryResponse, Gateway, GatewayKey, Header, KeyConfig, OhttpError};
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::error::MapHostError;
use crate::http::{Host, respond};

/// The key id of the host's one gateway key.
const KEY_ID: u8 = 1;
/// The largest encapsulated request accepted: the client pads its inner requests to 1 KiB.
const MAX_REQUEST: usize = 8 * 1024;
/// The smallest padded response.
const MIN_PADDED: usize = 1024;
/// The largest padded response: what one relayed answer may carry (1 MiB, the client
/// transport's limit, less room for the encapsulation around it).
pub const MAX_PADDED: usize = 1024 * 1024 - 4 * 1024;
/// The request header naming the padded length a client wants.
pub const PAD_HEADER: &str = "pingers-pad";
/// Inner request headers passed on to the file handlers; anything else is dropped.
const PASSED: [&str; 3] = ["range", "if-range", "if-none-match"];
/// Inner response headers sealed back; anything else is dropped.
const RETURNED: [&str; 5] = [
    "content-type",
    "content-range",
    "etag",
    "retry-after",
    "accept-ranges",
];

/// The gateway: its key, and randomness for sealing responses.
pub struct RelayGateway {
    gateway: Gateway,
    config: KeyConfig,
    rng: Mutex<Box<dyn CryptoRng + Send>>,
}

impl RelayGateway {
    /// A gateway with `key`, sealing with `rng`.
    #[must_use]
    pub fn new(key: GatewayKey, rng: Box<dyn CryptoRng + Send>) -> Self {
        let config = key.config();
        Self {
            gateway: Gateway::new(vec![key]),
            config,
            rng: Mutex::new(rng),
        }
    }

    /// The public key config clients encapsulate to.
    #[must_use]
    pub const fn key_config(&self) -> &KeyConfig {
        &self.config
    }
}

/// Reads the gateway key from `path`, or makes one there with `rng` if the file does not
/// exist (readable by the owner only).
///
/// # Errors
///
/// [`MapHostError::OhttpKeyFile`] if the file cannot be read or written, or does not hold 64
/// hex digits; [`MapHostError::OhttpKey`] if they are not a usable key.
pub fn load_or_create_key(
    path: &Path,
    rng: &mut (impl CryptoRng + ?Sized),
) -> Result<GatewayKey, MapHostError> {
    let failed = |source: io::Error| MapHostError::OhttpKeyFile {
        path: path.to_owned(),
        source,
    };
    let secret = match std::fs::read_to_string(path) {
        Ok(text) => {
            let text = Zeroizing::new(text);
            let mut secret = Zeroizing::new([0_u8; 32]);
            hex::decode_to_slice(text.trim(), secret.as_mut_slice()).map_err(|_| {
                failed(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "not 64 hex digits",
                ))
            })?;
            secret
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut secret = Zeroizing::new([0_u8; 32]);
            rng.fill_bytes(secret.as_mut_slice());
            write_new_key(path, &secret).map_err(failed)?;
            secret
        }
        Err(error) => return Err(failed(error)),
    };
    GatewayKey::from_bytes(KEY_ID, secret).map_err(MapHostError::OhttpKey)
}

fn write_new_key(path: &Path, secret: &[u8; 32]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    let text = Zeroizing::new(format!("{}\n", hex::encode(secret)));
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// `GET /ohttp-keys`.
pub(crate) async fn key_config(State(host): State<Arc<Host>>) -> Response {
    let mut response = Response::new(Body::from(host.gateway.config.encoded().to_vec()));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/ohttp-keys"),
    );
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    response
}

/// `POST /ohttp`.
pub(crate) async fn post(
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if headers
        .get(CONTENT_TYPE)
        .is_none_or(|value| value.as_bytes() != b"message/ohttp-req")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(message) = to_bytes(body, MAX_REQUEST).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    // RFC 9458 §5.2: a request the gateway cannot open is a 400 to whoever forwarded it.
    let Ok(decapsulated) = host.gateway.gateway.decapsulate(&message) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let inner = decapsulated.request;
    let padded_to = requested_padding(&inner.headers);
    let answer = match inner_answer(&host, &inner).await {
        Ok(answer) => Ok(answer),
        Err(status) => status_answer(status),
    };
    let sealed =
        answer.and_then(|answer| seal(&host.gateway, decapsulated.responder, &answer, padded_to));
    match sealed {
        Ok(sealed) => {
            let mut response = Response::new(Body::from(sealed));
            let headers = response.headers_mut();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("message/ohttp-res"));
            headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// The padded length the client asked for, if any and not above [`MAX_PADDED`].
fn requested_padding(headers: &[Header]) -> usize {
    headers
        .iter()
        .find(|header| header.name == PAD_HEADER)
        .and_then(|header| std::str::from_utf8(&header.value).ok())
        .and_then(|text| text.trim().parse::<usize>().ok())
        .map_or(0, |length| length.min(MAX_PADDED))
}

/// What the host answers the inner request, as a Binary HTTP response.
async fn inner_answer(
    host: &Arc<Host>,
    inner: &pingers_ohttp::BinaryRequest,
) -> Result<BinaryResponse, StatusCode> {
    let method = match inner.method.as_str() {
        "GET" => Method::GET,
        "HEAD" => Method::HEAD,
        _ => return Err(StatusCode::METHOD_NOT_ALLOWED),
    };
    let uri: Uri = inner.path.parse().map_err(|_| StatusCode::NOT_FOUND)?;
    let mut headers = HeaderMap::new();
    for header in &inner.headers {
        if PASSED.contains(&header.name.as_str())
            && let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(header.name.as_bytes()),
                HeaderValue::from_bytes(&header.value),
            )
        {
            headers.append(name, value);
        }
    }
    let (response, body_bytes) = respond(host, &method, &uri, &headers).await;
    host.counts.record(response.status(), body_bytes);
    let (parts, body) = response.into_parts();
    // Anything larger than the largest padded answer could not be sealed back anyway.
    let body: Bytes = to_bytes(body, MAX_PADDED)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let status = pingers_ohttp::StatusCode::new(parts.status.as_u16())
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let headers = parts
        .headers
        .iter()
        .filter(|(name, _)| RETURNED.contains(&name.as_str()))
        .map(|(name, value)| Header {
            name: name.as_str().to_owned(),
            value: value.as_bytes().to_vec(),
        })
        .collect();
    Ok(BinaryResponse {
        status,
        headers,
        body: body.to_vec(),
    })
}

/// A body-less inner answer.
fn status_answer(status: StatusCode) -> Result<BinaryResponse, OhttpError> {
    Ok(BinaryResponse {
        status: pingers_ohttp::StatusCode::new(status.as_u16())?,
        headers: Vec::new(),
        body: Vec::new(),
    })
}

/// Seals `answer` padded to its size class, or answers 413 inside if it does not fit.
fn seal(
    gateway: &RelayGateway,
    responder: pingers_ohttp::Responder,
    answer: &BinaryResponse,
    requested: usize,
) -> Result<Vec<u8>, OhttpError> {
    let encoded = answer.encode()?.len();
    let mut rng = gateway.rng.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(length) = padded_length(encoded, requested) {
        return responder.seal_padded(answer, length, &mut *rng);
    }
    let too_large = status_answer(StatusCode::PAYLOAD_TOO_LARGE)?;
    let length =
        padded_length(too_large.encode()?.len(), requested).ok_or(OhttpError::LongerThanPadding)?;
    responder.seal_padded(&too_large, length, &mut *rng)
}

/// The length an encoded answer of `encoded` bytes is padded to: the next power of two from
/// [`MIN_PADDED`], or `requested` if larger, at most [`MAX_PADDED`]; `None` if it does not fit.
#[must_use]
pub fn padded_length(encoded: usize, requested: usize) -> Option<usize> {
    let class = encoded.max(MIN_PADDED).checked_next_power_of_two()?;
    let length = class.max(requested).min(MAX_PADDED);
    (encoded <= length).then_some(length)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn answers_pad_to_a_power_of_two_or_the_requested_length() {
        assert_eq!(padded_length(0, 0), Some(1024));
        assert_eq!(padded_length(1024, 0), Some(1024));
        assert_eq!(padded_length(1025, 0), Some(2048));
        assert_eq!(padded_length(5000, 0), Some(8192));
        // The requested length wins when larger, so every piece of a download is one size.
        assert_eq!(padded_length(300, 262_144), Some(262_144));
        assert_eq!(padded_length(262_144, 262_144), Some(262_144));
        assert_eq!(padded_length(262_145, 262_144), Some(524_288));
        // Never above the cap: the power of two above it is cut down, and a larger answer
        // does not fit.
        assert_eq!(padded_length(600_000, 0), Some(MAX_PADDED));
        assert_eq!(padded_length(MAX_PADDED, usize::MAX), Some(MAX_PADDED));
        assert_eq!(padded_length(MAX_PADDED + 1, 0), None);
    }

    #[test]
    fn padding_requests_are_read_from_the_header_and_capped() {
        let header = |value: &str| Header {
            name: PAD_HEADER.to_owned(),
            value: value.as_bytes().to_vec(),
        };
        assert_eq!(requested_padding(&[header("4096")]), 4096);
        assert_eq!(requested_padding(&[header(" 4096 ")]), 4096);
        assert_eq!(requested_padding(&[header("99999999")]), MAX_PADDED);
        assert_eq!(requested_padding(&[header("lots")]), 0);
        assert_eq!(requested_padding(&[]), 0);
    }

    #[test]
    fn a_key_file_is_made_once_and_read_back() -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("pingers-map-host-key-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(".ohttp-key");
        let _stale = std::fs::remove_file(&path);
        let made = load_or_create_key(&path, &mut pingers_testkit::seeded_rng(1))?;
        // A second start reads the same key, whatever the generator would draw.
        let read = load_or_create_key(&path, &mut pingers_testkit::seeded_rng(2))?;
        assert_eq!(made.config().encoded(), read.config().encoded());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&path, "not hex\n")?;
        assert!(matches!(
            load_or_create_key(&path, &mut pingers_testkit::seeded_rng(3)),
            Err(MapHostError::OhttpKeyFile { .. })
        ));
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }
}
