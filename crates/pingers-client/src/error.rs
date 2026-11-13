// SPDX-License-Identifier: AGPL-3.0-only
//! The client's error. pingers-ffi flattens it for the app.

use pingers_store::StoreError;

use crate::PlatformError;

/// Why a client call failed. Names the crate that failed and keeps its error as the source;
/// never carries names, coordinates, identifiers or keys (style guide §9).
///
/// The four L3 variants carry no source yet: each takes its crate's error type as
/// `#[source]` when the crate lands.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The platform's secure store failed.
    #[error("platform")]
    Platform(#[from] PlatformError),
    /// The secure store holds no database key. The shell makes one on first run.
    #[error("no database key")]
    NoDatabaseKey,
    /// The secure store's database key is not 32 bytes.
    #[error("database key has the wrong length")]
    MalformedDatabaseKey,
    /// The encrypted store failed (wrong key, I/O, corruption).
    #[error("store")]
    Store(#[from] StoreError),
    /// A blocking task panicked.
    #[error("internal")]
    Internal,
    /// pingers-parties failed.
    #[error("parties")]
    Parties,
    /// pingers-sync failed.
    #[error("sync")]
    Sync,
    /// pingers-wake failed.
    #[error("wake")]
    Wake,
    /// pingers-meetup-session failed.
    #[error("meetup session")]
    MeetupSession,
}
