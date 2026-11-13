// SPDX-License-Identifier: AGPL-3.0-only
//! Exact-location consent inside a Meetup: location keys, grants and asks (design B.6.1).

use core::fmt;

use pingers_proto::{
    Epoch, GroupId, LeafIndex, LocationGeneration, LocationPlaintext, LocationRequestId, SealedAsk,
    SealedGrant, SealedLocationFrame,
};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;

use crate::CryptoError;
use crate::secret::{random_secret, secret_key};
use crate::stub::not_implemented;

secret_key!(
    /// A member's location key `LK_self` for one generation (design B.6.1). AES-256-GCM.
    /// `from_bytes` and `expose_bytes` are public for pingers-store (`meetup_self_lk`,
    /// `meetup_peer`), whose rows are deleted when the Meetup ends.
    LocationKey,
    32,
    pub
);
random_secret!(LocationKey);

secret_key!(
    /// The secret half of a member's per-Meetup grant key pair (X25519 HPKE; X-Wing under the
    /// `x-wing` feature). `from_bytes` and `expose_bytes` are public for pingers-store.
    GrantSecretKey,
    32,
    pub
);

/// The public half of a grant key pair, published in the `pingers_grant_key` leaf extension
/// of a member's Meetup `KeyPackage` (design B.6.1). `Debug` does not print it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GrantPublicKey(Vec<u8>);

impl GrantPublicKey {
    /// Wraps key bytes read from a leaf extension. Validity is checked when sealing.
    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The key bytes, for the leaf extension.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for GrantPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GrantPublicKey(redacted)")
    }
}

/// A fresh grant key pair, generated when joining a Meetup.
pub struct GrantKeyPair {
    /// The secret half, kept on the device until the Meetup ends.
    pub secret: GrantSecretKey,
    /// The public half, for the `KeyPackage` extension.
    pub public: GrantPublicKey,
}

impl GrantKeyPair {
    /// Generates a key pair.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Internal`] if key generation fails.
    pub fn generate(rng: &mut impl CryptoRng) -> Result<Self, CryptoError> {
        not_implemented(rng)
    }
}

/// The associated data of a sealed location: `group_id || be64(epoch) || be32(sender_leaf)
/// || be32(lk_gen)` (design B.6.1).
#[derive(Clone, Copy, Debug)]
pub struct LocationAad<'group> {
    /// The Meetup group.
    pub group: &'group GroupId,
    /// The MLS epoch the frame is sent in.
    pub epoch: Epoch,
    /// The sender's leaf.
    pub sender: LeafIndex,
    /// The location key generation.
    pub generation: LocationGeneration,
}

/// Length of a sealed location's ciphertext (padded plaintext plus tag). Real and dummy
/// frames both have exactly this length, so they cannot be told apart.
pub const SEALED_LOCATION_LENGTH: usize = 80;

/// Seals a position under the sender's location key. The plaintext is padded to a fixed
/// length first, so optional fields do not change the ciphertext length.
///
/// # Errors
///
/// [`CryptoError::Encoding`] if the plaintext cannot be encoded;
/// [`CryptoError::Internal`] if the AEAD fails.
pub fn seal_location(
    key: &LocationKey,
    aad: &LocationAad<'_>,
    location: &LocationPlaintext,
    rng: &mut impl CryptoRng,
) -> Result<SealedLocationFrame, CryptoError> {
    not_implemented((key, aad, location, rng))
}

/// A frame indistinguishable from a real sealed location: random nonce and ciphertext of
/// [`SEALED_LOCATION_LENGTH`] bytes under a generation nobody holds (design B.6.1).
#[must_use]
pub fn dummy_location(
    generation: LocationGeneration,
    rng: &mut impl CryptoRng,
) -> SealedLocationFrame {
    let mut nonce = [0; 12];
    rng.fill_bytes(&mut nonce);
    let mut ciphertext = vec![0; SEALED_LOCATION_LENGTH];
    rng.fill_bytes(&mut ciphertext);
    SealedLocationFrame {
        generation,
        nonce,
        ciphertext,
    }
}

