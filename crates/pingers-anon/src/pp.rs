// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy Pass wire formats shared by the client and the issuer (RFC 9577, RFC 9578, token
//! type 0x0001: VOPRF over P-384 with SHA-384, RFC 9497).
//!
//! - Token (RFC 9577 §2.2): `token_type(2) || nonce(32) || challenge_digest(32) ||
//!   token_key_id(32) || authenticator(48)`, 146 bytes.
//! - `token_key_id = SHA-256(pkI)`, `pkI` the 49-byte compressed public key.
//! - Batched request (RFC 9578 batched issuance): `token_type(2) || truncated_token_key_id(1)
//!   || blinded_elements<0..2^16-1>`; response: `evaluated_elements<0..2^16-1> ||
//!   evaluated_proof(96)`. Vector lengths are 2-byte big-endian byte counts.
//! - Tokens are fetched before any challenge exists, so every token answers one fixed
//!   `TokenChallenge`: type 0x0001, issuer name `pingers`, empty redemption context, empty
//!   origin info. The server's spent set, not the challenge, makes them single-use.

use sha2::{Digest as _, Sha256};

use crate::AnonError;

/// How many tokens one batch asks for, 1 to 300 (the daily allowance, design B.4.8). The
/// client chooses it; the issuer reads it back from a request with
/// [`batch_token_count`](crate::batch_token_count).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TokenCount(u16);

impl TokenCount {
    /// The most tokens one batch may ask for: the only definition of the batch limit.
    pub const MAX: u16 = 300;

    /// Checks the range 1 to 300.
    ///
    /// # Errors
    ///
    /// [`AnonError::BadIssuance`] outside that range.
    pub const fn new(count: u16) -> Result<Self, AnonError> {
        if count >= 1 && count <= Self::MAX {
            Ok(Self(count))
        } else {
            Err(AnonError::BadIssuance)
        }
    }

    /// The count.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// The VOPRF suite: P384-SHA384.
pub(crate) type Suite = p384::NistP384;

/// Token type 0x0001, big-endian.
pub(crate) const TOKEN_TYPE: [u8; 2] = [0x00, 0x01];
/// A serialised P-384 element (compressed SEC1).
pub(crate) const ELEMENT_LEN: usize = 49;
/// The batch DLEQ proof: two P-384 scalars.
pub(crate) const PROOF_LEN: usize = 96;
/// The VOPRF output: one SHA-384 digest.
const AUTHENTICATOR_LEN: usize = 48;
/// The token nonce.
pub(crate) const NONCE_LEN: usize = 32;
/// The issuer name in the fixed token challenge.
const ISSUER_NAME: &[u8] = b"pingers";

/// The digest of the fixed `TokenChallenge` every Pingers token answers.
pub(crate) fn challenge_digest() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TOKEN_TYPE);
    hasher.update([0x00, 0x07]); // issuer_name length; must match ISSUER_NAME
    hasher.update(ISSUER_NAME);
    hasher.update([0x00]); // redemption_context: empty
    hasher.update([0x00, 0x00]); // origin_info: empty
    hasher.finalize().into()
}

/// `token_key_id = SHA-256(pkI)` (RFC 9578), for a client checking that a key list from the
/// issuer names each key by its own hash before fetching tokens against it.
#[must_use]
pub fn token_key_id(public_key: &[u8]) -> [u8; 32] {
    key_id(public_key)
}

/// `token_key_id = SHA-256(pkI)`.
pub(crate) fn key_id(public_key: &[u8]) -> [u8; 32] {
    Sha256::digest(public_key).into()
}

#[cfg(feature = "prover")]
/// The VOPRF input for a token: everything in it but the authenticator.
pub(crate) fn token_input(nonce: &[u8; NONCE_LEN], key_id: &[u8; 32]) -> Vec<u8> {
    let mut input = Vec::with_capacity(TOKEN_INPUT_LEN);
    input.extend_from_slice(&TOKEN_TYPE);
    input.extend_from_slice(nonce);
    input.extend_from_slice(&challenge_digest());
    input.extend_from_slice(key_id);
    input
}

