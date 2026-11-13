// SPDX-License-Identifier: AGPL-3.0-only
//! Keyed hashes for the push relay: per-token rate-limit keys and stored server credentials
//! (design C.1a).
//!
//! `keyed_hash = HMAC-SHA-256(key, label || input)`, with one label per purpose so a value
//! made for one purpose never equals one made for the other under the same key.

use subtle::{Choice, ConstantTimeEq};

use crate::secret::{random_secret, secret_key};
use crate::{CryptoError, labels, primitives};

secret_key!(
    /// The 32-byte key of [`keyed_hash`]. `from_bytes` and `expose_bytes` are public because
    /// the push relay reads it from its configuration file, where the operator puts it next to
    /// the credential hashes made with it.
    KeyedHashKey,
    32,
    pub
);
random_secret!(KeyedHashKey);

/// What a keyed hash is for. Each purpose has its own label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeyedHashPurpose {
    /// The push relay's per-token rate-limit key, in place of `SHA-256(push_token)` (design
    /// C.1a): without the key, a memory dump does not confirm a guessed token. Label
    /// `pingers/v1/relay-token`.
    PushTokenRateLimit,
    /// A Pingers server's bearer credential as the relay stores it, so the configuration
    /// file never holds a usable credential. Label `pingers/v1/relay-credential`.
    ServerCredential,
}

impl KeyedHashPurpose {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::PushTokenRateLimit => labels::RELAY_TOKEN,
            Self::ServerCredential => labels::RELAY_CREDENTIAL,
        }
    }
}

/// A 32-byte keyed hash.
///
/// Compared only with [`ConstantTimeEq::ct_eq`] (style guide §8); there is no `==`. A
/// rate-limit map can key on [`KeyedHash::as_bytes`].
#[derive(Clone, Copy)]
pub struct KeyedHash([u8; 32]);

impl KeyedHash {
    /// Wraps hash bytes, for example a credential hash from the relay's configuration.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The hash bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl ConstantTimeEq for KeyedHash {
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.ct_eq(&other.0)
    }
}

impl core::fmt::Debug for KeyedHash {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("KeyedHash(..)")
    }
}

/// `HMAC-SHA-256(key, label || input)`, the label chosen by `purpose`.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HMAC refuses the key (it cannot for 32 bytes).
pub fn keyed_hash(
    key: &KeyedHashKey,
    purpose: KeyedHashPurpose,
    input: &[u8],
) -> Result<KeyedHash, CryptoError> {
    primitives::hmac_sha256(key.expose_bytes(), &[purpose.label(), input]).map(KeyedHash)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use zeroize::Zeroizing;

    use super::*;

    fn key(byte: u8) -> KeyedHashKey {
        KeyedHashKey::from_bytes(Zeroizing::new([byte; 32]))
    }

    #[test]
    fn a_token_hash_never_passes_for_a_credential_hash() -> Result<(), CryptoError> {
        let token = keyed_hash(&key(1), KeyedHashPurpose::PushTokenRateLimit, b"secret")?;
        let credential = keyed_hash(&key(1), KeyedHashPurpose::ServerCredential, b"secret")?;
        assert!(!bool::from(token.ct_eq(&credential)));
        Ok(())
    }

    #[test]
    fn another_key_gives_another_hash() -> Result<(), CryptoError> {
        let first = keyed_hash(&key(1), KeyedHashPurpose::ServerCredential, b"secret")?;
        let second = keyed_hash(&key(2), KeyedHashPurpose::ServerCredential, b"secret")?;
        assert!(!bool::from(first.ct_eq(&second)));
        assert!(bool::from(
            first.ct_eq(&KeyedHash::from_bytes(*first.as_bytes()))
        ));
        Ok(())
    }

    #[test]
    fn debug_does_not_print_the_hash() -> Result<(), CryptoError> {
        let hash = keyed_hash(&key(1), KeyedHashPurpose::ServerCredential, b"secret")?;
        assert_eq!(format!("{hash:?}"), "KeyedHash(..)");
        Ok(())
    }
}
