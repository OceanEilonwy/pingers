// SPDX-License-Identifier: AGPL-3.0-only
//! Binary HTTP messages (RFC 9292), the inner requests and responses of OHTTP.
//!
//! Encoding uses the known-length form and omits trailing empty sections (RFC 9292 §3.8), so a
//! bare `200` response is the three bytes `01 40 c8`, as in RFC 9458 Appendix A. Decoding
//! accepts either form and trailing zero padding (RFC 9292 §3.8), and rejects what Pingers
//! never sends: informational responses, trailers, and response messages where a request is
//! expected (or the reverse).

use std::io::Cursor;

use crate::OhttpError;

/// The scheme written into every request's control data. Gateways route on the path only.
const SCHEME: &[u8] = b"https";

/// One header field.
#[derive(Clone, PartialEq, Eq)]
pub struct Header {
    /// Lower-case field name.
    pub name: String,
    /// Field value.
    pub value: Vec<u8>,
}

impl core::fmt::Debug for Header {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Values may be credentials (Authorization); print names only.
        formatter
            .debug_struct("Header")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for BinaryRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("BinaryRequest")
            .field("method", &self.method)
            .field("headers", &self.headers)
            .field("body_length", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for BinaryResponse {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("BinaryResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body_length", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// An HTTP status code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StatusCode(u16);

impl StatusCode {
    /// Checks the range 100 to 599.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] outside that range.
    pub const fn new(code: u16) -> Result<Self, OhttpError> {
        if code >= 100 && code <= 599 {
            Ok(Self(code))
        } else {
            Err(OhttpError::BadMessage)
        }
    }

    /// The code.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// An inner request: method, path (with query), headers, body. `Debug` prints the method,
/// header names and body length only: paths name groups and mailboxes, and bodies are
/// message contents (style guide §9).
#[derive(Clone, PartialEq, Eq)]
pub struct BinaryRequest {
    /// Upper-case method.
    pub method: String,
    /// Path and query, for example `/v1/board/sync`.
    pub path: String,
    /// Header fields, including the anonymous credential.
    pub headers: Vec<Header>,
    /// The body.
    pub body: Vec<u8>,
}

impl BinaryRequest {
    /// The known-length Binary HTTP encoding.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] if the method is empty, the path does not start with `/`, or
    /// a header name is empty or not lower-case.
    pub fn encode(&self) -> Result<Vec<u8>, OhttpError> {
        check_request_line(&self.method, &self.path)?;
        let mut message = bhttp::Message::request(
            self.method.as_bytes().to_vec(),
            SCHEME.to_vec(),
            Vec::new(),
            self.path.as_bytes().to_vec(),
        );
        write_message(&mut message, &self.headers, &self.body)
    }

    /// Parses a Binary HTTP request.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] if it is malformed, is a response, carries trailers, has
    /// non-zero bytes after the message, or breaks a rule [`BinaryRequest::encode`] enforces.
    pub fn decode(bytes: &[u8]) -> Result<Self, OhttpError> {
        let message = read_message(bytes)?;
        let bhttp::ControlData::Request { method, path, .. } = message.control() else {
            return Err(OhttpError::BadMessage);
        };
        let request = Self {
            method: text(method)?,
            path: text(path)?,
            headers: headers(message.header())?,
            body: message.content().to_vec(),
        };
        check_request_line(&request.method, &request.path)?;
        Ok(request)
    }
}

/// An inner response: status, headers, body. `Debug` prints the status, header names and
/// body length only.
#[derive(Clone, PartialEq, Eq)]
pub struct BinaryResponse {
    /// The status.
    pub status: StatusCode,
    /// Header fields.
    pub headers: Vec<Header>,
    /// The body.
    pub body: Vec<u8>,
}

impl BinaryResponse {
    /// The known-length Binary HTTP encoding.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] if the status is informational (1xx), which a final response
    /// cannot carry, or a header name is empty or not lower-case.
    pub fn encode(&self) -> Result<Vec<u8>, OhttpError> {
        let status =
            bhttp::StatusCode::try_from(self.status.get()).map_err(|_| OhttpError::BadMessage)?;
        if status.informational() {
            return Err(OhttpError::BadMessage);
        }
        let mut message = bhttp::Message::response(status);
        write_message(&mut message, &self.headers, &self.body)
    }

    /// Parses a Binary HTTP response.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] if it is malformed, is a request, includes informational
    /// responses or trailers, or has non-zero bytes after the message.
    pub fn decode(bytes: &[u8]) -> Result<Self, OhttpError> {
        let message = read_message(bytes)?;
        let bhttp::ControlData::Response(status) = message.control() else {
            return Err(OhttpError::BadMessage);
        };
        if !message.informational().is_empty() {
            return Err(OhttpError::BadMessage);
        }
        Ok(Self {
            status: StatusCode::new(status.code())?,
            headers: headers(message.header())?,
            body: message.content().to_vec(),
        })
    }
}

fn check_request_line(method: &str, path: &str) -> Result<(), OhttpError> {
    if method.is_empty() || !path.starts_with('/') {
        return Err(OhttpError::BadMessage);
    }
    Ok(())
}

fn valid_name(name: &[u8]) -> bool {
    !name.is_empty() && !name.iter().any(u8::is_ascii_uppercase)
}

/// Adds headers and body, writes the known-length form and drops the trailing empty sections.
fn write_message(
    message: &mut bhttp::Message,
    fields: &[Header],
    body: &[u8],
) -> Result<Vec<u8>, OhttpError> {
    for field in fields {
        if !valid_name(field.name.as_bytes()) {
            return Err(OhttpError::BadMessage);
        }
        message.put_header(field.name.as_bytes(), field.value.as_slice());
    }
    message.write_content(body);
    let mut encoded = Vec::new();
    message
        .write_bhttp(bhttp::Mode::KnownLength, &mut encoded)
        .map_err(|_| OhttpError::BadMessage)?;
    // The last byte is the empty trailer section's zero length; before it, an empty body's,
    // and before that, an empty header section's (RFC 9292 §3.8 allows omitting all three).
    let omitted: usize = match (fields.is_empty(), body.is_empty()) {
        (true, true) => 3,
        (false, true) => 2,
        (_, false) => 1,
    };
    let kept = encoded
        .len()
        .checked_sub(omitted)
        .ok_or(OhttpError::Internal)?;
    encoded.truncate(kept);
    Ok(encoded)
}

fn read_message(bytes: &[u8]) -> Result<bhttp::Message, OhttpError> {
    let mut cursor = Cursor::new(bytes);
    let message = bhttp::Message::read_bhttp(&mut cursor).map_err(|_| OhttpError::BadMessage)?;
    let consumed = usize::try_from(cursor.position()).map_err(|_| OhttpError::BadMessage)?;
    let padding = bytes.get(consumed..).ok_or(OhttpError::BadMessage)?;
    if padding.iter().any(|byte| *byte != 0) || !message.trailer().is_empty() {
        return Err(OhttpError::BadMessage);
    }
    Ok(message)
}

fn text(bytes: &[u8]) -> Result<String, OhttpError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| OhttpError::BadMessage)
}

fn headers(section: &bhttp::Header) -> Result<Vec<Header>, OhttpError> {
    section
        .fields()
        .iter()
        .map(|field| {
            if !valid_name(field.name()) {
                return Err(OhttpError::BadMessage);
            }
            Ok(Header {
                name: text(field.name())?,
                value: field.value().to_vec(),
            })
        })
        .collect()
}
