// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

/// Why a board operation failed. Never carries records, lease ids, keys or cursor bytes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BoardError {
    /// The lease's start bucket is neither the current bucket nor the previous one.
    #[error("lease start bucket is not current")]
    LeaseNotCurrent(#[source] pingers_presence::PresenceError),
    /// The revoke key does not hash to the lease's commitment.
    #[error("revoke key does not match the lease")]
    RevokeKeyMismatch,
    /// No live lease has this id (never stored, already revoked, or expired).
    #[error("no such lease")]
    UnknownLease,
    /// The cursor was not issued by this board: tampered, truncated, or from before a restart.
    #[error("invalid board cursor")]
    BadCursor,
    /// The cursor is older than [`crate::CURSOR_LIFETIME`].
    #[error("board cursor expired")]
    CursorExpired,
    /// A page size of zero or above [`pingers_proto::BoardPage::MAX_RECORDS`].
    #[error("page size out of range")]
    PageSize,
    /// Sealing a cursor failed.
    #[error("cryptographic step failed")]
    Crypto(#[from] pingers_crypto::CryptoError),
    /// A count did not fit its type; the types should rule this out (style guide §5).
    #[error("internal board error")]
    Internal,
}
