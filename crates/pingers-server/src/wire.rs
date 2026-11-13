// SPDX-License-Identifier: AGPL-3.0-only
//! How the wired routes read requests and write answers.
//!
//! Bodies are pingers-proto's canonical CBOR (`application/cbor`), as design D says. Design D
//! leaves the textual encodings open, so until pingers-transport fixes them with its client:
//!
//! - identifiers in paths (`{id}`) are lowercase or uppercase hex of their bytes;
//! - the board cursor is `?c=<hex>`, a sequence number `?after=<decimal>`;
//! - a mailbox read key (auth mode Cap) travels in the `pingers-read-key` header as hex;
//! - `DELETE /v1/board/lease/{id}` carries the 32-byte revoke key as the raw body.

use axum::http::StatusCode;
use pingers_ohttp::BinaryRequest;
use pingers_proto::{WireDecode, WireEncode};

use crate::dispatch::Reply;

/// The media type of every CBOR body.
pub(crate) const CBOR: &str = "application/cbor";
/// The header carrying a mailbox read key.
pub(crate) const READ_KEY_HEADER: &str = "pingers-read-key";

/// The path without its query.
fn path_only(path: &str) -> &str {
    path.split_once('?').map_or(path, |(path, _query)| path)
}

/// Segment `index` of the path after `/v1/` (0 is the first).
pub(crate) fn segment(path: &str, index: usize) -> Option<&str> {
    path_only(path).strip_prefix("/v1/")?.split('/').nth(index)
}

/// The value of query parameter `name`, if present.
pub(crate) fn query<'a>(path: &'a str, name: &str) -> Option<&'a str> {
    let (_path, query) = path.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Exactly `N` bytes of hex.
pub(crate) fn hex_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    let mut bytes = [0_u8; N];
    hex::decode_to_slice(text, &mut bytes).ok()?;
    Some(bytes)
}

/// Exactly `N` bytes of hex from header `name`.
pub(crate) fn header_hex<const N: usize>(request: &BinaryRequest, name: &str) -> Option<[u8; N]> {
    let header = request.headers.iter().find(|header| header.name == name)?;
    hex_array(core::str::from_utf8(&header.value).ok()?)
}

/// The request body decoded as `T`, or a 400.
pub(crate) fn decode<T: WireDecode>(body: &[u8]) -> Result<T, Reply> {
    T::decode(body).map_err(|_| Reply::status(StatusCode::BAD_REQUEST))
}

/// `value` as a CBOR body with `status`; a 500 if it cannot be encoded.
pub(crate) fn encoded(status: StatusCode, value: &impl WireEncode) -> Reply {
    match value.encode() {
        Ok(body) => Reply {
            status,
            content_type: Some(CBOR),
            body,
        },
        Err(_) => Reply::status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Folds a handler's early refusal into its answer.
pub(crate) fn either(result: Result<Reply, Reply>) -> Reply {
    result.unwrap_or_else(|refusal| refusal)
}

#[cfg(test)]
mod tests {
    use super::{hex_array, query, segment};

    #[test]
    fn segments_count_from_after_the_version_and_ignore_the_query() {
        assert_eq!(segment("/v1/mailboxes/ab/msgs?after=3", 1), Some("ab"));
        assert_eq!(segment("/v1/mailboxes/ab/msgs?after=3", 2), Some("msgs"));
        assert_eq!(segment("/v1/mailboxes", 1), None);
        assert_eq!(segment("/v2/mailboxes/ab", 1), None);
    }

    #[test]
    fn query_parameters_are_found_by_exact_name() {
        assert_eq!(query("/v1/board/page?c=00ff", "c"), Some("00ff"));
        assert_eq!(query("/v1/x?a=1&after=7", "after"), Some("7"));
        assert_eq!(query("/v1/x?cc=1", "c"), None);
        assert_eq!(query("/v1/x?c", "c"), None);
        assert_eq!(query("/v1/x", "c"), None);
    }

    #[test]
    fn hex_must_have_exactly_the_length() {
        assert_eq!(hex_array::<2>("00ff"), Some([0, 0xff]));
        assert_eq!(hex_array::<2>("00"), None);
        assert_eq!(hex_array::<2>("00ff00"), None);
        assert_eq!(hex_array::<2>("zzzz"), None);
    }
}
