// SPDX-License-Identifier: AGPL-3.0-only
//! The presence key schedule, board beacons and knocks (design B.4.1, B.4.2, B.5).
#![expect(
    dead_code,
    reason = "the crate-internal key types are constructed once the stub bodies are implemented"
)]

use pingers_proto::{
    BoardBeacon, Epoch, KeyPackageBytes, KnockSignature, MailboxId, PartyId, Record,
    RecordRandomness, RecordTag, SealedBeacon, SizeClass, SpotId, Subscription,
};
use pingers_time::Bucket;
use rand_core::CryptoRng;

use crate::secret::secret_key;
use crate::stub::not_implemented;
use crate::{AccountPublicKey, AccountSigningKey, CryptoError};

secret_key!(
    /// `PR_e = MLS-Exporter("pingers/v1/presence", party_id, 32)`: the root of every Spot key
    /// of a Party at one epoch (design B.4.1).
    ///
    /// `from_bytes` is public because pingers-mls produces it from the exporter.
    PresenceRoot,
    32,
    pub
);

secret_key!(
    /// `K_spot = HKDF-Expand(PR_e, "spot" || spot_id, 32)`.
    SpotKey,
    32,
    pub(crate)
);

secret_key!(
    /// `K_tag = HKDF-Expand(K_spot, "tag", 32)`. Derived only while the device is Inside the
    /// Spot and dropped (zeroed) when it leaves (design B.4.3). The implementer may hold the
    /// precomputed HMAC key state here instead of the raw bytes.
    TagKey,
    32,
    pub(crate)
);

secret_key!(
    /// `K_seal(b0) = HKDF-Expand(K_spot, "seal" || be64(b0), 32)`: seals the board beacons of
    /// one start bucket.
    SealKey,
    32,
    pub(crate)
);

secret_key!(
    /// `K_knock(b0, r) = HKDF-Expand(K_seal(b0), "knock" || r, 32)`: seals knocks answering
    /// the record with randomness `r`.
    KnockKey,
    32,
    pub(crate)
);

secret_key!(
    /// `PSK_meet(b) = HKDF-Expand(K_spot, "psk" || be64(b), 32)`: the external PSK that binds
    /// a Meetup group to its Party and Spot (design B.5).
    ///
    /// `expose_bytes` is public because pingers-mls must hand the value to `OpenMLS`.
    MeetupPsk,
    32,
    pub
);

/// Derives a Spot's key from the Party's presence root.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HKDF rejects its inputs (not possible with these lengths).
pub fn derive_spot_key(root: &PresenceRoot, spot: &SpotId) -> Result<SpotKey, CryptoError> {
    not_implemented((root, spot))
}

/// Derives a Spot's tag key.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HKDF rejects its inputs.
pub fn derive_tag_key(spot_key: &SpotKey) -> Result<TagKey, CryptoError> {
    not_implemented(spot_key)
}

/// `tag(b0, r) = HMAC-SHA256(K_tag, be64(b0) || r)[..16]`.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HMAC rejects the key.
pub fn record_tag(
    tag_key: &TagKey,
    start: Bucket,
    randomness: &RecordRandomness,
) -> Result<RecordTag, CryptoError> {
    not_implemented((tag_key, start, randomness))
}

/// Whether `record`'s tag is `tag(start, record.r)` under `tag_key`, compared in constant
/// time. This is the inner loop of trial matching (design B.4.3); keep it allocation-free.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HMAC rejects the key.
pub fn tag_matches(tag_key: &TagKey, start: Bucket, record: &Record) -> Result<bool, CryptoError> {
    not_implemented((tag_key, start, record))
}

/// Derives the beacon seal key for one start bucket.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HKDF rejects its inputs.
pub fn derive_seal_key(spot_key: &SpotKey, start: Bucket) -> Result<SealKey, CryptoError> {
    not_implemented((spot_key, start))
}

/// Seals a board beacon: `XChaCha20-Poly1305(K_seal(b0), nonce = r || 0⁸,
/// aad = "pingers/v1/board" || be64(b0) || r || tag, beacon)` (design B.4.2).
///
/// Deterministic: the nonce comes from `r`, which must be fresh per record.
///
/// # Errors
///
/// [`CryptoError::Internal`] if the AEAD fails.
pub fn seal_board_beacon(
    seal_key: &SealKey,
    start: Bucket,
    randomness: &RecordRandomness,
    tag: &RecordTag,
    beacon: &BoardBeacon,
) -> Result<SealedBeacon, CryptoError> {
    not_implemented((seal_key, start, randomness, tag, beacon))
}

