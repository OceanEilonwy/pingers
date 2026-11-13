// SPDX-License-Identifier: AGPL-3.0-only
//! Opening sealed push tokens. The only place this crate calls pingers-crypto's sealed-push
//! functions, so a change to that contract touches one file.

use pingers_crypto::{CryptoError, PushBind, PushRelayPrivateKey};
use pingers_proto::{PushTokenPlaintext, SealedPushBlob};

/// Why a blob did not give a usable token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The blob does not open under this key and bind: tampered, or presented with another
    /// mailbox's bind (design B.4.9). The relay answers 422.
    DoesNotOpen,
    /// It opened, but the plaintext is malformed. The device sealed garbage; it will never
    /// work, so the relay answers 410 and the server deletes it.
    Malformed,
    /// The cryptography failed for a reason unrelated to the blob. The relay answers 500.
    Unavailable,
}

/// Opens a sealed push token under one of the relay's keys and a bind.
pub trait TokenOpener: Send + Sync + 'static {
    /// Opens `blob`, checking that it was sealed for `bind`.
    ///
    /// # Errors
    ///
    /// See [`OpenError`].
    fn open(
        &self,
        key: &PushRelayPrivateKey,
        bind: &PushBind,
        blob: &SealedPushBlob,
    ) -> Result<PushTokenPlaintext, OpenError>;
}

/// The production opener: HPKE through pingers-crypto.
#[derive(Clone, Copy, Debug, Default)]
pub struct CryptoOpener;

impl TokenOpener for CryptoOpener {
    fn open(
        &self,
        key: &PushRelayPrivateKey,
        bind: &PushBind,
        blob: &SealedPushBlob,
    ) -> Result<PushTokenPlaintext, OpenError> {
        pingers_crypto::open_push_token(key, bind, blob).map_err(|error| classify(&error))
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "CryptoError is non-exhaustive and its stub-only variants disappear as it is \
              implemented; everything that is not about the blob is the relay's own failure"
)]
fn classify(error: &CryptoError) -> OpenError {
    match error {
        CryptoError::OpenFailed => OpenError::DoesNotOpen,
        CryptoError::Encoding(_) | CryptoError::WrongLength { .. } => OpenError::Malformed,
        // InvalidKey is the relay's own key; Internal is not the blob's fault either.
        _ => OpenError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_open_is_a_bind_mismatch_and_a_bad_plaintext_is_permanent() {
        assert_eq!(classify(&CryptoError::OpenFailed), OpenError::DoesNotOpen);
        assert_eq!(
            classify(&CryptoError::WrongLength {
                expected: 272,
                actual: 0
            }),
            OpenError::Malformed
        );
        assert_eq!(classify(&CryptoError::Internal), OpenError::Unavailable);
        assert_eq!(classify(&CryptoError::InvalidKey), OpenError::Unavailable);
    }
}
