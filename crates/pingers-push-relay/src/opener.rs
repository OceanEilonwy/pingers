// SPDX-License-Identifier: AGPL-3.0-only
//! Opening sealed push tokens. The only place this crate calls pingers-crypto's sealed-push
//! functions, so a change to that contract touches one file.

use pingers_crypto::{CryptoError, PushBind, PushTokenToOpen};
use pingers_proto::{PushTokenPlaintext, SealedPushBlob};

use crate::keys::RelayKey;

/// Why a blob did not give a usable token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The blob does not open under this key, key id and bind: tampered, or presented with
    /// another mailbox's bind or another key id (design B.4.9). The relay answers 422.
    DoesNotOpen,
    /// It opened, but the plaintext is malformed. The device sealed garbage; it will never
    /// work, so the relay answers 410 and the server deletes it.
    Malformed,
    /// The cryptography failed for a reason unrelated to the blob. The relay answers 500.
    Unavailable,
}

/// Opens a sealed push token under one of the relay's keys and a bind.
pub trait TokenOpener: Send + Sync + 'static {
    /// Opens `blob` with `key`, checking that it was sealed for `bind` and for `key`'s id.
    ///
    /// # Errors
    ///
    /// See [`OpenError`].
    fn open(
        &self,
        key: &RelayKey,
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
        key: &RelayKey,
        bind: &PushBind,
        blob: &SealedPushBlob,
    ) -> Result<PushTokenPlaintext, OpenError> {
        let token = PushTokenToOpen {
            key_id: key.key_id().get(),
            bind: *bind,
            blob,
        };
        pingers_crypto::open_push_token(key.private_key(), &token).map_err(|error| classify(&error))
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
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_crypto::{CryptoError, PushRelayPrivateKey, PushTokenToSeal};
    use pingers_proto::{PushClass, PushPlatform, PushRelayKeyId, PushToken};
    use pingers_testkit::seeded_rng;
    use pingers_time::UnixSeconds;
    use zeroize::Zeroizing;

    use super::*;
    use crate::keys::KeyLifetime;
    use crate::keys::tests::RFC7748_PRIVATE;

    fn relay_key(key_id: u8) -> Result<RelayKey, CryptoError> {
        RelayKey::new(
            PushRelayKeyId::new(key_id),
            PushRelayPrivateKey::from_bytes(Zeroizing::new(RFC7748_PRIVATE)),
            KeyLifetime {
                activated_at: UnixSeconds::EPOCH,
                retired_at: None,
            },
        )
    }

    #[test]
    fn a_blob_opens_only_under_the_key_id_it_was_sealed_for() -> Result<(), CryptoError> {
        // One private key configured as both key 1 and key 2: the key id is in the HPKE info,
        // so a blob sealed for key 1 does not open when the server names key 2.
        let key_one = relay_key(1)?;
        let key_two = relay_key(2)?;
        let bind = PushBind::from_bytes([0xb1; 32]);
        let plaintext = PushTokenPlaintext {
            platform: PushPlatform::UnifiedPush,
            class: PushClass::Standard,
            token: PushToken::from_bytes(b"https://up.example/t".to_vec()),
            not_after: UnixSeconds::new(2_000_000_000),
        };
        let blob = pingers_crypto::seal_push_token(
            key_one.public_key(),
            &PushTokenToSeal {
                bind,
                plaintext: &plaintext,
            },
            &mut seeded_rng(1),
        )?;
        assert_eq!(CryptoOpener.open(&key_one, &bind, &blob), Ok(plaintext));
        assert_eq!(
            CryptoOpener.open(&key_two, &bind, &blob),
            Err(OpenError::DoesNotOpen)
        );
        Ok(())
    }

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
