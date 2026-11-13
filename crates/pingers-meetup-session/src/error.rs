// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

use pingers_crypto::CryptoError;
use pingers_meetup::MeetupError;
use pingers_mls::MlsError;
use pingers_proto::ProtoError;
use pingers_store::StoreError;
use pingers_transport::TransportError;

use crate::NetworkError;

/// Why a live Meetup operation failed. Never carries coordinates, keys or message contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The local database failed. Fatal for a running Meetup.
    #[error("storage failed")]
    Store(#[from] StoreError),
    /// An MLS operation failed. Fatal for a running Meetup only when it is a storage failure.
    #[error("MLS failed")]
    Mls(#[from] MlsError),
    /// The Meetup state machine refused an input or action.
    #[error("meetup state machine failed")]
    Meetup(#[from] MeetupError),
    /// A frame did not encode or decode.
    #[error("frame encoding failed")]
    Proto(#[from] ProtoError),
    /// A key could not be generated.
    #[error("cryptography failed")]
    Crypto(#[from] CryptoError),
    /// The live channel could not be opened.
    #[error("live channel failed")]
    Transport(#[from] TransportError),
    /// A group log request failed.
    #[error("group log request failed")]
    Network(#[from] NetworkError),
    /// The Party named does not exist, or this device has no identity yet.
    #[error("party or identity not found")]
    NotFound,
    /// Another member's commit won the epoch; the group log has been caught up. Retry if
    /// still needed (for an add: the newcomer may already be in).
    #[error("another commit won the epoch")]
    CommitConflict,
    /// The Meetup is over; the handle can no longer be used.
    #[error("the meetup has ended")]
    Ended,
    /// A background step panicked or was cancelled.
    #[error("internal error")]
    Internal,
}

impl SessionError {
    /// Whether the running Meetup must stop: the database or its MLS state can no longer be
    /// trusted. Anything else (a frame that does not open, a stale commit) only drops the
    /// input that caused it.
    pub(crate) const fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Store(_) | Self::Mls(MlsError::Storage(_)) | Self::Internal
        )
    }
}