/// The length of a token input.
const TOKEN_INPUT_LEN: usize = 2 + NONCE_LEN + 32 + 32;

/// The length of a whole token.
pub(crate) const TOKEN_LEN: usize = TOKEN_INPUT_LEN + AUTHENTICATOR_LEN;

#[cfg(feature = "issuer")]
/// A token split into its fields.
pub(crate) struct ParsedToken<'token> {
    /// The VOPRF input (`token_type || nonce || challenge_digest || token_key_id`).
    pub(crate) input: &'token [u8],
    /// The nonce the spent set is keyed by.
    pub(crate) nonce: [u8; NONCE_LEN],
    /// Which issuer key it claims.
    pub(crate) key_id: &'token [u8],
    /// The VOPRF output.
    pub(crate) authenticator: &'token [u8],
}

#[cfg(feature = "issuer")]
/// Splits a token and checks its type and challenge.
pub(crate) fn parse_token(token: &[u8]) -> Result<ParsedToken<'_>, AnonError> {
    if token.len() != TOKEN_LEN {
        return Err(AnonError::BadToken);
    }
    let (input, authenticator) = token
        .split_at_checked(TOKEN_INPUT_LEN)
        .ok_or(AnonError::BadToken)?;
    let (token_type, rest) = input.split_at_checked(2).ok_or(AnonError::BadToken)?;
    let (nonce, rest) = rest
        .split_at_checked(NONCE_LEN)
        .ok_or(AnonError::BadToken)?;
    let (challenge, key_id) = rest.split_at_checked(32).ok_or(AnonError::BadToken)?;
    if token_type != TOKEN_TYPE || challenge != challenge_digest() {
        return Err(AnonError::BadToken);
    }
    Ok(ParsedToken {
        input,
        nonce: nonce.try_into().map_err(|_| AnonError::BadToken)?,
        key_id,
        authenticator,
    })
}

#[cfg(feature = "issuer")]
/// A decoded batched token request.
pub(crate) struct BatchRequest<'request> {
    /// The last byte of the issuer key id the client used.
    pub(crate) truncated_key_id: u8,
    /// The blinded elements, [`ELEMENT_LEN`] bytes each.
    pub(crate) elements: Vec<&'request [u8]>,
}

#[cfg(feature = "prover")]
/// Encodes a batched token request.
pub(crate) fn encode_request(
    truncated_key_id: u8,
    elements: &[impl AsRef<[u8]>],
) -> Result<Vec<u8>, AnonError> {
    let body = concatenate(elements);
    let mut request = Vec::with_capacity(body.len().saturating_add(5));
    request.extend_from_slice(&TOKEN_TYPE);
    request.push(truncated_key_id);
    request.extend_from_slice(&length_prefix(&body)?);
    request.extend_from_slice(&body);
    Ok(request)
}

#[cfg(feature = "issuer")]
/// Decodes a batched token request. Any malformation is [`AnonError::BadIssuance`].
pub(crate) fn decode_request(request: &[u8]) -> Result<BatchRequest<'_>, AnonError> {
    let (token_type, rest) = request.split_at_checked(2).ok_or(AnonError::BadIssuance)?;
    if token_type != TOKEN_TYPE {
        return Err(AnonError::BadIssuance);
    }
    let (truncated, rest) = rest.split_first().ok_or(AnonError::BadIssuance)?;
    let (body, rest) = read_vector(rest)?;
    if !rest.is_empty() {
        return Err(AnonError::BadIssuance);
    }
    Ok(BatchRequest {
        truncated_key_id: *truncated,
        elements: split_elements(body)?,
    })
}

