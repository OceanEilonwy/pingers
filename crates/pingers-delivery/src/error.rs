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
    /// A reader behind the head asked for what its epoch's key may not read: the snapshot
    /// of a later epoch. HTTP 403.
    #[error("not readable with an earlier epoch's key")]
    LaterEpoch,
    /// The group would hold more than [`crate::MAX_SUBSCRIPTIONS_PER_GROUP`] subscriptions.
    #[error("too many subscriptions in the group")]
    TooManySubscriptions,
    /// The group already has [`crate::MAX_LIVE_CHANNELS_PER_GROUP`] live channels open.
    #[error("too many live channels in the group")]
    TooManyChannels,
    /// The channel has stored [`crate::STORED_LIVE_FRAMES_PER_BUCKET`] frames this bucket.
    #[error("live channel over its stored-frame allowance")]
    LiveRateLimited,
    /// A live channel's request was signed with the key of an epoch that a commit has ended
    /// since. HTTP 403: the member attaches again with the next epoch's key.
    #[error("the epoch that authorised the channel has ended")]
    EpochEnded,
    /// A subscription would replace an unexpired one bound to another owner key. HTTP 403.
    #[error("the subscription belongs to another owner key")]
    SubscriptionOwned,
    /// `DELETE /v1/groups/{id}` named a Party group, which is never deleted on request. HTTP
    /// 403.
    #[error("a Party group is not deleted on request")]
    PartyGroupKept,
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
