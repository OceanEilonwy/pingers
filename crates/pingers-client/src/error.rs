// SPDX-License-Identifier: AGPL-3.0-only
//! The client's error. pingers-ffi flattens it for the app.

use pingers_meetup_session::SessionError;
use pingers_mls::StorageError;
use pingers_parties::PartiesError;
use pingers_store::StoreError;
use pingers_wake::WakeError;

use crate::PlatformError;

/// Why a client call failed. Names the crate that failed and keeps its error as the source;
/// never carries names, coordinates, identifiers or keys (style guide §9).
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
    /// The client's own records in the store's key-value table failed.
    #[error("client records")]
    Records(#[from] StorageError),
    /// The instance's relay or server settings do not make a transport.
    #[error("transport")]
    Transport(#[from] pingers_transport::TransportError),
    /// A blocking task panicked.
    #[error("internal")]
    Internal,
    /// pingers-parties failed.
    #[error("parties")]
    Parties(#[from] PartiesError),
    /// pingers-sync failed.
    #[error("sync")]
    Sync(#[from] pingers_sync::SyncError),
    /// pingers-wake failed.
    #[error("wake")]
    Wake(#[from] WakeError),
    /// pingers-meetup-session failed.
    #[error("meetup session")]
    MeetupSession(#[from] SessionError),
    /// The call needs a crate or a value that is not wired yet.
    #[error("unavailable")]
    Unavailable(#[from] Unavailable),
}

/// What an unwired call waits for. Each goes when its crate or value lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum Unavailable {
    /// pingers-sync has not merged: push registration, group logs, mailboxes, the board,
    /// subscriptions, Welcomes and every `Outbound` value wait on it.
    #[error("waits on pingers-sync")]
    Sync,
    /// No Meetup with this id is live on this device.
    #[error("no such live meetup")]
    NoMeetup,
    /// Wake-up pushes need the platform's push token, and there is none yet.
    #[error("no push token from the platform")]
    NoPushToken,
    /// Wake-up pushes need a push relay: the build names none, or the server does not list it.
    #[error("no push relay set up")]
    NoPushRelay,
    /// "No wake-up pushes" is set, so nothing can be sealed to wake this device. Joining does
    /// not need them: a join request then carries no subscription.
    #[error("wake-up pushes are off")]
    WakePushesOff,
}
