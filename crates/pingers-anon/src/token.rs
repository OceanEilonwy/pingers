// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy Pass client: token requests and redemption (RFC 9576/9578, type 0x0001).

use rand_core::CryptoRng;

use crate::AnonError;
use crate::stub::{Unimplemented, not_implemented};

/// How many tokens to request in one batch (up to 300 a day, design B.4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenCount(u16);

impl TokenCount {
    /// The most tokens one batch may ask for.
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

/// A batch request in flight: blinded elements sent, blinds kept to finalise the answer.
pub struct PendingIssuance {
    unimplemented: Unimplemented,
}

impl PendingIssuance {
    /// The request body for `POST /v1/pp/issue`.
    #[must_use]
    pub fn request(&self) -> &[u8] {
        match self.unimplemented {}
    }

    /// Verifies the issuer's batch proof and unblinds the tokens.
    ///
    /// # Errors
    ///
    /// [`AnonError::BadIssuance`] if the response does not match or its proof fails.
    pub fn finish(self, response: &[u8]) -> Result<Vec<PrivacyPassToken>, AnonError> {
        let _ = response;
        match self.unimplemented {}
    }
}

/// Starts a batch request against the issuer's public key (from `GET /v1/pp/keys`).
///
/// # Errors
///
/// [`AnonError::BadIssuance`] if the issuer key is invalid; [`AnonError::Internal`].
pub fn begin_token_issuance(
    issuer_public_key: &[u8],
    count: TokenCount,
    rng: &mut impl CryptoRng,
) -> Result<PendingIssuance, AnonError> {
    not_implemented((issuer_public_key, count, rng))
}

/// One unspent token, shared with the app's background handlers through the `pp_token` table.
/// `Debug` does not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivacyPassToken(Vec<u8>);

impl PrivacyPassToken {
    /// Loads a stored token.
    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The token encoding, for storage.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The `Authorization: PrivateToken token=...` header value that spends it (RFC 9577).
    ///
    /// # Errors
    ///
    /// [`AnonError::Internal`] if the encoding fails.
    pub fn authorization(&self) -> Result<String, AnonError> {
        not_implemented(self)
    }
}

impl core::fmt::Debug for PrivacyPassToken {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("PrivacyPassToken(redacted)")
    }
}
