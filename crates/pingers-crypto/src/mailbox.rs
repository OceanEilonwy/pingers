// SPDX-License-Identifier: AGPL-3.0-only
//! Mailbox ids and sealed push tokens (design B.4.9).

/// Re-exported from pingers-proto, where the push request carries it; the derivations stay
/// here.
pub use pingers_proto::PushBind;
use pingers_proto::{MailboxId, PushTokenPlaintext, SealedPushBlob, SubscriptionId};
use rand_core::CryptoRng;
use sha2::{Digest as _, Sha256};

use crate::hpke::{self, Recipient, Suite};
use crate::secret::{random_secret, secret_key};
use crate::{CryptoError, labels};

secret_key!(
    /// A mailbox's 32-byte read key: the capability to read it (design B.4.9). Stays on the
    /// device. `from_bytes` and `expose_bytes` are public for pingers-store and for the read
    /// request, which carries the key as its credential (design D, auth mode Cap).
    MailboxReadKey,
    32,
    pub
);
random_secret!(MailboxReadKey);

/// `mailbox_id = SHA-256("pingers/v1/mbox" || read_key)[..16]`.
#[must_use]
pub fn mailbox_id(read_key: &MailboxReadKey) -> MailboxId {
    let digest = Sha256::new()
        .chain_update(labels::MAILBOX)
        .chain_update(read_key.expose_bytes());
    let mut id = [0; 16];
    for (target, source) in id.iter_mut().zip(digest.finalize().iter()) {
        *target = *source;
    }
    MailboxId::from_bytes(id)
}

/// `bind = SHA-256("pingers/v1/push-bind" || mailbox_id)`.
#[must_use]
pub fn push_bind_for_mailbox(mailbox: &MailboxId) -> PushBind {
    push_bind(labels::PUSH_BIND, mailbox.as_bytes())
}

/// The bind of a group subscription: `SHA-256("pingers/v1/sub-bind" || subscription_id)`.
/// The design says "as for a mailbox"; its own label means a subscription bind never equals
/// the bind of a mailbox whose id has the same bytes, so a blob registered for one cannot be
/// presented as the other.
#[must_use]
pub fn push_bind_for_subscription(subscription: &SubscriptionId) -> PushBind {
    push_bind(labels::SUBSCRIPTION_BIND, subscription.as_bytes())
}

fn push_bind(label: &[u8], id: &[u8; 16]) -> PushBind {
    let digest = Sha256::new()
        .chain_update(label)
        .chain_update(id)
        .finalize();
    let mut bind = [0; 32];
    for (target, source) in bind.iter_mut().zip(digest.iter()) {
        *target = *source;
    }
    PushBind::from_bytes(bind)
}

/// SHA-256 of an encoded OHTTP or push relay key config: the value members pin in Party state
/// (design B.4.9, B.12).
#[must_use]
pub fn key_config_hash(encoded: &[u8]) -> pingers_proto::KeyConfigHash {
    let digest = Sha256::digest(encoded);
    let mut hash = [0; 32];
    for (target, source) in hash.iter_mut().zip(digest.iter()) {
        *target = *source;
    }
    pingers_proto::KeyConfigHash::from_bytes(hash)
}

/// A push relay's HPKE public key (X25519, HKDF-SHA256, AES-128-GCM) from its pinned key
/// config (design D, `RelayConfig`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushRelayPublicKey {
    key_id: u8,
    public_key: [u8; 32],
}

impl PushRelayPublicKey {
    /// Wraps a key from the relay's config. Validity is checked when sealing.
    #[must_use]
    pub const fn new(key_id: u8, public_key: [u8; 32]) -> Self {
        Self { key_id, public_key }
    }

    /// The key id the relay selects its private key by.
    #[must_use]
    pub const fn key_id(&self) -> u8 {
        self.key_id
    }

    /// The X25519 public key.
    #[must_use]
    pub const fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }
}

secret_key!(
    /// The push relay's HPKE private key. `from_bytes` is public because the relay loads it
    /// from its configuration.
    PushRelayPrivateKey,
    32,
    pub
);
random_secret!(PushRelayPrivateKey);

impl PushRelayPrivateKey {
    /// The matching public key, for the relay's published key config.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Internal`] if the X25519 base-point multiplication fails (it cannot for
    /// a 32-byte key).
    pub fn public_key(&self, key_id: u8) -> Result<PushRelayPublicKey, CryptoError> {
        let public = hpke::public_key(Suite::PushRelay, self.expose_bytes())?;
        Ok(PushRelayPublicKey::new(key_id, public))
    }
}

