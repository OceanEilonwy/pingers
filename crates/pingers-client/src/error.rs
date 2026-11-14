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
    /// The server needs a registration code from its operator before it takes this device
    /// (design B.1): ask the user for one ("This server needs a code") and pass it to
    /// `submit_registration_code`.
    #[error("the server needs a registration code")]
    RegistrationCodeRequired,
    /// The server refused the registration code offered: nothing was set up, and another code
    /// can be tried ("Registration code refused").
    #[error("the server refused the registration code")]
    RegistrationCodeRefused,
    /// Adding a server failed; see [`ServerProblem`].
    #[error("server")]
    Server(#[from] ServerProblem),
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

/// Why a server could not be added (New party with another server, or an invite to a server
/// this device has not used).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum ServerProblem {
    /// The address is not a server origin (`https://host[:port]`).
    #[error("not a server address")]
    InvalidAddress,
    /// The server did not answer, or answered with something that is not Pingers.
    #[error("server unreachable")]
    Unreachable,
    /// The server advertises no OHTTP relay: the app never talks to a server without one
    /// (design B.12).
    #[error("server advertises no privacy relay")]
    NoRelay,
    /// None of the server's relays answered.
    #[error("privacy relay unreachable")]
    RelayUnreachable,
    /// The server's relay list said one thing directly and another through the relay.
    #[error("server gave different relays")]
    RelaysDiffer,
    /// The keys the server serves are not the ones being trusted: an invite's pins do not
    /// match, or the keys changed since they were shown. Shown as the relay key warning.
    #[error("server keys do not match")]
    KeyMismatch,
    /// `trust_server` was called for a server that was not looked at first.
    #[error("server not looked at")]
    NotProbed,
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
    /// Joining a Party needs wake-up pushes: the join request carries a sealed subscription
    /// (pingers-proto `JoinRequest.party_subscription` is not optional).
    #[error("joining needs wake-up pushes on")]
    WakePushesOff,
    /// The invite's Welcome has not arrived yet: the inviter has not admitted this device.
    #[error("waiting for the inviter")]
    AwaitingWelcome,
    /// This device knows no such server (or knows it without key pins): look at it and trust
    /// it first, or open an invite to it.
    #[error("unknown server")]
    UnknownServer,
}
