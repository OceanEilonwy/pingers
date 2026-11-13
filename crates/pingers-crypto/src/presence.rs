// SPDX-License-Identifier: AGPL-3.0-only
//! The presence key schedule, board beacons and knocks (design B.4.1, B.4.2, B.5).

use pingers_proto::{
    BoardBeacon, Epoch, KeyPackageBytes, KnockSignature, MailboxId, PartyId, Record,
    RecordRandomness, RecordTag, SealedBeacon, SizeClass, SpotId, Subscription,
};
use pingers_time::Bucket;
use rand_core::CryptoRng;

use pingers_proto::WireEncode as _;
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::primitives::{self, TAG_LENGTH, XNONCE_LENGTH};
use crate::secret::secret_key;
use crate::{AccountPublicKey, AccountSigningKey, CryptoError, labels};

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
    primitives::hkdf_expand_32(root.expose_bytes(), &[labels::SPOT, spot.as_bytes()])
        .map(SpotKey::from_bytes)
}

/// Derives a Spot's tag key.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HKDF rejects its inputs.
pub fn derive_tag_key(spot_key: &SpotKey) -> Result<TagKey, CryptoError> {
    primitives::hkdf_expand_32(spot_key.expose_bytes(), &[labels::TAG]).map(TagKey::from_bytes)
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
    tag_bytes(tag_key, start, randomness).map(RecordTag::from_bytes)
}

fn tag_bytes(
    tag_key: &TagKey,
    start: Bucket,
    randomness: &RecordRandomness,
) -> Result<[u8; 16], CryptoError> {
    let mac = primitives::hmac_sha256(
        tag_key.expose_bytes(),
        &[&start.to_be_bytes(), randomness.as_bytes()],
    )?;
    primitives::prefix::<16>(&mac)
}

/// Whether `record`'s tag is `tag(start, record.r)` under `tag_key`, compared in constant
/// time. This is the inner loop of trial matching (design B.4.3); keep it allocation-free.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HMAC rejects the key.
pub fn tag_matches(tag_key: &TagKey, start: Bucket, record: &Record) -> Result<bool, CryptoError> {
    let expected = tag_bytes(tag_key, start, &record.randomness)?;
    Ok(expected.ct_eq(record.tag.as_bytes()).into())
}

/// Derives the beacon seal key for one start bucket.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HKDF rejects its inputs.
pub fn derive_seal_key(spot_key: &SpotKey, start: Bucket) -> Result<SealKey, CryptoError> {
    primitives::hkdf_expand_32(
        spot_key.expose_bytes(),
        &[labels::SEAL, &start.to_be_bytes()],
    )
    .map(SealKey::from_bytes)
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
    header: &BeaconHeader<'_>,
    beacon: &BoardBeacon,
) -> Result<SealedBeacon, CryptoError> {
    let sealed = primitives::xchacha_seal(
        seal_key.expose_bytes(),
        &beacon_nonce(header.randomness),
        &beacon_aad(header.start, header.randomness, header.tag),
        &beacon.to_bytes(),
    )?;
    let bytes: [u8; 64] = sealed
        .as_slice()
        .try_into()
        .map_err(|_length| CryptoError::Internal)?;
    Ok(SealedBeacon::from_bytes(bytes))
}

/// `r || 0⁸`.
fn beacon_nonce(randomness: &RecordRandomness) -> [u8; XNONCE_LENGTH] {
    let mut nonce = [0; XNONCE_LENGTH];
    let (prefix, _zeros) = nonce.split_at_mut(RecordRandomness::LENGTH);
    prefix.copy_from_slice(randomness.as_bytes());
    nonce
}

/// `"pingers/v1/board" || be64(b0) || r || tag`.
fn beacon_aad(start: Bucket, randomness: &RecordRandomness, tag: &RecordTag) -> Vec<u8> {
    [
        labels::BOARD,
        &start.to_be_bytes(),
        randomness.as_bytes(),
        tag.as_bytes(),
    ]
    .concat()
}

/// The parts of a record a beacon seal binds: its start bucket, randomness `r` (which also
/// gives the nonce) and tag.
#[derive(Clone, Copy, Debug)]
pub struct BeaconHeader<'record> {
    /// The lease's start bucket.
    pub start: Bucket,
    /// The record's randomness.
    pub randomness: &'record RecordRandomness,
    /// The record's tag.
    pub tag: &'record RecordTag,
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
    let plaintext = primitives::xchacha_open(
        seal_key.expose_bytes(),
        &beacon_nonce(&record.randomness),
        &beacon_aad(start, &record.randomness, &record.tag),
        record.sealed.as_bytes(),
    )?;
    let bytes: &[u8; BoardBeacon::SIZE] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_length| CryptoError::Internal)?;
    BoardBeacon::from_bytes(bytes).map_err(CryptoError::Encoding)
}

/// Derives the meetup PSK for bucket `bucket`.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HKDF rejects its inputs.
pub fn derive_meetup_psk(spot_key: &SpotKey, bucket: Bucket) -> Result<MeetupPsk, CryptoError> {
    primitives::hkdf_expand_32(
        spot_key.expose_bytes(),
        &[labels::PSK, &bucket.to_be_bytes()],
    )
    .map(MeetupPsk::from_bytes)
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
    primitives::hkdf_expand_32(
        seal_key.expose_bytes(),
        &[labels::KNOCK, randomness.as_bytes()],
    )
    .map(KnockKey::from_bytes)
}

