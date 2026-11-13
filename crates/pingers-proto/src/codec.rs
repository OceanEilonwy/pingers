// SPDX-License-Identifier: AGPL-3.0-only
//! The encode and decode contract every variable-size wire type implements.

use crate::ProtoError;

/// A type with one canonical byte encoding.
///
/// Encoding the same value twice gives the same bytes, so encodings can be hashed and signed.
pub trait WireEncode {
    /// The canonical encoding.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if a field cannot be represented on the wire (for example a
    /// duration longer than the format allows).
    fn encode(&self) -> Result<Vec<u8>, ProtoError>;
}

/// A type that can be read back from its canonical encoding.
///
/// `decode(encode(value)) == value` for every value; decoders reject anything that is not a
/// canonical encoding, including trailing bytes and unknown versions.
pub trait WireDecode: Sized {
    /// Parses `bytes`.
    ///
    /// # Errors
    ///
    /// Any [`ProtoError`] variant describing why `bytes` is not a canonical encoding.
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError>;
}
