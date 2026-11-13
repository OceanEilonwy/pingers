// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

use crate::SizeClass;

/// Why bytes could not be encoded or decoded.
///
/// Variants never carry the offending bytes: they may be identifiers, tokens or coordinates
/// (style guide §9).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProtoError {
    /// A fixed-size structure was given the wrong number of bytes.
    #[error("expected {expected} bytes, got {actual}")]
    WrongLength {
        /// The size the structure must have.
        expected: usize,
        /// The size it was given.
        actual: usize,
    },
    /// Bytes that the format reserves as zero were not zero.
    #[error("reserved bytes are not zero")]
    ReservedNotZero,
    /// A payload is larger than the size class it was to be padded to.
    #[error("payload of {length} bytes does not fit size class {class:?}")]
    PayloadTooLarge {
        /// The payload length.
        length: usize,
        /// The class it was to be padded to.
        class: SizeClass,
    },
    /// A padded message's length is not one of the allowed size classes.
    #[error("{length} bytes is not a size class")]
    NotASizeClass {
        /// The received length.
        length: usize,
    },
    /// A padded message's length prefix or padding bytes are inconsistent.
    #[error("padding is malformed")]
    BadPadding,
    /// A message carries a version this implementation does not understand.
    #[error("unsupported version {version}")]
    UnsupportedVersion {
        /// The version found on the wire.
        version: u64,
    },
    /// A field holds a value outside its allowed range.
    #[error("field `{field}` is out of range")]
    OutOfRange {
        /// The field's name in this crate.
        field: &'static str,
    },
    /// CBOR decoding failed.
    #[error("malformed CBOR")]
    Cbor(#[source] minicbor::decode::Error),
    /// A bucket index from the wire is out of range.
    #[error("bucket out of range")]
    Bucket(#[source] pingers_time::TimeError),
    /// CBOR input parsed, but is not the one canonical encoding of the value it holds (for
    /// example a long-form integer or an indefinite-length array).
    #[error("CBOR is not in canonical form")]
    NonCanonical,
    /// A complete value was followed by more bytes.
    #[error("trailing bytes after the value")]
    TrailingBytes,
}