/// Opens a matched record's beacon.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if the record was not sealed under this key and bucket;
/// [`CryptoError::Encoding`] if the plaintext is not a valid beacon.
pub fn open_board_beacon(
    seal_key: &SealKey,
    start: Bucket,
    record: &Record,
) -> Result<BoardBeacon, CryptoError> {
    not_implemented((seal_key, start, record))
}

/// Derives the meetup PSK for bucket `bucket`.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HKDF rejects its inputs.
pub fn derive_meetup_psk(spot_key: &SpotKey, bucket: Bucket) -> Result<MeetupPsk, CryptoError> {
    not_implemented((spot_key, bucket))
}

/// Derives the key that seals knocks answering the record with randomness `randomness`.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HKDF rejects its inputs.
pub fn derive_knock_key(
    seal_key: &SealKey,
    randomness: &RecordRandomness,
) -> Result<KnockKey, CryptoError> {
    not_implemented((seal_key, randomness))
}

/// Seals an encoded knock so that the mailbox post is exactly `class` bytes:
/// `nonce (24, random) || XChaCha20-Poly1305(K_knock, be16(len) || knock || zero padding)`.
/// The classical suite uses 1 KiB, X-Wing Parties 4 KiB (design B.4.2).
///
/// # Errors
///
/// [`CryptoError::WrongLength`] if the knock does not fit the class after the 40 bytes of
/// nonce and tag; [`CryptoError::Internal`] if the AEAD fails.
pub fn seal_knock(
    knock_key: &KnockKey,
    encoded_knock: &[u8],
    class: SizeClass,
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, CryptoError> {
    not_implemented((knock_key, encoded_knock, class, rng))
}

/// Opens a mailbox post as a knock and returns the encoded knock without padding.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if it was not sealed under this key (including posts that are
/// not knocks at all); [`CryptoError::WrongLength`] if the padding is malformed.
pub fn open_knock(knock_key: &KnockKey, post: &[u8]) -> Result<Vec<u8>, CryptoError> {
    not_implemented((knock_key, post))
}

/// The fields a knock signature covers (design B.4.2):
/// `"pingers/v1/knock" || party_id || be64(epoch) || spot_id || be64(b0) || matched_r ||
/// SHA-256(key_package) || reply_mailbox || SHA-256(meetup_subscription)`, where an absent
/// subscription contributes 32 zero bytes.
#[derive(Clone, Copy, Debug)]
pub struct KnockSignedFields<'knock> {
    /// The Party.
    pub party: &'knock PartyId,
    /// The Party epoch the knocker is at.
    pub epoch: Epoch,
    /// The Spot.
    pub spot: &'knock SpotId,
    /// The matched record's start bucket.
    pub start: Bucket,
    /// The matched record's randomness.
    pub matched: &'knock RecordRandomness,
    /// The knock's `KeyPackage`, hashed into the signature.
    pub key_package: &'knock KeyPackageBytes,
    /// The knocker's reply mailbox.
    pub reply_mailbox: &'knock MailboxId,
    /// The knocker's Meetup subscription, hashed (in its canonical encoding) if present.
    pub meetup_subscription: Option<&'knock Subscription>,
}

/// Signs a knock with the knocker's leaf (account) key.
///
/// # Errors
///
/// [`CryptoError::Encoding`] if the subscription cannot be encoded for hashing.
pub fn sign_knock(
    signing_key: &AccountSigningKey,
    fields: &KnockSignedFields<'_>,
) -> Result<KnockSignature, CryptoError> {
    not_implemented((signing_key, fields))
}

/// Verifies a knock's signature against the knocker's key from the Party roster.
///
/// # Errors
///
/// [`CryptoError::BadSignature`] if it does not verify; [`CryptoError::Encoding`] if the
/// subscription cannot be encoded for hashing.
pub fn verify_knock(
    public_key: &AccountPublicKey,
    fields: &KnockSignedFields<'_>,
    signature: &KnockSignature,
) -> Result<(), CryptoError> {
    not_implemented((public_key, fields, signature))
}