#[cfg(feature = "prover")]
/// A decoded batched token response.
pub(crate) struct BatchResponse<'response> {
    /// The evaluated elements, [`ELEMENT_LEN`] bytes each.
    pub(crate) elements: Vec<&'response [u8]>,
    /// The batch DLEQ proof.
    pub(crate) proof: &'response [u8],
}

#[cfg(feature = "issuer")]
/// Encodes a batched token response.
pub(crate) fn encode_response(
    elements: &[impl AsRef<[u8]>],
    proof: &[u8],
) -> Result<Vec<u8>, AnonError> {
    let body = concatenate(elements);
    let mut response = Vec::with_capacity(body.len().saturating_add(PROOF_LEN).saturating_add(2));
    response.extend_from_slice(&length_prefix(&body)?);
    response.extend_from_slice(&body);
    response.extend_from_slice(proof);
    Ok(response)
}

#[cfg(feature = "prover")]
/// Decodes a batched token response. Any malformation is [`AnonError::BadIssuance`].
pub(crate) fn decode_response(response: &[u8]) -> Result<BatchResponse<'_>, AnonError> {
    let (body, proof) = read_vector(response)?;
    if proof.len() != PROOF_LEN {
        return Err(AnonError::BadIssuance);
    }
    Ok(BatchResponse {
        elements: split_elements(body)?,
        proof,
    })
}

fn concatenate(elements: &[impl AsRef<[u8]>]) -> Vec<u8> {
    elements
        .iter()
        .flat_map(|element| element.as_ref().iter().copied())
        .collect()
}

fn length_prefix(body: &[u8]) -> Result<[u8; 2], AnonError> {
    u16::try_from(body.len())
        .map(u16::to_be_bytes)
        .map_err(|_| AnonError::BadIssuance)
}

/// Reads a vector with a 2-byte big-endian length; returns it and the bytes after it.
fn read_vector(bytes: &[u8]) -> Result<(&[u8], &[u8]), AnonError> {
    let (length, rest) = bytes.split_at_checked(2).ok_or(AnonError::BadIssuance)?;
    let length: [u8; 2] = length.try_into().map_err(|_| AnonError::BadIssuance)?;
    rest.split_at_checked(usize::from(u16::from_be_bytes(length)))
        .ok_or(AnonError::BadIssuance)
}

/// Splits a vector body into 1 to [`TokenCount::MAX`] elements.
fn split_elements(body: &[u8]) -> Result<Vec<&[u8]>, AnonError> {
    let elements: Vec<&[u8]> = body.chunks(ELEMENT_LEN).collect();
    let whole = elements.iter().all(|element| element.len() == ELEMENT_LEN);
    if !whole || elements.is_empty() || elements.len() > usize::from(TokenCount::MAX) {
        return Err(AnonError::BadIssuance);
    }
    Ok(elements)
}

#[cfg(feature = "prover")]
/// base64url with padding (RFC 4648 §5), as RFC 9577 headers carry tokens.
pub(crate) fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let symbol = |index: u32| -> char {
        ALPHABET
            .get(usize::try_from(index & 0x3f).unwrap_or(0))
            .map_or('A', |byte| char::from(*byte))
    };
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let mut group = [0_u8; 3];
        for (slot, byte) in group.iter_mut().zip(chunk) {
            *slot = *byte;
        }
        let [first, second, third] = group;
        let bits = u32::from_be_bytes([0, first, second, third]);
        out.push(symbol(bits >> 18));
        out.push(symbol(bits >> 12));
        out.push(if chunk.len() > 1 {
            symbol(bits >> 6)
        } else {
            '='
        });
        out.push(if chunk.len() > 2 { symbol(bits) } else { '=' });
    }
    out
}

