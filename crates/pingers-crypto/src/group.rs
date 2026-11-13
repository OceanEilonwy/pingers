// SPDX-License-Identifier: AGPL-3.0-only
//! Keys the Party and Meetup groups derive from the MLS exporter (design B.2).

use ed25519_dalek::SigningKey;
use pingers_proto::{GroupAuthKey, SealedSnapshot};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;

use crate::primitives::{self, XNONCE_LENGTH};
use crate::secret::secret_key;
use crate::{CryptoError, Signature, labels};

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
    let message = group_request_message(request)?;
    Ok(Signature::from_bytes(primitives::ed25519_sign(
        key_pair.expose_bytes(),
        &message,
    )))
}

/// Verifies a group log request signature against the epoch's registered key (server side).
///
/// # Errors
///
/// [`CryptoError::BadSignature`] if it does not verify;
/// [`CryptoError::WrongLength`] if the method or path is longer than 2³² − 1 bytes.
pub fn verify_group_request(
    public_key: &GroupAuthKey,
    request: &GroupRequest<'_>,
    signature: &Signature,
) -> Result<(), CryptoError> {
    let message = group_request_message(request)?;
    primitives::ed25519_verify(public_key.as_bytes(), &message, signature.as_bytes())
}

/// `"pingers/v1/grp-request" || be32(len(method)) || method || be32(len(path)) || path ||
/// SHA-256(body) || be64(timestamp)`.
pub(crate) fn group_request_message(request: &GroupRequest<'_>) -> Result<Vec<u8>, CryptoError> {
    Ok([
        labels::GROUP_REQUEST,
        &length_prefix(request.method.as_bytes())?,
        request.method.as_bytes(),
        &length_prefix(request.path.as_bytes())?,
        request.path.as_bytes(),
        &primitives::sha256(&[request.body]),
        &request.timestamp.get().to_be_bytes(),
    ]
    .concat())
}

fn length_prefix(field: &[u8]) -> Result<[u8; 4], CryptoError> {
    u32::try_from(field.len())
        .map(u32::to_be_bytes)
        .map_err(|_overflow| CryptoError::WrongLength {
            expected: usize::try_from(u32::MAX).unwrap_or(usize::MAX),
            actual: field.len(),
        })
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
    let mut nonce = [0; XNONCE_LENGTH];
    rng.fill_bytes(&mut nonce);
    seal_with_nonce(key.expose_bytes(), &nonce, snapshot).map(SealedSnapshot::from_bytes)
}

/// `nonce || XChaCha20-Poly1305(key, nonce, aad = "", plaintext)`, shared by snapshots and
/// join requests.
pub(crate) fn seal_with_nonce(
    key: &[u8; 32],
    nonce: &[u8; XNONCE_LENGTH],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let ciphertext = primitives::xchacha_seal(key, nonce, &[], plaintext)?;
    Ok([nonce.as_slice(), &ciphertext].concat())
}

/// Opens [`seal_with_nonce`]'s format.
pub(crate) fn open_with_nonce(
    key: &[u8; 32],
    sealed: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, CryptoError> {
    let (nonce, ciphertext) = sealed
        .split_first_chunk::<XNONCE_LENGTH>()
        .ok_or(CryptoError::OpenFailed)?;
    primitives::xchacha_open(key, nonce, &[], ciphertext)
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
    open_with_nonce(key.expose_bytes(), sealed.as_bytes())
}
