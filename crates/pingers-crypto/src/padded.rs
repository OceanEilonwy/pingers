// SPDX-License-Identifier: AGPL-3.0-only
//! Mailbox posts padded inside the AEAD: `nonce (24) || XChaCha20-Poly1305(key,
//! be16(len) || payload || zero padding)`, exactly one size class long. Knocks and join
//! requests share it, so neither leaks its payload size.

use pingers_proto::SizeClass;
use zeroize::Zeroizing;

use crate::CryptoError;
use crate::primitives::{self, TAG_LENGTH, XNONCE_LENGTH};

/// The bytes sealing adds around the padded plaintext: nonce and tag.
const OVERHEAD: usize = XNONCE_LENGTH + TAG_LENGTH;
/// The big-endian length prefix inside the padded plaintext.
const LENGTH_PREFIX: usize = 2;

/// The longest payload that fits `class`.
pub(crate) const fn capacity(class: SizeClass) -> usize {
    class
        .size()
        .saturating_sub(OVERHEAD)
        .saturating_sub(LENGTH_PREFIX)
}

/// Seals `payload` into exactly `class.size()` bytes.
pub(crate) fn seal(
    key: &[u8; 32],
    nonce: &[u8; XNONCE_LENGTH],
    payload: &[u8],
    class: SizeClass,
) -> Result<Vec<u8>, CryptoError> {
    let padded_length = class.size().saturating_sub(OVERHEAD);
    let too_long = CryptoError::WrongLength {
        expected: capacity(class),
        actual: payload.len(),
    };
    if payload.len() > capacity(class) {
        return Err(too_long);
    }
    let length = u16::try_from(payload.len()).map_err(|_overflow| too_long)?;
    let mut padded = Zeroizing::new(Vec::with_capacity(padded_length));
    padded.extend_from_slice(&length.to_be_bytes());
    padded.extend_from_slice(payload);
    padded.resize(padded_length, 0);
    let ciphertext = primitives::xchacha_seal(key, nonce, &[], &padded)?;
    let mut post = Vec::with_capacity(class.size());
    post.extend_from_slice(nonce);
    post.extend_from_slice(&ciphertext);
    Ok(post)
}

/// Opens [`seal`]'s format and strips the padding, which must be zero.
pub(crate) fn open(key: &[u8; 32], post: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let (nonce, ciphertext) = post
        .split_first_chunk::<XNONCE_LENGTH>()
        .ok_or(CryptoError::OpenFailed)?;
    let padded = primitives::xchacha_open(key, nonce, &[], ciphertext)?;
    let (length, rest) =
        padded
            .split_first_chunk::<LENGTH_PREFIX>()
            .ok_or(CryptoError::WrongLength {
                expected: LENGTH_PREFIX,
                actual: padded.len(),
            })?;
    let length = usize::from(u16::from_be_bytes(*length));
    let (payload, padding) = rest
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
    Ok(Zeroizing::new(payload.to_vec()))
}