#[cfg(all(test, feature = "prover", feature = "issuer"))]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    /// A copy of `bytes` with the byte at `index` replaced.
    fn with_byte(bytes: &[u8], index: usize, value: u8) -> Vec<u8> {
        let mut copy = bytes.to_vec();
        if let Some(byte) = copy.get_mut(index) {
            *byte = value;
        }
        copy
    }

    #[test]
    fn base64url_matches_rfc_4648_vectors() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg==");
        assert_eq!(base64url(b"fo"), "Zm8=");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg==");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8=");
    }

    #[test]
    fn requests_round_trip() -> Result<(), AnonError> {
        let elements = [[1_u8; ELEMENT_LEN], [2_u8; ELEMENT_LEN]];
        let encoded = encode_request(0x7f, &elements)?;
        let decoded = decode_request(&encoded)?;
        assert_eq!(decoded.truncated_key_id, 0x7f);
        assert_eq!(
            decoded.elements,
            vec![&[1_u8; ELEMENT_LEN][..], &[2_u8; ELEMENT_LEN][..]]
        );
        Ok(())
    }

    #[test]
    fn malformed_requests_are_rejected() -> Result<(), AnonError> {
        let encoded = encode_request(0, &[[1_u8; ELEMENT_LEN]])?;
        let wrong_type = with_byte(&encoded, 1, 2);
        assert_eq!(wrong_type.len(), encoded.len());
        let mut trailing = encoded.clone();
        trailing.push(0);
        let short = encoded
            .get(..encoded.len().saturating_sub(1))
            .unwrap_or(&[]);
        let empty = encode_request(0, &[[0_u8; 0]])?;
        let ragged = encode_request(0, &[[1_u8; ELEMENT_LEN - 1]])?;
        let too_many = encode_request(
            0,
            &vec![[1_u8; ELEMENT_LEN]; usize::from(TokenCount::MAX).saturating_add(1)],
        )?;
        for bad in [
            &wrong_type[..],
            &trailing,
            short,
            &empty,
            &ragged,
            &too_many,
        ] {
            assert!(matches!(decode_request(bad), Err(AnonError::BadIssuance)));
        }
        assert!(
            decode_request(&encode_request(
                0,
                &vec![[1_u8; ELEMENT_LEN]; usize::from(TokenCount::MAX)]
            )?)
            .is_ok()
        );
        Ok(())
    }

    #[test]
    fn responses_need_a_whole_proof() -> Result<(), AnonError> {
        let encoded = encode_response(&[[3_u8; ELEMENT_LEN]], &[4_u8; PROOF_LEN])?;
        let decoded = decode_response(&encoded)?;
        assert_eq!(decoded.proof, &[4_u8; PROOF_LEN][..]);
        assert_eq!(decoded.elements.len(), 1);
        let short = encode_response(&[[3_u8; ELEMENT_LEN]], &[4_u8; PROOF_LEN - 1])?;
        assert!(matches!(
            decode_response(&short),
            Err(AnonError::BadIssuance)
        ));
        Ok(())
    }

    #[test]
    fn tokens_with_another_type_or_challenge_are_rejected() -> Result<(), AnonError> {
        let mut token = token_input(&[9; NONCE_LEN], &[8; 32]);
        token.extend_from_slice(&[7; AUTHENTICATOR_LEN]);
        let parsed = parse_token(&token)?;
        assert_eq!(parsed.nonce, [9; NONCE_LEN]);
        assert_eq!(parsed.key_id, &[8; 32][..]);
        assert_eq!(parsed.authenticator, &[7; AUTHENTICATOR_LEN][..]);
        let other_type = with_byte(&token, 1, 2);
        let other_challenge = with_byte(&token, 2 + NONCE_LEN, 0);
        assert_eq!(other_type.len(), TOKEN_LEN);
        assert_ne!(other_challenge, token, "the flipped byte was already zero");
        let short = token.get(..TOKEN_LEN - 1).unwrap_or(&[]);
        for bad in [&other_type[..], &other_challenge, short] {
            assert!(matches!(parse_token(bad), Err(AnonError::BadToken)));
        }
        Ok(())
    }
}
