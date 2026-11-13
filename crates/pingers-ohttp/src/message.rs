// SPDX-License-Identifier: AGPL-3.0-only
//! Binary HTTP messages (RFC 9292), the inner requests and responses of OHTTP.

use crate::OhttpError;
use crate::stub::not_implemented;

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

/// An inner request: method, path (with query), headers, body.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// [`OhttpError::BadMessage`] if a field cannot be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, OhttpError> {
        not_implemented(self)
    }

    /// Parses a Binary HTTP request.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] if it is malformed.
    pub fn decode(bytes: &[u8]) -> Result<Self, OhttpError> {
        not_implemented(bytes)
    }
}

/// An inner response: status, headers, body.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// [`OhttpError::BadMessage`] if a field cannot be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, OhttpError> {
        not_implemented(self)
    }

    /// Parses a Binary HTTP response.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] if it is malformed.
    pub fn decode(bytes: &[u8]) -> Result<Self, OhttpError> {
        not_implemented(bytes)
    }
}
