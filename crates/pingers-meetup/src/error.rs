// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

/// Why a Meetup input was rejected. Never carries positions, chat text or keys.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MeetupError {
    /// The input names a leaf that is not a current member.
    #[error("not a member of this meetup")]
    NotAMember,
    /// The answer names a location request that does not exist or has expired.
    #[error("unknown or expired location request")]
    UnknownRequest,
    /// The session has ended; it accepts no more inputs.
    #[error("meetup has ended")]
    Ended,
    /// A cryptographic step failed (other than an expected failed trial-open).
    #[error("cryptographic step failed")]
    Crypto(#[from] pingers_crypto::CryptoError),
    /// Bytes could not be encoded or decoded.
    #[error("encoding failed")]
    Proto(#[from] pingers_proto::ProtoError),
    /// A coordinate could not be converted.
    #[error("invalid coordinate")]
    Geo(#[from] pingers_geo::GeoError),
    /// This part of the contract has no implementation yet.
    #[error("not implemented")]
    NotImplemented,
}
