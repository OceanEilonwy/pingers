// SPDX-License-Identifier: AGPL-3.0-only
//! The account key: one Ed25519 key per device, also its MLS leaf signature key (design B.1).

use core::fmt;

use ed25519_dalek::SigningKey;
use pingers_proto::AccountId;
use sha2::{Digest as _, Sha256};

use crate::CryptoError;
use crate::secret::{random_secret, secret_key};
use crate::stub::not_implemented;

secret_key!(
    /// The account's Ed25519 secret key (32-byte seed).
    ///
    /// `from_bytes` and `expose_bytes` are public because pingers-store keeps it in the
    /// encrypted database and pingers-mls builds the MLS credential from it.
    AccountSigningKey,
    32,
    pub
);
random_secret!(AccountSigningKey);

impl AccountSigningKey {
    /// The matching public key.
    #[must_use]
    pub fn public_key(&self) -> AccountPublicKey {
        AccountPublicKey(
            SigningKey::from_bytes(self.expose_bytes())
                .verifying_key()
                .to_bytes(),
        )
    }
}

/// An account's Ed25519 public key: its identity in every Party roster.
/// `Debug` does not print it (style guide §9).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountPublicKey([u8; 32]);

impl AccountPublicKey {
    /// Wraps key bytes from a roster or the wire. Validity is checked when verifying.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for AccountPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountPublicKey(redacted)")
    }
}

/// An Ed25519 signature.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; 64]);

impl Signature {
    /// Wraps signature bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// The signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Signature(..)")
    }
}

/// `account_id = SHA-256(pk)[..16]` (design B.1).
#[must_use]
pub fn account_id(public_key: &AccountPublicKey) -> AccountId {
    let digest = Sha256::digest(public_key.as_bytes());
    let mut id = [0; 16];
    for (target, source) in id.iter_mut().zip(digest.iter()) {
        *target = *source;
    }
    AccountId::from_bytes(id)
}

/// The proof of key possession in `POST /v1/accounts` (design D): a signature over
/// `"pingers/v1/register" || public key || server origin`.
///
/// # Errors
///
/// [`CryptoError::Internal`] if signing fails.
pub fn sign_registration(
    signing_key: &AccountSigningKey,
    server: &pingers_proto::ServerId,
) -> Result<Signature, CryptoError> {
    not_implemented((signing_key, server))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_id_is_the_first_16_bytes_of_the_key_hash() {
        // SHA-256 of 32 zero bytes begins 66 68 7a ad f8 62 bd 77 6c 8f c1 8b 8e 9f 8e 20.
        let id = account_id(&AccountPublicKey::from_bytes([0; 32]));
        assert_eq!(
            id,
            AccountId::from_bytes([
                0x66, 0x68, 0x7a, 0xad, 0xf8, 0x62, 0xbd, 0x77, 0x6c, 0x8f, 0xc1, 0x8b, 0x8e, 0x9f,
                0x8e, 0x20
            ])
        );
    }
}
