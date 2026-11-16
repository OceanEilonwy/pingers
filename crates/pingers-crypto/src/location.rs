// SPDX-License-Identifier: AGPL-3.0-only
//! Exact-location consent inside a Meetup: location keys, grants and asks (design B.6.1).

use core::fmt;

use pingers_proto::{
    Epoch, GroupId, LeafIndex, LocationGeneration, LocationPlaintext, LocationRequestId, SealedAsk,
    SealedGrant, SealedLocationFrame, WireDecode as _, WireEncode as _,
};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::hpke::{self, Recipient, Suite};
use crate::primitives::{self, GCM_NONCE_LENGTH, TAG_LENGTH};
use crate::secret::{random_secret, secret_key};
use crate::{CryptoError, labels};

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
    /// `x-wing` feature, where these 32 bytes are the X-Wing seed). `from_bytes` and
    /// `expose_bytes` are public for pingers-store.
    GrantSecretKey,
    32,
    pub
);

/// The public half of a grant key pair, published in the `pingers_grant_key` leaf extension
/// of a member's Meetup `KeyPackage` (design B.6.1). `Debug` does not print it.
///
/// 32 bytes for X25519, 1,216 for X-Wing; the length selects the HPKE suite when sealing.
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
    /// Generates an X25519 key pair: RFC 9180 `DeriveKeyPair` over 32 bytes from `rng`.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Internal`] if key generation fails.
    pub fn generate(rng: &mut impl CryptoRng) -> Result<Self, CryptoError> {
        Self::generate_for(Suite::Grant, rng)
    }

    /// Generates a throwaway key pair of the same suite as `like`, which selects it by its
    /// length: a dummy grant sealed to it has exactly the length of a real grant sealed to
    /// `like` (design B.6.1, "Uniform traffic").
    ///
    /// # Errors
    ///
    /// [`CryptoError::InvalidKey`] if `like` is of no suite this build supports;
    /// [`CryptoError::Internal`] if key generation fails.
    pub fn generate_like(
        like: &GrantPublicKey,
        rng: &mut impl CryptoRng,
    ) -> Result<Self, CryptoError> {
        Self::generate_for(suite_for_public_key(like)?, rng)
    }

    /// Generates an X-Wing key pair, for Meetups of post-quantum Parties (design B.8).
    ///
    /// # Errors
    ///
    /// [`CryptoError::Internal`] if key generation fails.
    #[cfg(feature = "x-wing")]
    pub fn generate_x_wing(rng: &mut impl CryptoRng) -> Result<Self, CryptoError> {
        Self::generate_for(Suite::GrantXWing, rng)
    }

    fn generate_for(suite: Suite, rng: &mut impl CryptoRng) -> Result<Self, CryptoError> {
        let mut ikm = Zeroizing::new([0; 32]);
        rng.fill_bytes(ikm.as_mut_slice());
        Self::derive(suite, &ikm)
    }

    /// RFC 9180 `DeriveKeyPair(ikm)`.
    fn derive(suite: Suite, ikm: &[u8; 32]) -> Result<Self, CryptoError> {
        let pair = hpke::derive_key_pair(suite, ikm)?;
        Ok(Self {
            secret: GrantSecretKey::from_bytes(pair.secret),
            public: GrantPublicKey::from_bytes(pair.public),
        })
    }
}

/// The grant suites this build supports.
const GRANT_SUITES: &[Suite] = &[
    Suite::Grant,
    #[cfg(feature = "x-wing")]
    Suite::GrantXWing,
];

/// The suite a sealed grant or ask was made with, told apart by its length: the encapsulated
/// key is 32 bytes for X25519 and 1,120 for X-Wing.
fn suite_for_sealed(sealed_length: usize, plaintext_length: usize) -> Result<Suite, CryptoError> {
    GRANT_SUITES
        .iter()
        .copied()
        .find(|suite| suite.overhead().checked_add(plaintext_length) == Some(sealed_length))
        .ok_or(CryptoError::OpenFailed)
}

