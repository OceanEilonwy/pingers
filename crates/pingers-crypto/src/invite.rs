// SPDX-License-Identifier: AGPL-3.0-only
//! Invite join requests (design D, invite flow).

use pingers_proto::SizeClass;
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::primitives::{self, XNONCE_LENGTH};
use crate::secret::{random_secret, secret_key};
use crate::{CryptoError, labels, padded};

secret_key!(
    /// The 32-byte secret in an invite link's fragment. Possession authenticates a join
    /// request. `from_bytes` and `expose_bytes` are public because pingers-parties puts it
    /// into and reads it from the link, and pingers-store keeps it until the invite expires.
    InviteSecret,
    32,
    pub
);
random_secret!(InviteSecret);

/// Seals a join request to an invite, padded inside the AEAD to the smallest mailbox size
/// class that fits (1, 4, 8 or 32 KiB): `nonce (24, random) ||
/// XChaCha20-Poly1305(HKDF(invite_secret, "join"), be16(len) || request || zero padding)`.
/// The post is exactly one class long, so it reveals only the class, not the request size.
///
/// # Errors
///
/// [`CryptoError::WrongLength`] if the request does not fit 32 KiB;
/// [`CryptoError::Internal`] if HKDF or the AEAD fails.
pub fn seal_join_request(
    secret: &InviteSecret,
    encoded_request: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, CryptoError> {
    let class = SizeClass::MAILBOX_POST
        .into_iter()
        .find(|class| encoded_request.len() <= padded::capacity(*class))
        .ok_or(CryptoError::WrongLength {
            expected: padded::capacity(SizeClass::Bytes32768),
            actual: encoded_request.len(),
        })?;
    seal_join_request_in_class(secret, encoded_request, class, rng)
}

/// As [`seal_join_request`], padded to `class`, for callers that want every join request of a
/// Party to look the same (for example 4 KiB when X-Wing key packages are possible).
///
/// # Errors
///
/// [`CryptoError::WrongLength`] if the request does not fit `class`;
/// [`CryptoError::Internal`] if HKDF or the AEAD fails.
pub fn seal_join_request_in_class(
    secret: &InviteSecret,
    encoded_request: &[u8],
    class: SizeClass,
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0; XNONCE_LENGTH];
    rng.fill_bytes(&mut nonce);
    padded::seal(&*join_key(secret)?, &nonce, encoded_request, class)
}

/// `HKDF-Expand(invite_secret, "join", 32)`: the invite secret is already uniformly random,
/// so it serves as the PRK, as the Spot keys do.
pub(crate) fn join_key(secret: &InviteSecret) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    primitives::hkdf_expand_32(secret.expose_bytes(), &[labels::INVITE_JOIN])
}

/// Opens a join request posted to the invite mailbox and strips its padding.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if it was not sealed with this invite's secret;
/// [`CryptoError::WrongLength`] if the padding inside is malformed.
pub fn open_join_request(
    secret: &InviteSecret,
    sealed: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, CryptoError> {
    padded::open(&*join_key(secret)?, sealed)
}
