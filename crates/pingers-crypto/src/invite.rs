// SPDX-License-Identifier: AGPL-3.0-only
//! Invite join requests (design D, invite flow).

use rand_core::CryptoRng;

use crate::CryptoError;
use crate::secret::{random_secret, secret_key};
use crate::stub::not_implemented;

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
    not_implemented((secret, encoded_request, rng))
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
    not_implemented((secret, sealed))
}