/// The suite of a recipient public key, told apart by its length.
fn suite_for_public_key(public_key: &GrantPublicKey) -> Result<Suite, CryptoError> {
    GRANT_SUITES
        .iter()
        .copied()
        .find(|suite| suite.public_key_length() == public_key.as_bytes().len())
        .ok_or(CryptoError::InvalidKey)
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

/// The padded plaintext inside a sealed location: `be16(length) || encoding || zero bytes`,
/// the padding format of pingers-proto.
pub(crate) const PADDED_LOCATION_LENGTH: usize = SEALED_LOCATION_LENGTH - TAG_LENGTH;
/// The length prefix of the padded plaintext.
const LENGTH_PREFIX: usize = 2;

/// `group_id || be64(epoch) || be32(sender_leaf) || be32(lk_gen)`.
fn location_aad(aad: &LocationAad<'_>) -> Vec<u8> {
    [
        aad.group.as_bytes().as_slice(),
        &aad.epoch.get().to_be_bytes(),
        &aad.sender.get().to_be_bytes(),
        &aad.generation.get().to_be_bytes(),
    ]
    .concat()
}

pub(crate) fn pad_location(
    encoded: &[u8],
) -> Result<Zeroizing<[u8; PADDED_LOCATION_LENGTH]>, CryptoError> {
    let too_long = || CryptoError::WrongLength {
        expected: PADDED_LOCATION_LENGTH - LENGTH_PREFIX,
        actual: encoded.len(),
    };
    let length = u16::try_from(encoded.len()).map_err(|_overflow| too_long())?;
    let mut padded = Zeroizing::new([0; PADDED_LOCATION_LENGTH]);
    let (prefix, body) = padded.split_at_mut(LENGTH_PREFIX);
    prefix.copy_from_slice(&length.to_be_bytes());
    body.get_mut(..encoded.len())
        .ok_or_else(too_long)?
        .copy_from_slice(encoded);
    Ok(padded)
}

pub(crate) fn unpad_location(padded: &[u8]) -> Result<&[u8], CryptoError> {
    let bad_padding = || CryptoError::Encoding(pingers_proto::ProtoError::BadPadding);
    let (length, body) = padded
        .split_first_chunk::<LENGTH_PREFIX>()
        .ok_or_else(bad_padding)?;
    let (encoded, padding) = body
        .split_at_checked(usize::from(u16::from_be_bytes(*length)))
        .ok_or_else(bad_padding)?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err(bad_padding());
    }
    Ok(encoded)
}

pub(crate) fn seal_padded_location(
    key: &LocationKey,
    aad: &LocationAad<'_>,
    padded: &[u8; PADDED_LOCATION_LENGTH],
    nonce: [u8; GCM_NONCE_LENGTH],
) -> Result<SealedLocationFrame, CryptoError> {
    let ciphertext =
        primitives::aes_gcm_seal(key.expose_bytes(), &nonce, &location_aad(aad), padded)?;
    Ok(SealedLocationFrame {
        generation: aad.generation,
        nonce,
        ciphertext,
    })
}

pub(crate) fn open_padded_location(
    key: &LocationKey,
    aad: &LocationAad<'_>,
    frame: &SealedLocationFrame,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if frame.ciphertext.len() != SEALED_LOCATION_LENGTH {
        return Err(CryptoError::WrongLength {
            expected: SEALED_LOCATION_LENGTH,
            actual: frame.ciphertext.len(),
        });
    }
    primitives::aes_gcm_open(
        key.expose_bytes(),
        &frame.nonce,
        &location_aad(aad),
        &frame.ciphertext,
    )
}

