// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

/// Why encapsulation or decapsulation failed. Never carries message contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OhttpError {
    /// A key config is malformed or offers no supported suite.
    #[error("malformed or unsupported key config")]
    BadKeyConfig,
    /// The key config differs from the pinned hash: raise `RelayKeyMismatch` and stop
    /// anonymous traffic (design B.12).
    #[error("key config does not match the pinned hash")]
    KeyMismatch,
    /// A request names a key id the gateway does not have.
    #[error("unknown key id {key_id}")]
    UnknownKeyId {
        /// The key id from the request.
        key_id: u8,
    },
    /// A message uses a suite other than X25519 / HKDF-SHA256 / AES-128-GCM.
    #[error("unsupported HPKE suite")]
    UnsupportedSuite,
    /// Decryption failed.
    #[error("could not open message")]
    OpenFailed,
    /// A non-final chunk had length zero, which draft-08 §6 requires receivers to reject.
    #[error("zero-length non-final chunk")]
    EmptyNonFinalChunk,
    /// The stream ended without a final chunk (possible truncation), or a chunked preamble
    /// ended before its first chunk did.
    #[error("stream ended without a final chunk")]
    Truncated,
    /// Bytes arrived after the final chunk.
    #[error("data after the final chunk")]
    DataAfterFinalChunk,
    /// A chunk counter would overflow.
    #[error("too many chunks")]
    TooManyChunks,
    /// A chunk is larger than 16 KiB of plaintext (upstream `ohttp`'s limit), so a peer cannot
    /// make a decoder buffer without bound.
    #[error("chunk too large")]
    ChunkTooLarge,
    /// A Binary HTTP message is malformed.
    #[error("malformed binary HTTP message")]
    BadMessage,
    /// A pin is not a base64url SHA-256, or a pin set is empty.
    #[error("malformed key pin")]
    InvalidPin,
    /// A message is longer than the padded length asked for.
    #[error("message longer than its padded length")]
    LongerThanPadding,
    /// The HPKE layer reported an error the types should have ruled out.
    #[error("internal OHTTP error")]
    Internal,
}
