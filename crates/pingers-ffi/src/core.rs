// SPDX-License-Identifier: AGPL-3.0-only
//! [`Core`]: pingers-client for the app.
//!
//! The surface follows design E.1, adapted to the split crates: the constructor takes the app's
//! [`Platform`] and [`Events`], actions are `suspend` functions on the tokio runtime, and
//! reads go through [`Core::snapshot`] and the screens on [`Snapshot`].
//!
//! WAITING(w3-client): pingers-client's async API is being built on `w3-client`. Until it
//! merges, `open` succeeds and holds the adapters, and every other call returns
//! [`CoreError::Unavailable`]; each body then becomes one call into the client.

// WAITING(w3-client): the stubs have nothing to await yet.
#![expect(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    reason = "each stub awaits its pingers-client call once that merges"
)]

use std::sync::Arc;

use crate::CoreError;
use crate::callbacks::{Events, EventsAdapter, Platform, PlatformAdapter};
use crate::geo::Fix;
use crate::ids::{LocationRequestId, MeetupId, MemberId, MessageId, PartyId, SpotId};
use crate::snapshot::Snapshot;
use crate::views::{AnswerDraft, MeetPointDraft, PartyDraft, Reachability, SpotDraft, Visibility};

/// A change to a device-wide privacy setting, as pingers-client's `SettingChange` takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SettingChange {
    /// The app-wide sharing switch.
    LocationSharing {
        /// On or off.
        on: bool,
    },
    /// The manual pause.
    Paused {
        /// Paused or not.
        paused: bool,
    },
    /// "Pause during Focus".
    PauseInFocus {
        /// On or off.
        on: bool,
    },
    /// Quiet hours on or off (the window is kept).
    QuietHours {
        /// On or off.
        on: bool,
    },
    /// "Hide trail near zones".
    HideTrail {
        /// On or off.
        on: bool,
    },
    /// Who sees the exact spot in Meetups.
    Visibility {
        /// The choice.
        visibility: Visibility,
    },
    /// "No wake-up pushes (maximum privacy)".
    WakePushes {
        /// Whether wake-up pushes are registered.
        on: bool,
    },
}

/// The error every call returns until pingers-client's API merges.
fn not_yet() -> CoreError {
    CoreError::Unavailable {
        reason: "pingers-client's API has not merged yet".to_owned(),
    }
}

/// The Rust core: one per process.
#[derive(uniffi::Object)]
pub struct Core {
    /// The encrypted database's path.
    _db_path: String,
    /// The app's platform, as pingers-client takes it.
    _platform: Arc<PlatformAdapter>,
    /// The app's event sink, as pingers-client takes it.
    _events: Arc<EventsAdapter>,
}

impl core::fmt::Debug for Core {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Core")
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Core {
    /// Opens the core on the database at `db_path`.
    ///
    /// # Errors
    ///
    /// [`CoreError::Storage`] if the database cannot be opened.
    #[uniffi::constructor]
    pub fn open(
        db_path: String,
        platform: Arc<dyn Platform>,
        events: Arc<dyn Events>,
    ) -> Result<Arc<Self>, CoreError> {
        Ok(Arc::new(Self {
            _db_path: db_path,
            _platform: Arc::new(PlatformAdapter(platform)),
            _events: Arc::new(EventsAdapter(events)),
        }))
    }

    /// The state now, for the screens.
    ///
    /// # Errors
    ///
    /// [`CoreError::Unavailable`] until the client is open.
    pub fn snapshot(&self) -> Result<Arc<Snapshot>, CoreError> {
        Err(not_yet())
    }

    /// Registers this device with a server.
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`] if the server cannot be reached or refuses.
    pub async fn register(
        &self,
        server_url: String,
        registration_code: Option<String>,
    ) -> Result<(), CoreError> {
        let _ = (server_url, registration_code);
        Err(not_yet())
    }

    /// Asks a server or relay who runs it, for New party and Advanced (`Reachability`).
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a host that is not one; [`CoreError::Network`] if it does not
    /// answer.
    pub async fn check_service(&self, host: String) -> Result<Reachability, CoreError> {
        let _ = pingers_screens::common::Host::new(&host)?;
        Err(not_yet())
    }

    /// Creates a Party from New party's draft.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a draft that cannot be created; [`CoreError::Network`].
    pub async fn create_party(
        &self,
        draft: PartyDraft,
        my_name: String,
    ) -> Result<PartyId, CoreError> {
        let _ = (draft, my_name);
        Err(not_yet())
    }

    /// Makes an invite link for a Party.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`], [`CoreError::Network`].
    pub async fn create_invite(&self, party: PartyId) -> Result<String, CoreError> {
        let _ = party;
        Err(not_yet())
    }

