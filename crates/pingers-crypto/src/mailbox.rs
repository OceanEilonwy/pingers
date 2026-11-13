// SPDX-License-Identifier: AGPL-3.0-only
//! Mailbox ids and sealed push tokens (design B.4.9).

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