/// Seals a position under the sender's location key. The plaintext is padded to a fixed
/// length first, so optional fields do not change the ciphertext length.
///
/// # Errors
///
/// [`CryptoError::Encoding`] if the plaintext cannot be encoded;
/// [`CryptoError::WrongLength`] if its encoding does not fit the padded length;
/// [`CryptoError::Internal`] if the AEAD fails.
pub fn seal_location(
    key: &LocationKey,
    frame: &LocationToSeal<'_>,
    rng: &mut impl CryptoRng,
) -> Result<SealedLocationFrame, CryptoError> {
    let encoded = Zeroizing::new(frame.location.encode().map_err(CryptoError::Encoding)?);
    let padded = pad_location(&encoded)?;
    let mut nonce = [0; GCM_NONCE_LENGTH];
    rng.fill_bytes(&mut nonce);
    seal_padded_location(key, &frame.aad, &padded, nonce)
}

/// A position and the associated data that binds it to its frame.
#[derive(Clone, Copy, Debug)]
pub struct LocationToSeal<'frame> {
    /// Group, epoch, sender and generation.
    pub aad: LocationAad<'frame>,
    /// The position.
    pub location: &'frame LocationPlaintext,
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

/// Opens a peer's sealed location with the key they granted. The generation bound in the
/// associated data is `aad.generation`, so a frame of another generation does not open.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if the frame is not sealed under this key (including dummy
/// frames); [`CryptoError::WrongLength`] if the ciphertext is not
/// [`SEALED_LOCATION_LENGTH`] bytes; [`CryptoError::Encoding`] if the plaintext is malformed.
pub fn open_location(
    key: &LocationKey,
    aad: &LocationAad<'_>,
    frame: &SealedLocationFrame,
) -> Result<LocationPlaintext, CryptoError> {
    let padded = open_padded_location(key, aad, frame)?;
    LocationPlaintext::decode(unpad_location(&padded)?).map_err(CryptoError::Encoding)
}

/// The HPKE info of a grant: `"pingers/v1/lk-grant" || group_id || be32(sender_leaf)`.
#[derive(Clone, Copy, Debug)]
pub struct GrantContext<'group> {
    /// The Meetup group.
    pub group: &'group GroupId,
    /// The granting member's leaf.
    pub sender: LeafIndex,
}

/// A grant to seal: the HPKE context and what it carries, `lk_gen || LK_self || expires_at`
/// (design B.6.1).
pub struct LocationGrant<'key> {
    /// The group and granting leaf, bound through the HPKE info.
    pub context: GrantContext<'key>,
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

/// `be32(lk_gen) || LK_self || be64(expires_at)`.
const GRANT_PLAINTEXT_LENGTH: usize = 4 + LocationKey::LENGTH + 8;

/// `"pingers/v1/lk-grant" || group_id || be32(sender_leaf)`.
fn grant_info(context: &GrantContext<'_>) -> Vec<u8> {
    [
        labels::LOCATION_GRANT,
        context.group.as_bytes().as_slice(),
        &context.sender.get().to_be_bytes(),
    ]
    .concat()
}

fn grant_plaintext(grant: &LocationGrant<'_>) -> Zeroizing<Vec<u8>> {
    let mut plaintext = Zeroizing::new(Vec::with_capacity(GRANT_PLAINTEXT_LENGTH));
    plaintext.extend_from_slice(&grant.generation.get().to_be_bytes());
    plaintext.extend_from_slice(grant.key.expose_bytes());
    plaintext.extend_from_slice(&grant.expires_at.get().to_be_bytes());
    plaintext
}

pub(crate) fn parse_grant(plaintext: &[u8]) -> Result<OpenedGrant, CryptoError> {
    let wrong_length = || CryptoError::WrongLength {
        expected: GRANT_PLAINTEXT_LENGTH,
        actual: plaintext.len(),
    };
    let (generation, rest) = plaintext
        .split_first_chunk::<4>()
        .ok_or_else(wrong_length)?;
    let (key, rest) = rest.split_first_chunk::<32>().ok_or_else(wrong_length)?;
    let expires_at: &[u8; 8] = rest.try_into().map_err(|_length| wrong_length())?;
    Ok(OpenedGrant {
        generation: LocationGeneration::new(u32::from_be_bytes(*generation)),
        key: LocationKey::from_bytes(Zeroizing::new(*key)),
        expires_at: UnixSeconds::new(u64::from_be_bytes(*expires_at)),
    })
}

