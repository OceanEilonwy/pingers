// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

/// What went wrong in the store. Never carries stored values, keys or identifiers (style
/// guide §9): variants name the table or field, not the content.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The key does not decrypt the database (or the file is not a database at all).
    #[error("the database key is wrong or the file is not a Pingers database")]
    WrongKey,
    /// Another process or thread holds the store's lock and the caller asked not to wait.
    #[error("the store is locked by another process")]
    Busy,
    /// The lock file could not be created or locked.
    #[error("the store's lock file failed")]
    Lock(#[source] std::io::Error),
    /// SQLCipher reported an error.
    #[error("database error")]
    Database(#[source] rusqlite::Error),
    /// The database was written by a newer version of the app.
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    NewerSchema {
        /// The version found in the file.
        found: u32,
        /// The newest version this build knows.
        supported: u32,
    },
    /// A stored value does not decode to its domain type.
    #[error("stored {field} is corrupt")]
    Corrupt {
        /// The table and column, for example `spot.geometry`.
        field: &'static str,
    },
    /// A value does not fit its column (for example a `u64` time past `i64::MAX`).
    #[error("{field} is out of the range the database can store")]
    OutOfRange {
        /// The field.
        field: &'static str,
    },
    /// An update or delete named a row that does not exist.
    #[error("no {table} row matches")]
    NotFound {
        /// The table.
        table: &'static str,
    },
    /// A name or label failed its length check.
    #[error("{field} must be 1 to {max} bytes")]
    InvalidText {
        /// Which kind of text.
        field: &'static str,
        /// The maximum length in bytes.
        max: usize,
    },
    /// A value the store derives itself could not be encoded (for example a group's MLS
    /// storage prefix). Not caused by stored data.
    #[error("internal encoding error")]
    Internal,
    /// A server was named that has no `server` row.
    #[error("unknown server")]
    UnknownServer,
    /// A thread panicked while holding the session's connection.
    #[error("the session's connection was poisoned by a panic")]
    Poisoned,
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(failure, _) = &error
            && failure.code == rusqlite::ErrorCode::NotADatabase
        {
            return Self::WrongKey;
        }
        Self::Database(error)
    }
}
