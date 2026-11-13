// SPDX-License-Identifier: AGPL-3.0-only
//! Keys the Party and Meetup groups derive from the MLS exporter (design B.2).

use ed25519_dalek::SigningKey;
use pingers_proto::{GroupAuthKey, SealedSnapshot};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;

use crate::secret::secret_key;
use crate::stub::not_implemented;
use crate::{CryptoError, Signature};

secret_key!(
    /// `MLS-Exporter("pingers/v1/state", epoch, 32)`: seals the state snapshot uploaded with
    /// each commit. `from_bytes` is public because pingers-mls produces it.
    StateKey,
    32,
    pub
);

secret_key!(
    /// The seed `MLS-Exporter("pingers/v1/log-auth", group_id, 32)` of the Ed25519 key pair
    /// that authorises group log access for one epoch. `from_bytes` is public because
    /// pingers-mls produces it.
    LogAuthKeyPair,
    32,
    pub
);

impl LogAuthKeyPair {
    /// The public half, registered with the server for the epoch.
    #[must_use]
    pub fn public_key(&self) -> GroupAuthKey {
        GroupAuthKey::from_bytes(
            SigningKey::from_bytes(self.expose_bytes())
                .verifying_key()
                .to_bytes(),
        )
    }
}

/// What a group log request signature covers (design D, auth mode Grp):
/// `"pingers/v1/grp-request" || method || path || SHA-256(body) || be64(timestamp)`, with
/// method and path length-prefixed.
#[derive(Clone, Copy, Debug)]
pub struct GroupRequest<'request> {
    /// The HTTP method, upper case.
    pub method: &'request str,
    /// The request path including the query.
    pub path: &'request str,
    /// The request body (hashed, not signed directly).
    pub body: &'request [u8],
    /// When the request was made.
    pub timestamp: UnixSeconds,
}

/// Signs a group log request with the epoch's log-auth key.
///
/// # Errors
///
/// [`CryptoError::Internal`] if signing fails.
pub fn sign_group_request(
    key_pair: &LogAuthKeyPair,
    request: &GroupRequest<'_>,
) -> Result<Signature, CryptoError> {
    not_implemented((key_pair, request))
}

/// Seals a state snapshot: `nonce (24, random) || XChaCha20-Poly1305(state key, snapshot)`.
///
/// # Errors
///
/// [`CryptoError::Internal`] if the AEAD fails.
pub fn seal_state_snapshot(
    key: &StateKey,
    snapshot: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<SealedSnapshot, CryptoError> {
    not_implemented((key, snapshot, rng))
}

/// Opens a state snapshot.
///
/// # Errors
///
/// [`CryptoError::OpenFailed`] if it was not sealed under this key.
pub fn open_state_snapshot(
    key: &StateKey,
    sealed: &SealedSnapshot,
) -> Result<zeroize::Zeroizing<Vec<u8>>, CryptoError> {
    not_implemented((key, sealed))
}
