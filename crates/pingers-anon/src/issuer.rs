// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy Pass issuer and redemption check, server side (type 0x0001, issuer = origin).

use rand_core::CryptoRng;

use crate::AnonError;
use crate::stub::{Unimplemented, not_implemented};

/// The issuer's VOPRF private key, rotated weekly (design C.1 `pp_key`). No `Debug` or `Clone`.
pub struct IssuerKey {
    unimplemented: Unimplemented,
}

impl IssuerKey {
    /// Generates a key.
    ///
    /// # Errors
    ///
    /// [`AnonError::Internal`] if key generation fails.
    pub fn generate(rng: &mut impl CryptoRng) -> Result<Self, AnonError> {
        not_implemented(rng)
    }

    /// Loads a stored key.
    ///
    /// # Errors
    ///
    /// [`AnonError::NotAFieldElement`] if the bytes are not a valid P-384 scalar.
    pub fn from_bytes(bytes: zeroize::Zeroizing<Vec<u8>>) -> Result<Self, AnonError> {
        not_implemented(bytes)
    }

    /// The public key to publish at `GET /v1/pp/keys`.
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        match self.unimplemented {}
    }

    /// The key id tokens name.
    #[must_use]
    pub fn key_id(&self) -> [u8; 32] {
        match self.unimplemented {}
    }
}

/// Evaluates a batch request and proves it (batched issuance, RFC 9578). Quotas are the
/// caller's check (pingers-quota).
///
/// # Errors
///
/// [`AnonError::BadIssuance`] if the request is malformed.
pub fn issue_tokens(
    key: &IssuerKey,
    request: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, AnonError> {
    not_implemented((key, request, rng))
}

/// Verifies a redeemed token and returns its nonce, which the caller checks against and adds to
/// the spent set.
///
/// # Errors
///
/// [`AnonError::BadToken`] if it does not verify under `key`.
pub fn check_redemption(key: &IssuerKey, token: &[u8]) -> Result<[u8; 32], AnonError> {
    not_implemented((key, token))
}