    /// Opens an invite link, giving the name this device's user goes by in that Party: the
    /// preview then shows on Accept invite.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a link that is not an invite or an empty name;
    /// [`CoreError::Network`].
    pub async fn open_invite(&self, link: String, my_name: String) -> Result<(), CoreError> {
        let _ = (
            pingers_screens::snapshot::InviteUrl::new(&link)?,
            pingers_screens::common::Name::new(&my_name)?,
        );
        Err(not_yet())
    }

    /// Joins the Party of the opened invite, after the codes were compared.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] if no invite is open; [`CoreError::Network`].
    pub async fn accept_invite(&self) -> Result<PartyId, CoreError> {
        Err(not_yet())
    }

    /// Changes a device-wide privacy setting (Privacy settings, Exclusion zones).
    ///
    /// # Errors
    ///
    /// [`CoreError::Storage`] if it cannot be saved.
    pub async fn change_setting(&self, change: SettingChange) -> Result<(), CoreError> {
        let _ = change;
        Err(not_yet())
    }

    /// Adds a Spot (`spot` is `None`) or replaces one.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a draft that cannot be saved; [`CoreError::NotFound`].
    pub async fn upsert_spot(
        &self,
        party: PartyId,
        spot: Option<SpotId>,
        draft: SpotDraft,
    ) -> Result<SpotId, CoreError> {
        let _ = (party, spot, draft);
        Err(not_yet())
    }

    /// Deletes a Spot.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`].
    pub async fn delete_spot(&self, party: PartyId, spot: SpotId) -> Result<(), CoreError> {
        let _ = (party, spot);
        Err(not_yet())
    }

    /// A fix from the platform.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an impossible fix.
    pub async fn on_location(&self, fix: Fix) -> Result<(), CoreError> {
        let _ = pingers_geo::Fix::try_from(fix)?;
        Err(not_yet())
    }

    /// An OS geofence transition (`entered` false: exited).
    ///
    /// # Errors
    ///
    /// [`CoreError::Unavailable`] until the client is open.
    pub async fn on_os_geofence(&self, region_id: String, entered: bool) -> Result<(), CoreError> {
        let _ = (region_id, entered);
        Err(not_yet())
    }

    /// A background wake.
    ///
    /// # Errors
    ///
    /// [`CoreError::Unavailable`] until the client is open.
    pub async fn on_wake(&self) -> Result<(), CoreError> {
        Err(not_yet())
    }

    /// Sends a chat message, or a reply in `parent`'s thread.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an empty or too long body; [`CoreError::NotFound`].
    pub async fn send_chat(
        &self,
        meetup: MeetupId,
        body: String,
        parent: Option<MessageId>,
    ) -> Result<MessageId, CoreError> {
        let _ = (meetup, pingers_proto::ChatBody::new(body)?, parent);
        Err(not_yet())
    }

    /// Shares a meet point.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`], [`CoreError::Invalid`].
    pub async fn set_meet_point(
        &self,
        meetup: MeetupId,
        draft: MeetPointDraft,
    ) -> Result<(), CoreError> {
        let _ = (meetup, draft);
        Err(not_yet())
    }

    /// Asks a member to see their exact location.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`].
    pub async fn ask_location(
        &self,
        meetup: MeetupId,
        peer: MemberId,
    ) -> Result<LocationRequestId, CoreError> {
        let _ = (meetup, peer);
        Err(not_yet())
    }

    /// Answers a location request: `Some` allows with that answer, `None` is "Not now".
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] if the request lapsed.
    pub async fn answer_location_request(
        &self,
        meetup: MeetupId,
        request: LocationRequestId,
        answer: Option<AnswerDraft>,
    ) -> Result<(), CoreError> {
        let _ = (meetup, request, answer);
        Err(not_yet())
    }

    /// Leaves a Meetup ("Hide me here").
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`].
    pub async fn leave_meetup(&self, meetup: MeetupId) -> Result<(), CoreError> {
        let _ = meetup;
        Err(not_yet())
    }
}
