// SPDX-License-Identifier: AGPL-3.0-only
//! Composition root: wires the above, no domain logic.
//!
//! The only crate pingers-ffi and pingers-screens see (architecture §3). It builds the client
//! crates from a [`Platform`] (secure store, OS geofences, fixes, DND, wake scheduling, push
//! tokens) and an [`Events`] sink, exposes one async API, and publishes a read-only state
//! snapshot for pingers-screens.
//!
//! **Rule: no domain logic.** Each method delegates to one crate. If a change here needs a
//! domain `if`, it belongs in a named crate instead. Size budget: under 500 lines.
//!
//! - [`Platform`] and [`Events`]: the two traits the shell implements (design E.1, adapted to
//!   the split crates, Android only), and the [`Notice`] vocabulary.
//! - [`Client`]: the async API. [`Client::open`] reads the database key from the platform's
//!   secure store, opens the store and builds the real [`Crates`] over it
//!   ([`Crates::real`]); [`Client::open_with`] takes another builder (tests use testkit's
//!   `FakeCrates`).
//! - [`ports`]: what the client calls on each L3 crate; the adapters over the real crates
//!   are in the private `real` module.
//! - [`snapshot`]: the [`ClientSnapshot`](snapshot::ClientSnapshot) every call republishes.
//!
//! Commands report changes through the snapshot ([`Client::subscribe`]); [`Events`] carries
//! what the pipeline found on its own.
#![forbid(unsafe_code)]

mod client;
mod error;
pub mod ports;
mod real;
pub mod snapshot;

pub use client::{Client, ClientConfig};
pub use error::{ClientError, Unavailable};
pub use pingers_ohttp::KeyPins;
pub use pingers_wake::PushPriority;
pub use ports::Crates;
pub use real::{Instance, Outside, Trust};

use std::time::Duration;

use pingers_gate::FocusState;
use pingers_geo::Circle;
use pingers_meetup::ExactLocation;
use pingers_proto::{
    GroupId, LeafIndex, LocationRequestId, PartyId, PushPlatform, PushToken, SpotId,
};
use zeroize::Zeroizing;

/// Why a platform call failed. The shell maps its own errors onto these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// The secure store (Keystore-wrapped storage) refused or is locked.
    #[error("secure store unavailable")]
    SecureStoreUnavailable,
}

/// A name for a value in the platform's secure store (for example the SQLCipher key).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecureStoreKey(String);

impl SecureStoreKey {
    /// The SQLCipher database key.
    #[must_use]
    pub fn database() -> Self {
        Self(String::from("pingers.database-key"))
    }

    /// The key's name, for the shell.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An OS geofence to register (design E.2): a circle with an id the shell reports back.
#[derive(Clone, Debug, PartialEq)]
pub struct OsRegion {
    /// Opaque id, reported back with each transition.
    pub id: String,
    /// The circle.
    pub region: Circle,
}

/// How hard the platform should try for a fix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FixQuality {
    /// Balanced power and accuracy (the pipeline default, design E.2).
    Balanced,
    /// GPS-quality, for liveness checks and the compass.
    High,
}

/// The location mode the shell should run (design E.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationMode {
    /// Region monitoring only.
    Idle,
    /// Inside a Spot alone: no continuous GPS.
    Dwell,
    /// In a Meetup, app in the background: the location foreground service.
    MeetupBackground,
    /// In a Meetup with the compass open: GPS at 1 Hz and heading.
    MeetupForeground,
}

/// The device's push registration. The core seals it per mailbox and subscription and never
/// sends it raw (design B.4.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushTokens {
    /// FCM or a UnifiedPush distributor.
    pub platform: PushPlatform,
    /// The token or endpoint.
    pub token: PushToken,
}

/// What the Android shell provides to the core. Called from core threads; implementations must
/// not block for long.
pub trait Platform: Send + Sync {
    /// Reads a value from the Keystore-wrapped secure store.
    ///
    /// # Errors
    ///
    /// [`PlatformError::SecureStoreUnavailable`].
    fn secure_store_get(
        &self,
        key: &SecureStoreKey,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, PlatformError>;

    /// Writes a value to the secure store.
    ///
    /// # Errors
    ///
    /// [`PlatformError::SecureStoreUnavailable`].
    fn secure_store_put(&self, key: &SecureStoreKey, value: &[u8]) -> Result<(), PlatformError>;

    /// Replaces all registered OS geofences.
    fn set_os_geofences(&self, regions: &[OsRegion]);

    /// Asks for a fix; the result arrives through the core's `on_location` entry point.
    fn request_fix(&self, quality: FixQuality, timeout: Duration);

    /// Switches the location mode.
    fn set_location_mode(&self, mode: LocationMode);

    /// The current interruption filter, read fresh (design B.10).
    fn focus_state(&self) -> FocusState;

    /// Schedules the next background wake (WorkManager).
    fn schedule_wake(&self, after: Duration);

    /// The current push registration, if push is set up.
    fn push_tokens(&self) -> Option<PushTokens>;
}

/// Something the user should be told about (design E.1 `Notice`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Notice {
    /// A knock at this Spot went unanswered for a lease (design B.4.4).
    UnansweredKnock {
        /// The Party.
        party: PartyId,
        /// The Spot.
        spot: SpotId,
    },
    /// Meetup members saw different boards (design B.4.3).
    BoardEquivocation,
    /// The device clock is off by more than half a bucket; posting stopped (design F.1).
    ClockSkew,
    /// The platform does not report DND; offer the manual pause (design B.10).
    DndUnavailable,
    /// An OHTTP or push relay key does not match its pin; anonymous traffic stopped (B.12).
    RelayKeyMismatch,
    /// The board is too large to read reliably (design B.4.3).
    BoardTooLarge,
    /// FCM has been downgrading high-priority messages (design B.4.9).
    PushDowngraded,
    /// A runtime fallback is active (design §0).
    FallbackActive(Fallback),
}

/// A runtime fallback, shown in "What your server sees".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Fallback {
    /// Privacy Pass is used for everything: the leaf is not live yet, the prover is too slow,
    /// or proofs are disabled (design B.4.8).
    PrivacyPassOnly,
    /// The push relay and the server declare the same operator (design B.4.9).
    SameOperator,
}

/// What the core tells the shell. Events carry ids; the shell reads details from the state
/// snapshot, so no position or message text crosses this interface.
pub trait Events: Send + Sync {
    /// Parties, rosters or Spots changed.
    fn parties_changed(&self);
    /// Someone asked to join a Party through a multi-use invite.
    fn join_requested(&self, party: PartyId);
    /// A Meetup started.
    fn meetup_started(&self, meetup: GroupId);
    /// A Meetup's roster or meet point changed.
    fn meetup_updated(&self, meetup: GroupId);
    /// A chat message arrived.
    fn meetup_message(&self, meetup: GroupId);
    /// A peer's position updated (only for peers whose location key is held).
    fn peer_location(&self, meetup: GroupId, peer: LeafIndex);
    /// A peer asks to see this device's location.
    fn location_request(&self, meetup: GroupId, from: LeafIndex, request: LocationRequestId);
    /// Whether this device can see a peer changed.
    fn location_access_changed(&self, meetup: GroupId, peer: LeafIndex, access: ExactLocation);
    /// A Meetup ended and its content was deleted.
    fn meetup_ended(&self, meetup: GroupId);
    /// Something to tell the user.
    fn notice(&self, notice: Notice);
    /// The "What your server sees" status changed.
    fn privacy_status_changed(&self);
}
