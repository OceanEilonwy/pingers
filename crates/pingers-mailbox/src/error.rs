// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's errors.

/// Why a mailbox request was refused. Never carries mailbox ids, keys, blobs or bodies.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MailboxError {
    /// The registration asks for a lifetime over 30 days.
    #[error("mailbox lifetime too long")]
    LifetimeTooLong(#[source] pingers_presence::PresenceError),
    /// The sealed push names a relay this server does not forward to.
    #[error("unknown push relay")]
    UnknownRelay,
    /// A live mailbox already has this id.
    #[error("mailbox already exists")]
    AlreadyExists,
    /// No live mailbox has this id.
    #[error("no such mailbox")]
    NotFound,
    /// The read key does not derive the mailbox id.
    #[error("wrong read key")]
    WrongReadKey,
    /// A post is not padded to 1, 4, 8 or 32 KiB.
    #[error("post is not a mailbox size class")]
    NotAMailboxClass(#[source] pingers_presence::PresenceError),
    /// The mailbox holds as many messages as the rules allow.
    #[error("mailbox is full")]
    Full,
    /// The post limit refused the post.
    #[error("too many posts to this mailbox")]
    RateLimited,
    /// The storage failed.
    #[error("mailbox storage failed")]
    Storage(#[from] StorageError),
    /// A push relay URL is neither `https://` nor `http://` to a loopback address.
    #[error("push relay URL must be https://, or http:// to a loopback address")]
    RelayUrl,
    /// A time did not fit its type; the types should rule this out (style guide §5).
    #[error("internal mailbox error")]
    Internal,
}

/// Why a storage operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// A mailbox with this id is already stored.
    #[error("duplicate mailbox")]
    Duplicate,
    /// No mailbox with this id is stored.
    #[error("mailbox not stored")]
    NotFound,
    /// The mailbox has used every sequence number.
    #[error("sequence numbers exhausted")]
    SequenceExhausted,
    /// A stored value could not be read back as its type.
    #[error("stored value is corrupt")]
    Corrupt,
    /// The database reported an error.
    #[cfg(feature = "sqlite")]
    #[error("database error")]
    Sqlite(#[from] rusqlite::Error),
}
