// SPDX-License-Identifier: AGPL-3.0-only
//! `Platform` and `Events` as UniFFI callback interfaces, and the adapters that hand them to
//! pingers-client as its own traits. Conversion only.

use std::sync::Arc;
use std::time::Duration;

use pingers_client as client;
use zeroize::Zeroizing;

use crate::geo::Coordinate;
use crate::ids::{LocationRequestId, MeetupId, MemberId, PartyId, SpotId};

/// Why a platform call failed.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PlatformError {
    /// The Keystore-wrapped store refused or is locked.
    #[error("secure store unavailable")]
    SecureStoreUnavailable,
    /// The app's implementation threw something else.
    #[error("{reason}")]
    Unexpected {
        /// What it threw.
        reason: String,
    },
}

impl From<uniffi::UnexpectedUniFFICallbackError> for PlatformError {
    fn from(error: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Unexpected {
            reason: error.reason,
        }
    }
}

/// An OS geofence to register: a circle with an id to report back.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct OsRegion {
    /// Opaque id, reported back with each transition.
    pub id: String,
    /// The centre.
    pub center: Coordinate,
    /// The radius in metres.
    pub radius_m: f64,
}

impl From<&client::OsRegion> for OsRegion {
    fn from(region: &client::OsRegion) -> Self {
        Self {
            id: region.id.clone(),
            center: region.region.center().into(),
            radius_m: region.region.radius().get(),
        }
    }
}

/// How hard to try for a fix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum FixQuality {
    /// Balanced power and accuracy.
    Balanced,
    /// GPS quality.
    High,
}

/// The location mode the app should run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum LocationMode {
    /// Region monitoring only.
    Idle,
    /// Inside a Spot alone.
    Dwell,
    /// In a Meetup, app in the background: the location foreground service.
    MeetupBackground,
    /// In a Meetup with the compass open: GPS at 1 Hz.
    MeetupForeground,
}

/// The platform's Focus or DND state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum FocusState {
    /// A Focus or DND mode is on.
    On,
    /// Off.
    Off,
    /// The platform does not say.
    Unknown,
}

/// Which push service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum PushPlatform {
    /// Firebase Cloud Messaging.
    Fcm,
    /// A UnifiedPush distributor.
    UnifiedPush,
}

/// The device's push registration. Sealed by the core, never sent raw.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct PushTokens {
    /// Which service.
    pub platform: PushPlatform,
    /// The token or endpoint.
    pub token: Vec<u8>,
}

impl core::fmt::Debug for PushTokens {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("PushTokens(redacted)")
    }
}

/// What the app provides to the core. Called from core threads; must not block for long.
#[uniffi::export(with_foreign)]
pub trait Platform: Send + Sync {
    /// Reads a value from the Keystore-wrapped store.
    ///
    /// # Errors
    ///
    /// [`PlatformError::SecureStoreUnavailable`].
    fn secure_store_get(&self, key: String) -> Result<Option<Vec<u8>>, PlatformError>;
    /// Writes a value to the store.
    ///
    /// # Errors
    ///
    /// [`PlatformError::SecureStoreUnavailable`].
    fn secure_store_put(&self, key: String, value: Vec<u8>) -> Result<(), PlatformError>;
    /// Replaces all registered OS geofences.
    fn set_os_geofences(&self, regions: Vec<OsRegion>);
    /// Asks for a fix; it arrives through `Core.on_location`.
    fn request_fix(&self, quality: FixQuality, timeout_ms: u32);
    /// Switches the location mode.
    fn set_location_mode(&self, mode: LocationMode);
    /// The current Focus state, read fresh.
    fn focus_state(&self) -> FocusState;
    /// Schedules the next background wake.
    fn schedule_wake(&self, after_seconds: u32);
    /// The push registration, if push is set up.
    fn push_tokens(&self) -> Option<PushTokens>;
}

/// Something to tell the user.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Notice {
    /// A knock at a Spot went unanswered for a lease.
    UnansweredKnock {
        /// The Party.
        party: PartyId,
        /// The Spot.
        spot: SpotId,
    },
    /// Meetup members saw different boards.
    BoardEquivocation,
    /// The clock is off; posting stopped.
    ClockSkew,
    /// The platform does not report DND.
    DndUnavailable,
    /// A relay key does not match its pin.
    RelayKeyMismatch,
    /// The board is too large to read reliably.
    BoardTooLarge,
    /// FCM has been downgrading high-priority messages.
    PushDowngraded,
    /// Privacy Pass is used for everything.
    PrivacyPassOnly,
    /// The push relay and the server declare the same operator.
    SameOperator,
    /// A notice this version of the app does not know.
    Other,
}

impl From<client::Notice> for Notice {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Notice and Fallback are non-exhaustive; a new one is shown as Other"
    )]
    fn from(notice: client::Notice) -> Self {
        match notice {
            client::Notice::UnansweredKnock { party, spot } => Self::UnansweredKnock {
                party: party.into(),
                spot: spot.into(),
            },
            client::Notice::BoardEquivocation => Self::BoardEquivocation,
            client::Notice::ClockSkew => Self::ClockSkew,
            client::Notice::DndUnavailable => Self::DndUnavailable,
            client::Notice::RelayKeyMismatch => Self::RelayKeyMismatch,
            client::Notice::BoardTooLarge => Self::BoardTooLarge,
            client::Notice::PushDowngraded => Self::PushDowngraded,
            client::Notice::FallbackActive(client::Fallback::PrivacyPassOnly) => {
                Self::PrivacyPassOnly
            }
            client::Notice::FallbackActive(client::Fallback::SameOperator) => Self::SameOperator,
            _ => Self::Other,
        }
    }
}