/// Opens a peer's sealed location with the key they granted.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if the frame is not sealed under this key (including dummy
/// frames); [`CryptoError::Encoding`] if the plaintext is malformed.
pub fn open_location(
    key: &LocationKey,
    aad: &LocationAad<'_>,
    frame: &SealedLocationFrame,
) -> Result<LocationPlaintext, CryptoError> {
    not_implemented((key, aad, frame))
}

/// The HPKE info of a grant: `"pingers/v1/lk-grant" || group_id || be32(sender_leaf)`.
#[derive(Clone, Copy, Debug)]
pub struct GrantContext<'group> {
    /// The Meetup group.
    pub group: &'group GroupId,
    /// The granting member's leaf.
    pub sender: LeafIndex,
}

/// What a grant carries: `lk_gen || LK_self || expires_at` (design B.6.1).
pub struct LocationGrant<'key> {
    /// The generation being granted.
    pub generation: LocationGeneration,
    /// The key for that generation.
    pub key: &'key LocationKey,
    /// When the grant lapses (the Meetup's end, or 15 minutes from now).
    pub expires_at: UnixSeconds,
}

/// A grant opened by its recipient.
pub struct OpenedGrant {
    /// The granted generation.
    pub generation: LocationGeneration,
    /// The granter's location key for that generation.
    pub key: LocationKey,
    /// When the grant lapses.
    pub expires_at: UnixSeconds,
}

/// Seals a grant to one member's grant key. Carries no recipient field.
///
/// # Errors
///
/// [`CryptoError::InvalidKey`] if the recipient key is invalid;
/// [`CryptoError::Internal`] if HPKE fails.
pub fn seal_location_grant(
    recipient: &GrantPublicKey,
    context: &GrantContext<'_>,
    grant: &LocationGrant<'_>,
    rng: &mut impl CryptoRng,
) -> Result<SealedGrant, CryptoError> {
    not_implemented((recipient, context, grant, rng))
}

/// Trial-opens a grant with this member's grant key. Most grants are for someone else, so
/// [`CryptoError::OpenFailed`] is the normal result.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if the grant is not for this key;
/// [`CryptoError::WrongLength`] if the plaintext is malformed.
pub fn open_location_grant(
    own_key: &GrantSecretKey,
    context: &GrantContext<'_>,
    sealed: &SealedGrant,
) -> Result<OpenedGrant, CryptoError> {
    not_implemented((own_key, context, sealed))
}

/// What an ask carries: `requester_leaf || req_id || ts` (design B.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocationAsk {
    /// Who is asking.
    pub requester: LeafIndex,
    /// The request's id, for the answer and its expiry.
    pub request: LocationRequestId,
    /// When it was asked.
    pub asked_at: UnixSeconds,
}

/// Seals an ask to the target's grant key, with HPKE info
/// `"pingers/v1/lk-ask" || group_id`.
///
/// # Errors
///
/// [`CryptoError::InvalidKey`] if the target key is invalid;
/// [`CryptoError::Internal`] if HPKE fails.
pub fn seal_location_ask(
    target: &GrantPublicKey,
    group: &GroupId,
    ask: &LocationAsk,
    rng: &mut impl CryptoRng,
) -> Result<SealedAsk, CryptoError> {
    not_implemented((target, group, ask, rng))
}

/// Trial-opens an ask with this member's grant key.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if the ask is not for this key;
/// [`CryptoError::WrongLength`] if the plaintext is malformed.
pub fn open_location_ask(
    own_key: &GrantSecretKey,
    group: &GroupId,
    sealed: &SealedAsk,
) -> Result<LocationAsk, CryptoError> {
    not_implemented((own_key, group, sealed))
}
