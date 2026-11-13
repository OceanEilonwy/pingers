// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy Pass issuance (design B.4.8, D: `POST /v1/pp/issue`, account-authenticated).
//!
//! The bodies carry pingers-anon's RFC 9578 batched issuance messages for token type 0x0001
//! (VOPRF P-384) as opaque byte strings, the TLS presentation inside the CBOR body:
//!
//! - request: `token_type (2) || truncated_token_key_id (1) || blinded_elements<0..2^16-1>`,
//!   each blinded element 49 bytes (`PendingIssuance::request` in pingers-anon);
//! - response: `evaluated_elements<0..2^16-1> || proof (96)` (`issue_tokens`), which the
//!   client hands to `PendingIssuance::finish`.
//!
//! This crate checks only that each fits a batch of [`MAX_TOKENS_PER_ISSUANCE`] tokens;
//! pingers-anon parses and verifies the contents.

use minicbor::Decoder;

use crate::ProtoError;
use crate::cbor::{CborBody, Writer, array_of, versioned_cbor};

/// The most tokens one issuance may carry (design B.4.8: a batch of up to 300 a day).
pub const MAX_TOKENS_PER_ISSUANCE: usize = 300;

/// A compressed P-384 element.
const ELEMENT_BYTES: usize = 49;
/// The batch DLEQ proof: two P-384 scalars.
const PROOF_BYTES: usize = 96;

/// The longest request: token type, truncated key id, vector length, 300 elements.
const MAX_REQUEST_BYTES: usize = 2 + 1 + 2 + MAX_TOKENS_PER_ISSUANCE * ELEMENT_BYTES;
/// The longest response: vector length, 300 elements, proof.
const MAX_RESPONSE_BYTES: usize = 2 + MAX_TOKENS_PER_ISSUANCE * ELEMENT_BYTES + PROOF_BYTES;

fn checked(bytes: Vec<u8>, max: usize, field: &'static str) -> Result<Vec<u8>, ProtoError> {
    if bytes.is_empty() || bytes.len() > max {
        Err(ProtoError::OutOfRange { field })
    } else {
        Ok(bytes)
    }
}

/// `POST /v1/pp/issue` (design D, `IssueReq`): a batched token request.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivacyPassIssueRequest(Vec<u8>);

impl PrivacyPassIssueRequest {
    /// Wraps pingers-anon's encoded batched token request.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if it is empty or longer than a request for
    /// [`MAX_TOKENS_PER_ISSUANCE`] tokens.
    pub fn new(token_request: Vec<u8>) -> Result<Self, ProtoError> {
        checked(token_request, MAX_REQUEST_BYTES, "privacy pass request").map(Self)
    }

    /// The encoded batched token request, for pingers-anon's `issue_tokens`.
    #[must_use]
    pub fn token_request(&self) -> &[u8] {
        &self.0
    }
}

impl core::fmt::Debug for PrivacyPassIssueRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "PrivacyPassIssueRequest({} bytes)", self.0.len())
    }
}

/// The answer to `POST /v1/pp/issue`: the evaluated elements and the batch proof.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivacyPassIssueResponse(Vec<u8>);

impl PrivacyPassIssueResponse {
    /// Wraps pingers-anon's encoded batched token response.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if it is empty or longer than a response for
    /// [`MAX_TOKENS_PER_ISSUANCE`] tokens.
    pub fn new(token_response: Vec<u8>) -> Result<Self, ProtoError> {
        checked(token_response, MAX_RESPONSE_BYTES, "privacy pass response").map(Self)
    }

    /// The encoded batched token response, for pingers-anon's `PendingIssuance::finish`.
    #[must_use]
    pub fn token_response(&self) -> &[u8] {
        &self.0
    }
}

impl core::fmt::Debug for PrivacyPassIssueResponse {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "PrivacyPassIssueResponse({} bytes)",
            self.0.len()
        )
    }
}

/// Layout: `[token request]`.
impl CborBody for PrivacyPassIssueRequest {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(1)?;
        writer.bytes(&self.0)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        array_of(decoder, 1)?;
        Self::new(decoder.bytes()?.to_vec())
    }
}

/// Layout: `[token response]`.
impl CborBody for PrivacyPassIssueResponse {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(1)?;
        writer.bytes(&self.0)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        array_of(decoder, 1)?;
        Self::new(decoder.bytes()?.to_vec())
    }
}

versioned_cbor!(PrivacyPassIssueRequest, PrivacyPassIssueResponse);

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn a_full_batch_fits_and_one_more_element_does_not() {
        assert!(PrivacyPassIssueRequest::new(vec![1; 14_705]).is_ok());
        assert!(PrivacyPassIssueRequest::new(vec![1; 14_706]).is_err());
        assert!(PrivacyPassIssueResponse::new(vec![1; 14_798]).is_ok());
        assert!(PrivacyPassIssueResponse::new(vec![1; 14_799]).is_err());
    }

    #[test]
    fn the_wrapped_message_is_returned_unchanged() -> Result<(), ProtoError> {
        let bytes = vec![0, 1, 0x7f, 0, 49];
        assert_eq!(
            PrivacyPassIssueRequest::new(bytes.clone())?.token_request(),
            bytes.as_slice()
        );
        assert_eq!(
            PrivacyPassIssueResponse::new(bytes.clone())?.token_response(),
            bytes.as_slice()
        );
        Ok(())
    }

    #[test]
    fn empty_messages_are_refused() {
        assert!(matches!(
            PrivacyPassIssueRequest::new(Vec::new()),
            Err(ProtoError::OutOfRange {
                field: "privacy pass request"
            })
        ));
        assert!(matches!(
            PrivacyPassIssueResponse::new(Vec::new()),
            Err(ProtoError::OutOfRange {
                field: "privacy pass response"
            })
        ));
    }

    #[test]
    fn debug_prints_only_the_length() -> Result<(), ProtoError> {
        let request = PrivacyPassIssueRequest::new(vec![7; 54])?;
        assert_eq!(format!("{request:?}"), "PrivacyPassIssueRequest(54 bytes)");
        let response = PrivacyPassIssueResponse::new(vec![7; 147])?;
        assert_eq!(
            format!("{response:?}"),
            "PrivacyPassIssueResponse(147 bytes)"
        );
        Ok(())
    }
}