/// Seals an encoded knock so that the mailbox post is exactly `knock.class` bytes:
/// `nonce (24, random) || XChaCha20-Poly1305(K_knock, be16(len) || knock || zero padding)`.
/// The classical suite uses 1 KiB, X-Wing Parties 4 KiB (design B.4.2).
///
/// # Errors
///
/// [`CryptoError::WrongLength`] if the knock does not fit the class after the 40 bytes of
/// nonce and tag; [`CryptoError::Internal`] if the AEAD fails.
pub fn seal_knock(
    knock_key: &KnockKey,
    knock: &KnockPlaintext<'_>,
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0; XNONCE_LENGTH];
    rng.fill_bytes(&mut nonce);
    seal_knock_with_nonce(knock_key, knock, &nonce)
}

/// The bytes sealing adds around the padded plaintext: nonce and tag.
const KNOCK_OVERHEAD: usize = XNONCE_LENGTH + TAG_LENGTH;
/// The big-endian length prefix inside the padded plaintext.
const KNOCK_LENGTH_PREFIX: usize = 2;

fn seal_knock_with_nonce(
    knock_key: &KnockKey,
    knock: &KnockPlaintext<'_>,
    nonce: &[u8; XNONCE_LENGTH],
) -> Result<Vec<u8>, CryptoError> {
    let padded_length = knock.class.size().saturating_sub(KNOCK_OVERHEAD);
    let capacity = padded_length.saturating_sub(KNOCK_LENGTH_PREFIX);
    let too_long = CryptoError::WrongLength {
        expected: capacity,
        actual: knock.encoded.len(),
    };
    if knock.encoded.len() > capacity {
        return Err(too_long);
    }
    let length = u16::try_from(knock.encoded.len()).map_err(|_overflow| too_long)?;
    let mut padded = Zeroizing::new(Vec::with_capacity(padded_length));
    padded.extend_from_slice(&length.to_be_bytes());
    padded.extend_from_slice(knock.encoded);
    padded.resize(padded_length, 0);
    let ciphertext = primitives::xchacha_seal(knock_key.expose_bytes(), nonce, &[], &padded)?;
    let mut post = Vec::with_capacity(knock.class.size());
    post.extend_from_slice(nonce);
    post.extend_from_slice(&ciphertext);
    Ok(post)
}

/// An encoded knock and the size class its sealed form must fill.
#[derive(Clone, Copy, Debug)]
pub struct KnockPlaintext<'knock> {
    /// The canonical encoding of the knock.
    pub encoded: &'knock [u8],
    /// The mailbox post size: 1 KiB, or 4 KiB for X-Wing Parties.
    pub class: SizeClass,
}

/// Opens a mailbox post as a knock and returns the encoded knock without padding.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if it was not sealed under this key (including posts that are
/// not knocks at all); [`CryptoError::WrongLength`] if the padding is malformed.
pub fn open_knock(knock_key: &KnockKey, post: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let (nonce, ciphertext) = post
        .split_first_chunk::<XNONCE_LENGTH>()
        .ok_or(CryptoError::OpenFailed)?;
    let padded = primitives::xchacha_open(knock_key.expose_bytes(), nonce, &[], ciphertext)?;
    let (length, rest) =
        padded
            .split_first_chunk::<KNOCK_LENGTH_PREFIX>()
            .ok_or(CryptoError::WrongLength {
                expected: KNOCK_LENGTH_PREFIX,
                actual: padded.len(),
            })?;
    let length = usize::from(u16::from_be_bytes(*length));
    let (knock, padding) = rest
        .split_at_checked(length)
        .ok_or(CryptoError::WrongLength {
            expected: length,
            actual: rest.len(),
        })?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err(CryptoError::WrongLength {
            expected: length,
            actual: rest.len(),
        });
    }
    Ok(knock.to_vec())
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
    let message = knock_message(fields, &subscription_hash(fields.meetup_subscription)?);
    Ok(KnockSignature::from_bytes(primitives::ed25519_sign(
        signing_key.expose_bytes(),
        &message,
    )))
}

/// `SHA-256` of the subscription's canonical encoding, or 32 zero bytes without one.
fn subscription_hash(subscription: Option<&Subscription>) -> Result<[u8; 32], CryptoError> {
    subscription.map_or(Ok([0; 32]), |subscription| {
        let encoded = subscription.encode().map_err(CryptoError::Encoding)?;
        Ok(primitives::sha256(&[&encoded]))
    })
}

/// The signed bytes, with the subscription already hashed (CONTRACT.md, "Knock signature
/// input"). Every field has a fixed width, so no length prefixes are needed.
fn knock_message(fields: &KnockSignedFields<'_>, subscription_hash: &[u8; 32]) -> Vec<u8> {
    [
        labels::KNOCK_SIGNATURE,
        fields.party.as_bytes(),
        &fields.epoch.get().to_be_bytes(),
        fields.spot.as_bytes(),
        &fields.start.to_be_bytes(),
        fields.matched.as_bytes(),
        &primitives::sha256(&[fields.key_package.as_bytes()]),
        fields.reply_mailbox.as_bytes(),
        subscription_hash,
    ]
    .concat()
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
    let message = knock_message(fields, &subscription_hash(fields.meetup_subscription)?);
    primitives::ed25519_verify(public_key.as_bytes(), &message, signature.as_bytes())
}
