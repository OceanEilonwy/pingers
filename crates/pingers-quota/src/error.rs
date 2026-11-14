// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's one error type.

use pingers_anon::AnonError;

/// Why a request may not post, or why the quota service failed. Carries no nullifiers,
/// tokens, accounts or leaves (style guide §9).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QuotaError {
    /// pingers-anon refused: [`AnonError::StaleRoot`] (proof against a root no longer
    /// accepted), [`AnonError::WrongRequest`], [`AnonError::BadProof`],
    /// [`AnonError::BadToken`], [`AnonError::BadIssuance`], [`AnonError::TreeFull`] or a
    /// non-canonical identity commitment.
    #[error("anonymous credential rejected")]
    Anon(#[from] AnonError),
    /// The proof's bucket is neither the current one nor, within 60 s of the rollover, the
    /// previous one.
    #[error("proof is for a bucket that is not accepted now")]
    StaleBucket,
    /// The nullifier was already spent in this bucket with a different request: the same
    /// message slot used twice. Rejected with 403; slashing is off, so the share is ignored.
    #[error("nullifier already used in this bucket for another request")]
    NullifierReused,
    /// The token was already redeemed.
    #[error("token already spent")]
    DoubleSpend,
    /// The batch would take the account past its daily allowance of tokens; nothing was
    /// issued. Carries what is left today and when the next day starts, for the 429 answer.
    #[error("daily token allowance exceeded")]
    DailyLimit(pingers_proto::PrivacyPassAllowance),
    /// A time computed from the clock does not fit its type.
    #[error("time out of range")]
    TimeOutOfRange,
    /// The tree rebuilt from stored leaves does not match the stored root.
    #[error("stored membership tree does not match its stored root")]
    RootMismatch,
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