/// Which relay key and push token a sealed push was made from, as a 16-byte fingerprint the
/// device keeps beside each mailbox and subscription it registered (design B.4.9): when the
/// token changes or the relay key rotates, the fingerprint changes, and the device knows which
/// registrations to seal again. Never sent anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PushSealFingerprint([u8; 16]);

impl PushSealFingerprint {
    /// Byte length.
    pub const LENGTH: usize = 16;

    /// Wraps stored bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The bytes, for storage.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// `SHA-256("pingers/v1/push-seal-id" || key_id || relay public key || padded plaintext with
/// class Standard and not_after 0)[..16]`: the platform and token under one relay key. The
/// class and `not_after` are left out, since they follow from the purpose and the time.
///
/// # Errors
///
/// [`CryptoError::Encoding`] if the token does not fit a sealed push.
pub fn push_seal_fingerprint(
    relay_key: &PushRelayPublicKey,
    platform: pingers_proto::PushPlatform,
    token: &pingers_proto::PushToken,
) -> Result<PushSealFingerprint, CryptoError> {
    let canonical = PushTokenPlaintext {
        platform,
        class: pingers_proto::PushClass::Standard,
        token: token.clone(),
        not_after: pingers_time::UnixSeconds::EPOCH,
    }
    .encode_padded()
    .map_err(CryptoError::Encoding)?;
    let digest = Sha256::new()
        .chain_update(labels::PUSH_SEAL_ID)
        .chain_update([relay_key.key_id()])
        .chain_update(relay_key.public_key())
        .chain_update(canonical)
        .finalize();
    let mut fingerprint = [0; 16];
    for (target, source) in fingerprint.iter_mut().zip(digest.iter()) {
        *target = *source;
    }
    Ok(PushSealFingerprint(fingerprint))
}

/// Seals a push token to the relay: `HPKE.Seal(pk, info = "pingers/v1/push" || bind ||
/// key_id, padded plaintext)`, giving exactly 320 bytes; `key_id` is `relay_key.key_id()`. Fresh randomness every time, so two blobs
/// for the same token never look alike.
///
/// # Errors
///
/// [`CryptoError::InvalidKey`] if the relay key is not a valid X25519 point;
/// [`CryptoError::Encoding`] if the plaintext does not fit.
pub fn seal_push_token(
    relay_key: &PushRelayPublicKey,
    token: &PushTokenToSeal<'_>,
    rng: &mut impl CryptoRng,
) -> Result<SealedPushBlob, CryptoError> {
    let padded = zeroize::Zeroizing::new(
        token
            .plaintext
            .encode_padded()
            .map_err(CryptoError::Encoding)?,
    );
    seal_push_padded(relay_key, &token.bind, &padded, rng)
}

/// Seals an already padded plaintext; the part of [`seal_push_token`] after proto encodes.
pub(crate) fn seal_push_padded(
    relay_key: &PushRelayPublicKey,
    bind: &PushBind,
    padded: &[u8; PushTokenPlaintext::PADDED_SIZE],
    rng: &mut impl CryptoRng,
) -> Result<SealedPushBlob, CryptoError> {
    let sealed = hpke::seal(
        Recipient {
            suite: Suite::PushRelay,
            public_key: relay_key.public_key(),
        },
        &push_info(bind, relay_key.key_id()),
        padded,
        rng,
    )?;
    push_blob(&sealed)
}

/// `"pingers/v1/push" || bind || key_id`. The key id is bound so a blob sealed to one relay
/// key cannot be presented under another key id (for example after an operator reuses a
/// private key under a new id); the design's info has only the bind.
pub(crate) fn push_info(bind: &PushBind, key_id: u8) -> Vec<u8> {
    [labels::PUSH, bind.as_bytes(), &[key_id]].concat()
}

fn push_blob(sealed: &[u8]) -> Result<SealedPushBlob, CryptoError> {
    let bytes: [u8; SealedPushBlob::SIZE] =
        sealed.try_into().map_err(|_length| CryptoError::Internal)?;
    Ok(SealedPushBlob::from_bytes(bytes))
}

/// Opens a blob to its padded plaintext, before proto decodes it.
pub(crate) fn open_push_padded(
    relay_key: &PushRelayPrivateKey,
    token: &PushTokenToOpen<'_>,
) -> Result<zeroize::Zeroizing<[u8; PushTokenPlaintext::PADDED_SIZE]>, CryptoError> {
    let plaintext = hpke::open(
        Suite::PushRelay,
        relay_key.expose_bytes(),
        &push_info(&token.bind, token.key_id),
        token.blob.as_bytes(),
    )?;
    let padded: [u8; PushTokenPlaintext::PADDED_SIZE] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_length| CryptoError::Internal)?;
    Ok(zeroize::Zeroizing::new(padded))
}

