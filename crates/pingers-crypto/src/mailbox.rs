// SPDX-License-Identifier: AGPL-3.0-only
//! Mailbox ids and sealed push tokens (design B.4.9).

use pingers_proto::{MailboxId, PushTokenPlaintext, SealedPushBlob, SubscriptionId};
use rand_core::CryptoRng;
use sha2::{Digest as _, Sha256};

use crate::CryptoError;
use crate::labels;
use crate::secret::{random_secret, secret_key};
use crate::stub::not_implemented;

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

/// `bind`: ties a sealed push to one mailbox or subscription, so the server cannot move it
/// (design B.4.9). The relay is given this hash, never the id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PushBind([u8; 32]);

impl PushBind {
    /// Wraps bind bytes received by the push relay.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The bind bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// `bind = SHA-256("pingers/v1/push-bind" || mailbox_id)`.
#[must_use]
pub fn push_bind_for_mailbox(mailbox: &MailboxId) -> PushBind {
    push_bind(mailbox.as_bytes())
}

/// The bind of a group subscription: as for a mailbox, with the subscription id in place of
/// the mailbox id (design D).
#[must_use]
pub fn push_bind_for_subscription(subscription: &SubscriptionId) -> PushBind {
    push_bind(subscription.as_bytes())
}

fn push_bind(id: &[u8; 16]) -> PushBind {
    let digest = Sha256::new()
        .chain_update(labels::PUSH_BIND)
        .chain_update(id)
        .finalize();
    let mut bind = [0; 32];
    for (target, source) in bind.iter_mut().zip(digest.iter()) {
        *target = *source;
    }
    PushBind(bind)
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

/// Seals a push token to the relay: `HPKE.Seal(pk, info = "pingers/v1/push" || bind,
/// padded plaintext)`, giving exactly 320 bytes. Fresh randomness every time, so two blobs
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
    not_implemented((relay_key, token, rng))
}

/// A push token and the mailbox or subscription it is bound to.
#[derive(Clone, Copy, Debug)]
pub struct PushTokenToSeal<'token> {
    /// The bind of the mailbox or subscription.
    pub bind: PushBind,
    /// Platform, class, token and expiry.
    pub plaintext: &'token PushTokenPlaintext,
}

/// Opens a sealed push token on the relay. Fails if `bind` is not the one it was sealed with.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if the blob does not open under this key and bind;
/// [`CryptoError::Encoding`] if the plaintext is malformed.
pub fn open_push_token(
    relay_key: &PushRelayPrivateKey,
    bind: &PushBind,
    blob: &SealedPushBlob,
) -> Result<PushTokenPlaintext, CryptoError> {
    not_implemented((relay_key, bind, blob))
}
