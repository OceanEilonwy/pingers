// SPDX-License-Identifier: AGPL-3.0-only
//! Thin wrappers over the `RustCrypto` primitives, so every module calls them the same way and
//! maps their errors the same way. Nothing outside this crate sees these.

use aes_gcm::Aes256Gcm;
use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::{AeadInOut as _, KeyInit as _};
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use hmac::{Hmac, Mac as _};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::CryptoError;

/// Length of an XChaCha20-Poly1305 nonce.
pub(crate) const XNONCE_LENGTH: usize = 24;
/// Length of an AES-256-GCM nonce.
pub(crate) const GCM_NONCE_LENGTH: usize = 12;
/// Length of every AEAD tag used here.
pub(crate) const TAG_LENGTH: usize = 16;

/// `HKDF-Expand(prk, info_0 || info_1 || ..., 32)` with HKDF-SHA256 (RFC 5869). The info is
/// given in parts so callers need not concatenate secrets into temporary buffers.
pub(crate) fn hkdf_expand_32(
    prk: &[u8; 32],
    info: &[&[u8]],
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let hkdf = Hkdf::<Sha256>::from_prk(prk).map_err(|_length| CryptoError::Internal)?;
    let mut output = Zeroizing::new([0; 32]);
    hkdf.expand_multi_info(info, output.as_mut_slice())
        .map_err(|_length| CryptoError::Internal)?;
    Ok(output)
}

/// `HMAC-SHA256(key, message_0 || message_1 || ...)`.
pub(crate) fn hmac_sha256(key: &[u8; 32], message: &[&[u8]]) -> Result<[u8; 32], CryptoError> {
    let mut mac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(key)
        .map_err(|_length| CryptoError::Internal)?;
    for part in message {
        mac.update(part);
    }
    Ok(mac.finalize().into_bytes().into())
}

/// `SHA-256(part_0 || part_1 || ...)`.
pub(crate) fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// XChaCha20-Poly1305 encryption; returns ciphertext with the tag appended.
pub(crate) fn xchacha_seal(
    key: &[u8; 32],
    nonce: &[u8; XNONCE_LENGTH],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut buffer = plaintext.to_vec();
    cipher
        .encrypt_in_place(nonce.into(), aad, &mut buffer)
        .map_err(|_error| CryptoError::Internal)?;
    Ok(buffer)
}

/// XChaCha20-Poly1305 decryption into a buffer that is zeroed when dropped.
pub(crate) fn xchacha_open(
    key: &[u8; 32],
    nonce: &[u8; XNONCE_LENGTH],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut buffer = Zeroizing::new(ciphertext.to_vec());
    cipher
        .decrypt_in_place(nonce.into(), aad, &mut *buffer)
        .map_err(|_error| CryptoError::OpenFailed)?;
    Ok(buffer)
}

/// AES-256-GCM encryption; returns ciphertext with the tag appended.
pub(crate) fn aes_gcm_seal(
    key: &[u8; 32],
    nonce: &[u8; GCM_NONCE_LENGTH],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new(key.into());
    let mut buffer = plaintext.to_vec();
    cipher
        .encrypt_in_place(nonce.into(), aad, &mut buffer)
        .map_err(|_error| CryptoError::Internal)?;
    Ok(buffer)
}

/// AES-256-GCM decryption into a buffer that is zeroed when dropped.
pub(crate) fn aes_gcm_open(
    key: &[u8; 32],
    nonce: &[u8; GCM_NONCE_LENGTH],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let cipher = Aes256Gcm::new(key.into());
    let mut buffer = Zeroizing::new(ciphertext.to_vec());
    cipher
        .decrypt_in_place(nonce.into(), aad, &mut *buffer)
        .map_err(|_error| CryptoError::OpenFailed)?;
    Ok(buffer)
}

/// An Ed25519 signature (RFC 8032, pure) over `message`.
pub(crate) fn ed25519_sign(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    SigningKey::from_bytes(seed).sign(message).to_bytes()
}

/// Verifies an Ed25519 signature, rejecting small-order keys and non-canonical encodings
/// (`verify_strict`), so one message has one valid signature per key.
pub(crate) fn ed25519_verify(
    public_key: &[u8; 32],
    message: &[u8],
    signature: &[u8; 64],
) -> Result<(), CryptoError> {
    let key = VerifyingKey::from_bytes(public_key).map_err(|_error| CryptoError::BadSignature)?;
    key.verify_strict(message, &ed25519_dalek::Signature::from_bytes(signature))
        .map_err(|_error| CryptoError::BadSignature)
}

/// Copies the first `N` bytes of `bytes`, or fails if there are fewer.
pub(crate) fn prefix<const N: usize>(bytes: &[u8]) -> Result<[u8; N], CryptoError> {
    bytes
        .first_chunk::<N>()
        .copied()
        .ok_or(CryptoError::Internal)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    // RFC 4231 test case 2 is keyed with "Jefe" (not 32 bytes); test case with a 32-byte key
    // is not in the RFC, so the HMAC wrapper is pinned through the presence vectors instead.

    #[test]
    fn hkdf_expand_matches_rfc5869_case_1_prefix() -> Result<(), CryptoError> {
        // RFC 5869 A.1: PRK and info, first 32 bytes of OKM.
        let prk = [
            0x07, 0x77, 0x09, 0x36, 0x2c, 0x2e, 0x32, 0xdf, 0x0d, 0xdc, 0x3f, 0x0d, 0xc4, 0x7b,
            0xba, 0x63, 0x90, 0xb6, 0xc7, 0x3b, 0xb5, 0x0f, 0x9c, 0x31, 0x22, 0xec, 0x84, 0x4a,
            0xd7, 0xc2, 0xb3, 0xe5,
        ];
        let info = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
        let (first, second) = info.split_at(4);
        let okm = hkdf_expand_32(&prk, &[first, second])?;
        assert_eq!(
            okm.as_slice(),
            [
                0x3c, 0xb2, 0x5f, 0x25, 0xfa, 0xac, 0xd5, 0x7a, 0x90, 0x43, 0x4f, 0x64, 0xd0, 0x36,
                0x2f, 0x2a, 0x2d, 0x2d, 0x0a, 0x90, 0xcf, 0x1a, 0x5a, 0x4c, 0x5d, 0xb0, 0x2d, 0x56,
                0xec, 0xc4, 0xc5, 0xbf
            ]
        );
        Ok(())
    }

    #[test]
    fn tampered_xchacha_ciphertext_does_not_open() -> Result<(), CryptoError> {
        let key = [7; 32];
        let nonce = [9; XNONCE_LENGTH];
        let mut sealed = xchacha_seal(&key, &nonce, b"aad", b"message")?;
        if let Some(byte) = sealed.first_mut() {
            *byte ^= 1;
        }
        assert!(matches!(
            xchacha_open(&key, &nonce, b"aad", &sealed),
            Err(CryptoError::OpenFailed)
        ));
        Ok(())
    }

    #[test]
    fn signature_from_another_key_is_rejected() {
        let signature = ed25519_sign(&[1; 32], b"message");
        let other = SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes();
        assert!(matches!(
            ed25519_verify(&other, b"message", &signature),
            Err(CryptoError::BadSignature)
        ));
    }
}
