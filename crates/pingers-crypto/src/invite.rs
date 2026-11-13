// SPDX-License-Identifier: AGPL-3.0-only
//! Invite join requests (design D, invite flow).

use rand_core::CryptoRng;

use zeroize::Zeroizing;

use crate::group::{open_with_nonce, seal_with_nonce};
use crate::primitives::{self, XNONCE_LENGTH};
use crate::secret::{random_secret, secret_key};
use crate::{CryptoError, labels};

secret_key!(
    /// The 32-byte secret in an invite link's fragment. Possession authenticates a join
    /// request. `from_bytes` and `expose_bytes` are public because pingers-parties puts it
    /// into and reads it from the link, and pingers-store keeps it until the invite expires.
    InviteSecret,
    32,
    pub
);
random_secret!(InviteSecret);

/// Seals a join request to an invite: `nonce (24, random) ||
/// XChaCha20-Poly1305(HKDF(invite_secret, "join"), request)`.
///
/// # Errors
///
/// [`CryptoError::Internal`] if HKDF or the AEAD fails.
pub fn seal_join_request(
    secret: &InviteSecret,
    encoded_request: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0; XNONCE_LENGTH];
    rng.fill_bytes(&mut nonce);
    seal_with_nonce(&*join_key(secret)?, &nonce, encoded_request)
}

/// `HKDF-Expand(invite_secret, "join", 32)`: the invite secret is already uniformly random,
/// so it serves as the PRK, as the Spot keys do.
pub(crate) fn join_key(secret: &InviteSecret) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    primitives::hkdf_expand_32(secret.expose_bytes(), &[labels::INVITE_JOIN])
}

/// Opens a join request posted to the invite mailbox.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if it was not sealed with this invite's secret.
pub fn open_join_request(
    secret: &InviteSecret,
    sealed: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, CryptoError> {
    open_with_nonce(&*join_key(secret)?, sealed)
}
