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
    /// pingers-presence does not hand `PSK_meet` to pingers-meetup-session yet (pingers-wake
    /// CONTRACT.md, "Needs"), so this device cannot create or join a Meetup.
    #[error("waits on pingers-presence handing out PSK_meet")]
    MeetupPsk,
    /// No Meetup with this id is live on this device.
    #[error("no such live meetup")]
    NoMeetup,
}
