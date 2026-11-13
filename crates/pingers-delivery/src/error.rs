// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's one error type.

use pingers_proto::{Epoch, ProtoError};

/// What went wrong in the delivery service. Carries no identifiers, tokens or message bodies
/// (style guide §9).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DeliveryError {
    /// No group with this id (never created, deleted, or expired). HTTP 404.
    #[error("no such group")]
    UnknownGroup,
    /// `POST /v1/groups` named an id that is already in use. HTTP 409.
    #[error("a group with this id already exists")]
    GroupExists,
    /// A message claims an epoch the log has not reached; no member can be in it yet.
    #[error("message epoch {epoch:?} is ahead of the log's head epoch {head_epoch:?}")]
    FutureEpoch {
        /// The epoch the message claimed.
        epoch: Epoch,
        /// The log's current epoch.
        head_epoch: Epoch,
    },
    /// A subscription asked to live longer than [`crate::MAX_SUBSCRIPTION_LIFETIME`].
    #[error("subscription lifetime is over the maximum")]
    SubscriptionTooLong,
    /// The epoch or sequence counter would overflow `u64`.
    #[error("a log counter would overflow")]
    CounterOverflow,
    /// A time computed from the clock does not fit in `UnixSeconds`.
    #[error("time out of range")]
    TimeOutOfRange,
    /// No live channel with this id is attached to the group.
    #[error("no such live channel")]
    UnknownChannel,
    /// A stored row could not be decoded: the database was written by something else.
    #[error("stored data is corrupt")]
    Corrupt(#[source] ProtoError),
    /// A stored row is out of range for its type.
    #[error("stored data is out of range: {field}")]
    CorruptField {
        /// Which column.
        field: &'static str,
    },
    /// A thread panicked while holding an internal lock.
    #[error("an internal lock was poisoned")]
    Poisoned,
    /// The SQLite store failed.
    #[cfg(feature = "sqlite")]
    #[error("SQLite store")]
    Sqlite(#[from] rusqlite::Error),
}