/// A push token and the mailbox or subscription it is bound to.
#[derive(Clone, Copy, Debug)]
pub struct PushTokenToSeal<'token> {
    /// The bind of the mailbox or subscription.
    pub bind: PushBind,
    /// Platform, class, token and expiry.
    pub plaintext: &'token PushTokenPlaintext,
}

/// A sealed push token as the relay receives it: the blob, and the bind and key id the server
/// presented it with.
#[derive(Clone, Copy, Debug)]
pub struct PushTokenToOpen<'blob> {
    /// The key id the request names; must be the id of the key the blob was sealed to.
    pub key_id: u8,
    /// The bind of the mailbox or subscription.
    pub bind: PushBind,
    /// The sealed token.
    pub blob: &'blob SealedPushBlob,
}

/// Opens a sealed push token on the relay with the private key of `token.key_id`. Fails if
/// the bind or the key id is not the one it was sealed with: `info = "pingers/v1/push" ||
/// bind || key_id`.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if the blob does not open under this key, bind and key id;
/// [`CryptoError::Encoding`] if the plaintext is malformed.
pub fn open_push_token(
    relay_key: &PushRelayPrivateKey,
    token: &PushTokenToOpen<'_>,
) -> Result<PushTokenPlaintext, CryptoError> {
    let padded = open_push_padded(relay_key, token)?;
    PushTokenPlaintext::decode_padded(&padded).map_err(CryptoError::Encoding)
}

#[cfg(test)]
mod fingerprint_tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::{PushPlatform, PushToken};

    use super::*;

    fn token(text: &str) -> PushToken {
        PushToken::from_bytes(text.as_bytes().to_vec())
    }

    #[test]
    fn the_fingerprint_is_the_labelled_hash_of_the_key_and_the_canonical_plaintext()
    -> Result<(), CryptoError> {
        let key = PushRelayPublicKey::new(3, [7; 32]);
        let fingerprint = push_seal_fingerprint(&key, PushPlatform::UnifiedPush, &token("e"))?;
        let canonical = PushTokenPlaintext {
            platform: PushPlatform::UnifiedPush,
            class: pingers_proto::PushClass::Standard,
            token: token("e"),
            not_after: pingers_time::UnixSeconds::EPOCH,
        }
        .encode_padded()
        .map_err(CryptoError::Encoding)?;
        let digest = Sha256::new()
            .chain_update(b"pingers/v1/push-seal-id")
            .chain_update([3])
            .chain_update([7; 32])
            .chain_update(canonical)
            .finalize();
        assert_eq!(
            fingerprint.as_bytes().as_slice(),
            digest.get(..16).unwrap_or_default()
        );
        Ok(())
    }

    #[test]
    fn a_new_token_platform_or_relay_key_changes_the_fingerprint() -> Result<(), CryptoError> {
        let key = PushRelayPublicKey::new(3, [7; 32]);
        let base = push_seal_fingerprint(&key, PushPlatform::UnifiedPush, &token("e"))?;
        assert_eq!(
            push_seal_fingerprint(&key, PushPlatform::UnifiedPush, &token("e"))?,
            base
        );
        for other in [
            push_seal_fingerprint(&key, PushPlatform::UnifiedPush, &token("f"))?,
            push_seal_fingerprint(&key, PushPlatform::Fcm, &token("e"))?,
            push_seal_fingerprint(
                &PushRelayPublicKey::new(4, [7; 32]),
                PushPlatform::UnifiedPush,
                &token("e"),
            )?,
            push_seal_fingerprint(
                &PushRelayPublicKey::new(3, [8; 32]),
                PushPlatform::UnifiedPush,
                &token("e"),
            )?,
        ] {
            assert_ne!(other, base);
        }
        Ok(())
    }
}