/// What the core tells the app. Events carry ids; details are read from a fresh `Snapshot`.
#[uniffi::export(with_foreign)]
pub trait Events: Send + Sync {
    /// Parties, rosters or Spots changed.
    fn parties_changed(&self);
    /// Someone asked to join a Party.
    fn join_requested(&self, party: PartyId);
    /// A Meetup started.
    fn meetup_started(&self, meetup: MeetupId);
    /// A Meetup's roster or meet point changed.
    fn meetup_updated(&self, meetup: MeetupId);
    /// A chat message arrived.
    fn meetup_message(&self, meetup: MeetupId);
    /// A peer's position updated.
    fn peer_location(&self, meetup: MeetupId, peer: MemberId);
    /// A peer asks to see this device's location.
    fn location_request(&self, meetup: MeetupId, from: MemberId, request: LocationRequestId);
    /// Whether this device can see a peer changed.
    fn location_access_changed(&self, meetup: MeetupId, peer: MemberId);
    /// A Meetup ended and its content was deleted.
    fn meetup_ended(&self, meetup: MeetupId);
    /// Something to tell the user.
    fn notice(&self, notice: Notice);
    /// The "What your server sees" status changed.
    fn privacy_status_changed(&self);
}

/// The app's [`Platform`] as pingers-client's.
pub(crate) struct PlatformAdapter(pub(crate) Arc<dyn Platform>);

fn whole_millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

fn whole_seconds(duration: Duration) -> u32 {
    u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
}

impl client::Platform for PlatformAdapter {
    fn secure_store_get(
        &self,
        key: &client::SecureStoreKey,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, client::PlatformError> {
        self.0
            .secure_store_get(key.as_str().to_owned())
            .map(|value| value.map(Zeroizing::new))
            .map_err(|_| client::PlatformError::SecureStoreUnavailable)
    }

    fn secure_store_put(
        &self,
        key: &client::SecureStoreKey,
        value: &[u8],
    ) -> Result<(), client::PlatformError> {
        self.0
            .secure_store_put(key.as_str().to_owned(), value.to_vec())
            .map_err(|_| client::PlatformError::SecureStoreUnavailable)
    }

    fn set_os_geofences(&self, regions: &[client::OsRegion]) {
        self.0
            .set_os_geofences(regions.iter().map(Into::into).collect());
    }

    fn request_fix(&self, quality: client::FixQuality, timeout: Duration) {
        let quality = match quality {
            client::FixQuality::Balanced => FixQuality::Balanced,
            client::FixQuality::High => FixQuality::High,
        };
        self.0.request_fix(quality, whole_millis(timeout));
    }

    fn set_location_mode(&self, mode: client::LocationMode) {
        self.0.set_location_mode(match mode {
            client::LocationMode::Idle => LocationMode::Idle,
            client::LocationMode::Dwell => LocationMode::Dwell,
            client::LocationMode::MeetupBackground => LocationMode::MeetupBackground,
            client::LocationMode::MeetupForeground => LocationMode::MeetupForeground,
        });
    }

    fn focus_state(&self) -> pingers_gate::FocusState {
        match self.0.focus_state() {
            FocusState::On => pingers_gate::FocusState::On,
            FocusState::Off => pingers_gate::FocusState::Off,
            FocusState::Unknown => pingers_gate::FocusState::Unknown,
        }
    }

    fn schedule_wake(&self, after: Duration) {
        self.0.schedule_wake(whole_seconds(after));
    }

    fn push_tokens(&self) -> Option<client::PushTokens> {
        self.0.push_tokens().map(|tokens| client::PushTokens {
            platform: match tokens.platform {
                PushPlatform::Fcm => pingers_proto::PushPlatform::Fcm,
                PushPlatform::UnifiedPush => pingers_proto::PushPlatform::UnifiedPush,
            },
            token: pingers_proto::PushToken::from_bytes(tokens.token),
        })
    }
}

/// The app's [`Events`] as pingers-client's.
pub(crate) struct EventsAdapter(pub(crate) Arc<dyn Events>);

impl client::Events for EventsAdapter {
    fn parties_changed(&self) {
        self.0.parties_changed();
    }

    fn join_requested(&self, party: pingers_proto::PartyId) {
        self.0.join_requested(party.into());
    }

    fn meetup_started(&self, meetup: pingers_proto::GroupId) {
        self.0.meetup_started(meetup.into());
    }

    fn meetup_updated(&self, meetup: pingers_proto::GroupId) {
        self.0.meetup_updated(meetup.into());
    }

    fn meetup_message(&self, meetup: pingers_proto::GroupId) {
        self.0.meetup_message(meetup.into());
    }

    fn peer_location(&self, meetup: pingers_proto::GroupId, peer: pingers_proto::LeafIndex) {
        self.0.peer_location(meetup.into(), peer.into());
    }

    fn location_request(
        &self,
        meetup: pingers_proto::GroupId,
        from: pingers_proto::LeafIndex,
        request: pingers_proto::LocationRequestId,
    ) {
        self.0
            .location_request(meetup.into(), from.into(), request.into());
    }

    fn location_access_changed(
        &self,
        meetup: pingers_proto::GroupId,
        peer: pingers_proto::LeafIndex,
        _access: pingers_meetup::ExactLocation,
    ) {
        // The app reads the new access from a fresh snapshot, as for every other event.
        self.0.location_access_changed(meetup.into(), peer.into());
    }

    fn meetup_ended(&self, meetup: pingers_proto::GroupId) {
        self.0.meetup_ended(meetup.into());
    }

    fn notice(&self, notice: client::Notice) {
        self.0.notice(notice.into());
    }

    fn privacy_status_changed(&self) {
        self.0.privacy_status_changed();
    }
}
