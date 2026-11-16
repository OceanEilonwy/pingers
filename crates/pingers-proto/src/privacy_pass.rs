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

use pingers_time::UnixSeconds;

use crate::ProtoError;
use crate::cbor::{CborBody, Writer, array_of, fixed_bytes, list, versioned_cbor};

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

/// One issuer key in `GET /v1/pp/keys` (design D, `PublicKeyInfo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyPassKey {
    /// `SHA-256(public_key)`, the RFC 9578 token key id.
    pub key_id: [u8; 32],
    /// The compressed P-384 public key (49 bytes), for pingers-anon's
    /// `begin_token_issuance`.
    pub public_key: Vec<u8>,
    /// Until when the key issues; fetch the next one after this.
    pub issue_until: UnixSeconds,
    /// Until when tokens under this key are accepted.
    pub redeem_until: UnixSeconds,
}

/// `GET /v1/pp/keys`: every key whose tokens are still accepted, oldest first; the last one
/// issues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyPassKeys {
    /// The keys.
    pub keys: Vec<PrivacyPassKey>,
}

/// Layout: `[key id (32), public key, issue until, redeem until]`. Nested, so unversioned.
impl CborBody for PrivacyPassKey {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        writer.bytes(&self.key_id)?;
        writer.bytes(&self.public_key)?;
        writer.unsigned(self.issue_until.get());
        writer.unsigned(self.redeem_until.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        array_of(decoder, 4)?;
        Ok(Self {
            key_id: fixed_bytes(decoder)?,
            public_key: decoder.bytes()?.to_vec(),
            issue_until: UnixSeconds::new(decoder.u64()?),
            redeem_until: UnixSeconds::new(decoder.u64()?),
        })
    }
}

/// Layout: `[[key...]]`.
impl CborBody for PrivacyPassKeys {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(1)?;
        writer.list(&self.keys)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        array_of(decoder, 1)?;
        Ok(Self {
            keys: list(decoder)?,
        })
    }
}

/// A number of Privacy Pass tokens an account may be issued: the daily limit a server
/// advertises in `Params`, or what is left of it today (design B.4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenAllowance(u16);

impl TokenAllowance {
    /// No tokens.
    pub const NONE: Self = Self(0);

    /// `tokens` tokens.
    #[must_use]
    pub const fn new(tokens: u16) -> Self {
        Self(tokens)
    }

    /// The count.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// The body of a `429` to `POST /v1/pp/issue`: the batch would exceed the account's daily
/// allowance, so nothing was issued. Says how many tokens the account may still be issued
/// today and when the next day's allowance starts, so the device asks for at most that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrivacyPassAllowance {
    /// Tokens the account may still be issued today.
    pub remaining: TokenAllowance,
    /// When the next (UTC) day starts and the full allowance is available again.
    pub resets_at: UnixSeconds,
}

/// Layout: `[remaining, resets at]`.
impl CborBody for PrivacyPassAllowance {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.unsigned(self.remaining.get());
        writer.unsigned(self.resets_at.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        array_of(decoder, 2)?;
        Ok(Self {
            remaining: TokenAllowance::new(decoder.u16()?),
            resets_at: UnixSeconds::new(decoder.u64()?),
        })
    }
}

versioned_cbor!(
    PrivacyPassIssueRequest,
    PrivacyPassIssueResponse,
    PrivacyPassKeys,
    PrivacyPassAllowance
);

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn issuer_keys_round_trip() -> Result<(), ProtoError> {
        use crate::{WireDecode as _, WireEncode as _};
        let keys = PrivacyPassKeys {
            keys: vec![
                PrivacyPassKey {
                    key_id: [1; 32],
                    public_key: vec![2; 49],
                    issue_until: UnixSeconds::new(100),
                    redeem_until: UnixSeconds::new(200),
                },
                PrivacyPassKey {
                    key_id: [3; 32],
                    public_key: vec![4; 49],
                    issue_until: UnixSeconds::new(300),
                    redeem_until: UnixSeconds::new(400),
                },
            ],
        };
        let encoded = keys.encode()?;
        assert_eq!(PrivacyPassKeys::decode(&encoded)?, keys);
        // `[1, [[[h'01'*32, h'02'*49, 100, 200], ...]]]`
        assert_eq!(encoded.get(..4), Some(&[0x82, 0x01, 0x81, 0x82][..]));
        Ok(())
    }

    #[test]
    fn an_allowance_round_trips_with_its_fixed_layout() -> Result<(), ProtoError> {
        use crate::{WireDecode as _, WireEncode as _};
        let allowance = PrivacyPassAllowance {
            remaining: TokenAllowance::new(19),
            resets_at: UnixSeconds::new(86_400),
        };
        let encoded = allowance.encode()?;
        // `[1, [19, 86400]]`
        assert_eq!(
            encoded,
            [0x82, 0x01, 0x82, 0x13, 0x1a, 0x00, 0x01, 0x51, 0x80]
        );
        assert_eq!(PrivacyPassAllowance::decode(&encoded)?, allowance);
        Ok(())
    }

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
