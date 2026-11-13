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

/// The value of header `name` (compared without case) as text.
pub(crate) fn header<'a>(request: &'a BinaryRequest, name: &str) -> Option<&'a str> {
    let header = request
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))?;
    core::str::from_utf8(&header.value).ok()
}

/// Exactly `N` bytes of hex from header `name`.
pub(crate) fn header_hex<const N: usize>(request: &BinaryRequest, name: &str) -> Option<[u8; N]> {
    hex_array(header(request, name)?)
}

/// RFC 4648 §5 base64url, with or without `=` padding; `None` for anything else.
pub(crate) fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    fn value(symbol: u8) -> Option<u32> {
        let index = match symbol {
            b'A'..=b'Z' => symbol.checked_sub(b'A')?,
            b'a'..=b'z' => symbol.checked_sub(b'a')?.checked_add(26)?,
            b'0'..=b'9' => symbol.checked_sub(b'0')?.checked_add(52)?,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        Some(u32::from(index))
    }
    let trimmed = text.trim_end_matches('=');
    if text.len().saturating_sub(trimmed.len()) > 2 {
        return None;
    }
    let symbols = trimmed.as_bytes();
    // One symbol left over carries only six bits: not a whole byte.
    if symbols.len() % 4 == 1 {
        return None;
    }
    let mut bytes = Vec::with_capacity(symbols.len().saturating_mul(3) / 4);
    for chunk in symbols.chunks(4) {
        let mut bits = 0_u32;
        for symbol in chunk {
            bits = (bits << 6) | value(*symbol)?;
        }
        let used = chunk.len().saturating_mul(6);
        let whole = used / 8;
        // The bits past the last whole byte must be zero (canonical encoding).
        let spare = used % 8;
        if bits & ((1_u32 << spare).saturating_sub(1)) != 0 {
            return None;
        }
        let aligned = bits >> spare;
        for index in (0..whole).rev() {
            let shift = u32::try_from(index.saturating_mul(8)).ok()?;
            bytes.push(u8::try_from((aligned >> shift) & 0xff).ok()?);
        }
    }
    Some(bytes)
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
            headers: Vec::new(),
            body,
        },
        Err(_) => Reply::status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Folds a handler's early refusal into its answer.
pub(crate) fn either(result: Result<Reply, Reply>) -> Reply {
    result.unwrap_or_else(|refusal| refusal)
}

/// RFC 4648 §5 base64url without padding, as clients send RLN proofs (tests only: the server
/// never encodes base64).
#[cfg(test)]
pub(crate) fn base64url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let symbol = |bits: u32| {
        usize::try_from(bits & 0x3f)
            .ok()
            .and_then(|index| ALPHABET.get(index))
            .map_or('A', |byte| char::from(*byte))
    };
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut group = [0_u8; 4];
        for (slot, byte) in group.iter_mut().skip(1).zip(chunk) {
            *slot = *byte;
        }
        let bits = u32::from_be_bytes(group);
        for shift in [18_u32, 12, 6, 0]
            .into_iter()
            .take(chunk.len().saturating_add(1))
        {
            out.push(symbol(bits >> shift));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64url_decode, base64url_encode, hex_array, query, segment};

    #[test]
    fn base64url_round_trips_every_length() {
        let bytes: Vec<u8> = (0..=255).collect();
        for length in 0..10 {
            let prefix = bytes.get(..length).unwrap_or_default();
            assert_eq!(
                base64url_decode(&base64url_encode(prefix)).as_deref(),
                Some(prefix)
            );
        }
        assert_eq!(base64url_encode(b"foob"), "Zm9vYg");
    }

    #[test]
    fn base64url_decodes_rfc_4648_vectors_padded_or_not() {
        for (text, bytes) in [
            ("", &b""[..]),
            ("Zg==", b"f"),
            ("Zg", b"f"),
            ("Zm8=", b"fo"),
            ("Zm9v", b"foo"),
            ("Zm9vYg", b"foob"),
            ("Zm9vYmE=", b"fooba"),
            ("Zm9vYmFy", b"foobar"),
            ("-_8=", &[0xfb, 0xff][..]),
        ] {
            assert_eq!(base64url_decode(text).as_deref(), Some(bytes), "{text}");
        }
    }

    #[test]
    fn base64url_refuses_other_alphabets_and_stray_bits() {
        // `+` and `/` are standard base64, not base64url; `Zh` has a set bit past the byte;
        // one symbol left over after whole groups is never a byte, even an `A`.
        for text in ["+/8=", "Zh", "Z", "Zg===", "Zm 9v", "QUFBA"] {
            assert_eq!(base64url_decode(text), None, "{text}");
        }
    }

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