/// Seals a grant to one member's grant key. Carries no recipient field. The HPKE suite
/// follows the key: X25519 for 32-byte keys, X-Wing for 1,216-byte keys (feature `x-wing`).
///
/// # Errors
///
/// [`CryptoError::InvalidKey`] if the recipient key is invalid;
/// [`CryptoError::Internal`] if HPKE fails.
pub fn seal_location_grant(
    recipient: &GrantPublicKey,
    grant: &LocationGrant<'_>,
    rng: &mut impl CryptoRng,
) -> Result<SealedGrant, CryptoError> {
    let suite = suite_for_public_key(recipient)?;
    hpke::seal(
        Recipient {
            suite,
            public_key: recipient.as_bytes(),
        },
        &grant_info(&grant.context),
        &grant_plaintext(grant),
        rng,
    )
    .map(SealedGrant::from_bytes)
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
    let suite = suite_for_sealed(sealed.as_bytes().len(), GRANT_PLAINTEXT_LENGTH)?;
    let plaintext = hpke::open(
        suite,
        own_key.expose_bytes(),
        &grant_info(context),
        sealed.as_bytes(),
    )?;
    parse_grant(&plaintext)
}

/// An ask: the group it is bound to (through the HPKE info), and what it carries,
/// `requester_leaf || req_id || ts` (design B.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocationAsk {
    /// The Meetup group.
    pub group: GroupId,
    /// Who is asking.
    pub requester: LeafIndex,
    /// The request's id, for the answer and its expiry.
    pub request: LocationRequestId,
    /// When it was asked.
    pub asked_at: UnixSeconds,
}

/// `be32(requester_leaf) || req_id || be64(ts)`.
const ASK_PLAINTEXT_LENGTH: usize = 4 + LocationRequestId::LENGTH + 8;

/// `"pingers/v1/lk-ask" || group_id`.
fn ask_info(group: &GroupId) -> Vec<u8> {
    [labels::LOCATION_ASK, group.as_bytes().as_slice()].concat()
}

fn ask_plaintext(ask: &LocationAsk) -> Vec<u8> {
    [
        ask.requester.get().to_be_bytes().as_slice(),
        ask.request.as_bytes(),
        &ask.asked_at.get().to_be_bytes(),
    ]
    .concat()
}

fn parse_ask(group: &GroupId, plaintext: &[u8]) -> Result<LocationAsk, CryptoError> {
    let wrong_length = || CryptoError::WrongLength {
        expected: ASK_PLAINTEXT_LENGTH,
        actual: plaintext.len(),
    };
    let (requester, rest) = plaintext
        .split_first_chunk::<4>()
        .ok_or_else(wrong_length)?;
    let (request, rest) = rest.split_first_chunk::<16>().ok_or_else(wrong_length)?;
    let asked_at: &[u8; 8] = rest.try_into().map_err(|_length| wrong_length())?;
    Ok(LocationAsk {
        group: *group,
        requester: LeafIndex::new(u32::from_be_bytes(*requester)),
        request: LocationRequestId::from_bytes(*request),
        asked_at: UnixSeconds::new(u64::from_be_bytes(*asked_at)),
    })
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
    ask: &LocationAsk,
    rng: &mut impl CryptoRng,
) -> Result<SealedAsk, CryptoError> {
    let suite = suite_for_public_key(target)?;
    hpke::seal(
        Recipient {
            suite,
            public_key: target.as_bytes(),
        },
        &ask_info(&ask.group),
        &ask_plaintext(ask),
        rng,
    )
    .map(SealedAsk::from_bytes)
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
    let suite = suite_for_sealed(sealed.as_bytes().len(), ASK_PLAINTEXT_LENGTH)?;
    let plaintext = hpke::open(
        suite,
        own_key.expose_bytes(),
        &ask_info(group),
        sealed.as_bytes(),
    )?;
    parse_ask(group, &plaintext)
}
