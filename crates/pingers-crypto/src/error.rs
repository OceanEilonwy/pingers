// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

/// Why a cryptographic operation failed. Never carries key material, plaintext or identifiers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CryptoError {
    /// Decryption or opening failed: wrong key, wrong associated data, or tampered bytes.
    /// Trial-opening with the wrong key (grants, beacons) reports this too; it is expected.
    #[error("could not open: wrong key or tampered ciphertext")]
    OpenFailed,
    /// A signature did not verify.
    #[error("signature does not verify")]
    BadSignature,
    /// Key bytes are not a valid key for the algorithm.
    #[error("invalid key")]
    InvalidKey,
    /// A plaintext or ciphertext has the wrong length for its fixed-size format.
    #[error("wrong length: expected {expected} bytes, got {actual}")]
    WrongLength {
        /// The required length.
        expected: usize,
        /// The length given.
        actual: usize,
    },
    /// A plaintext could not be encoded or a decrypted plaintext could not be decoded.
    #[error("encoding failed")]
    Encoding(#[source] pingers_proto::ProtoError),
    /// A primitive reported an error that the types should have ruled out (style guide §5:
    /// return an error instead of panicking).
    #[error("internal cryptographic error")]
    Internal,
}
