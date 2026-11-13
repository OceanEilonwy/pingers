// SPDX-License-Identifier: AGPL-3.0-only
//! [`Core`]: pingers-client's [`Client`] for the app.
//!
//! Each method converts its arguments, makes one client call and converts the answer. Reads go
//! through [`Core::snapshot`] and the screens on [`Snapshot`]. The constructor and the actions
//! are Kotlin `suspend` functions on the tokio runtime.
//!
//! [`Core::open`] opens the client on the real L3 crates (`Client::open`); pingers-sync's
//! parts answer [`CoreError::Unavailable`] until it merges. This module's tests use
//! pingers-testkit's `FakeCrates` through [`Core::open_with`].

use std::path::PathBuf;
use std::sync::Arc;

use pingers_client::ports::{self, Foreground, GeofenceTransition};
use pingers_client::{Client, ClientConfig, ClientError};

use crate::CoreError;
use crate::callbacks::{Events, EventsAdapter, Platform, PlatformAdapter};
use crate::geo::{Coordinate, Fence, Fix};
use crate::ids::{LocationRequestId, MeetupId, MemberId, MessageId, PartyId, SpotId};
use crate::snapshot::Snapshot;
use crate::views::{AnswerDraft, PartyColour, Visibility};

impl From<ClientError> for CoreError {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "ClientError is non-exhaustive; a new error is reported as unavailable"
    )]
    fn from(error: ClientError) -> Self {
        let reason = error.to_string();
        match error {
            ClientError::Platform(_)
            | ClientError::NoDatabaseKey
            | ClientError::MalformedDatabaseKey
            | ClientError::Store(_)
            | ClientError::Records(_) => Self::Storage { reason },
            ClientError::Parties(_) | ClientError::Wake(_) | ClientError::MeetupSession(_) => {
                Self::Network { reason }
            }
            _ => Self::Unavailable { reason },
        }
    }
}

/// A new Party, as New party has it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NewPartyDraft {
    /// The Party's name.
    pub name: String,
    /// The name this device's user goes by in it.
    pub my_name: String,
    /// Its colour.
    pub colour: PartyColour,
    /// Whether it uses the X-Wing suite.
    pub post_quantum: bool,
}

impl TryFrom<NewPartyDraft> for ports::PartyDraft {
    type Error = CoreError;

    fn try_from(draft: NewPartyDraft) -> Result<Self, CoreError> {
        Ok(Self {
            name: pingers_client::snapshot::Name::new(&draft.name)?,
            my_name: pingers_client::snapshot::Name::new(&draft.my_name)?,
            colour: draft.colour.into(),
            post_quantum: if draft.post_quantum {
                pingers_client::snapshot::PostQuantum::On
            } else {
                pingers_client::snapshot::PostQuantum::Off
            },
        })
    }
}

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
    /// Quiet hours on or off; the window stays as it is.
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
}

/// A meet point to share, as pingers-meetup's `MeetPointDraft` takes it.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum MeetPointChoice {
    /// A fixed place (a dropped pin, or the midpoint the screen showed).
    Fixed {
        /// Where.
        position: Coordinate,
        /// What to call it.
        label: Option<String>,
    },
    /// "At me": this device's live position.
    AtMe,
    /// No meet point.
    Clear,
}

impl TryFrom<MeetPointChoice> for pingers_meetup::MeetPointDraft {
    type Error = CoreError;

    fn try_from(choice: MeetPointChoice) -> Result<Self, CoreError> {
        Ok(match choice {
            MeetPointChoice::Fixed { position, label } => Self::Fixed {
                position: position.try_into()?,
                label: label
                    .filter(|label| !label.trim().is_empty())
                    .map(pingers_proto::MeetPointLabel::new)
                    .transpose()?,
            },
            MeetPointChoice::AtMe => Self::AtMe,
            MeetPointChoice::Clear => Self::Clear,
        })
    }
}

impl From<AnswerDraft> for pingers_meetup::LocationAnswer {
    fn from(answer: AnswerDraft) -> Self {
        let draft = pingers_screens::location_request::AnswerDraft::from(answer);
        Self::Allow {
            scope: draft.scope,
            ask_back: draft.ask_back,
            always: draft.always,
        }
    }
}

/// A Spot to save, as pingers-client's `SpotDraft` takes it.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SpotToSave {
    /// The Spot being edited, or `None` for a new one.
    pub id: Option<SpotId>,
    /// Its name.
    pub name: String,
    /// Its shape.
    pub fence: Fence,
}

/// An OS geofence transition, as the app reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum GeofenceEvent {
    /// Entered the region.
    Enter,
    /// Stayed inside for the loitering delay.
    Dwell,
    /// Left the region.
    Exit,
}

impl From<GeofenceEvent> for GeofenceTransition {
    fn from(event: GeofenceEvent) -> Self {
        match event {
            GeofenceEvent::Enter => Self::Enter,
            GeofenceEvent::Dwell => Self::Dwell,
            GeofenceEvent::Exit => Self::Exit,
        }
    }
}

/// The Rust core: one per process.
#[derive(uniffi::Object)]
pub struct Core {
    client: Client,
}

impl Core {
    /// Opens the client with the L3 crates `build` makes from the database (tests).
    #[cfg(test)]
    async fn open_with(
        db_path: String,
        platform: Arc<dyn Platform>,
        events: Arc<dyn Events>,
        build: impl FnOnce(pingers_store::Database) -> Result<ports::Crates, ClientError>,
    ) -> Result<Arc<Self>, CoreError> {
        let client = Client::open_with(
            ClientConfig {
                database: PathBuf::from(db_path),
            },
            Arc::new(PlatformAdapter(platform)),
            Arc::new(EventsAdapter(events)),
            build,
        )
        .await?;
        Ok(Arc::new(Self { client }))
    }
}

impl core::fmt::Debug for Core {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Core")
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Core {
    /// Opens the core on the database at `db_path`, with the key the platform's secure store
    /// holds under `pingers.database-key`.
    ///
    /// # Errors
    ///
    /// [`CoreError::Storage`] if the key is missing or the database does not open.
    #[uniffi::constructor]
    pub async fn open(
        db_path: String,
        platform: Arc<dyn Platform>,
        events: Arc<dyn Events>,
    ) -> Result<Arc<Self>, CoreError> {
        let client = Client::open(
            ClientConfig {
                database: PathBuf::from(db_path),
            },
            Arc::new(PlatformAdapter(platform)),
            Arc::new(EventsAdapter(events)),
        )
        .await?;
        Ok(Arc::new(Self { client }))
    }

    /// The latest published state, for the screens.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Snapshot::new(self.client.snapshot())
    }

    /// Creates a Party.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an empty name; [`CoreError::Network`].
    pub async fn create_party(&self, draft: NewPartyDraft) -> Result<PartyId, CoreError> {
        Ok(self.client.create_party(draft.try_into()?).await?.into())
    }

    /// Makes an invite link for a Party; returns the link.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`], [`CoreError::Network`].
    pub async fn create_invite(&self, party: PartyId) -> Result<String, CoreError> {
        let link = self.client.create_invite((&party).try_into()?).await?;
        Ok(link.url.as_str().to_owned())
    }

    /// Opens an invite link, giving the name this device's user goes by in that Party: the
    /// preview then shows on Accept invite.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a link that is not one or an empty name.
    pub async fn open_invite(&self, link: String, my_name: String) -> Result<(), CoreError> {
        let link = pingers_client::snapshot::InviteUrl::new(&link)?;
        let name = pingers_client::snapshot::Name::new(&my_name)?;
        Ok(self.client.open_invite(link, name).await?)
    }

    /// Joins the opened invite's Party once the codes matched.
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`].
    pub async fn accept_invite(&self) -> Result<PartyId, CoreError> {
        Ok(self.client.accept_invite().await?.into())
    }

    /// Leaves a Party.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`], [`CoreError::Network`].
    pub async fn leave_party(&self, party: PartyId) -> Result<(), CoreError> {
        Ok(self.client.leave_party((&party).try_into()?).await?)
    }

    /// Adds a Spot or replaces one.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an empty name or an impossible shape.
    pub async fn save_spot(&self, party: PartyId, spot: SpotToSave) -> Result<SpotId, CoreError> {
        let draft = ports::SpotDraft {
            id: spot.id.map(|id| (&id).try_into()).transpose()?,
            name: pingers_client::snapshot::Name::new(&spot.name)?,
            fence: (&spot.fence).try_into()?,
        };
        Ok(self
            .client
            .save_spot((&party).try_into()?, draft)
            .await?
            .into())
    }

    /// Deletes a Spot.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`], [`CoreError::Network`].
    pub async fn delete_spot(&self, party: PartyId, spot: SpotId) -> Result<(), CoreError> {
        Ok(self
            .client
            .delete_spot((&party).try_into()?, (&spot).try_into()?)
            .await?)
    }

    /// Changes a device-wide privacy setting.
    ///
    /// # Errors
    ///
    /// [`CoreError::Storage`].
    pub async fn change_setting(&self, change: SettingChange) -> Result<(), CoreError> {
        use pingers_client::snapshot::{HideTrail, PauseInFocus, QuietHoursPreference, Toggle};
        use pingers_gate::{LocationSharing, PauseState};
        let change = match change {
            SettingChange::LocationSharing { on } => ports::SettingChange::LocationSharing(if on {
                LocationSharing::On
            } else {
                LocationSharing::Off
            }),
            SettingChange::Paused { paused } => ports::SettingChange::Pause(if paused {
                PauseState::Paused
            } else {
                PauseState::NotPaused
            }),
            SettingChange::PauseInFocus { on } => ports::SettingChange::PauseInFocus(if on {
                PauseInFocus::On
            } else {
                PauseInFocus::Off
            }),
            SettingChange::QuietHours { on } => {
                ports::SettingChange::QuietHours(QuietHoursPreference {
                    state: if on { Toggle::On } else { Toggle::Off },
                    window: self.client.snapshot().settings.quiet_hours.window,
                })
            }
            SettingChange::HideTrail { on } => {
                ports::SettingChange::HideTrail(if on { HideTrail::On } else { HideTrail::Off })
            }
            SettingChange::Visibility { visibility } => {
                ports::SettingChange::Visibility(visibility.into())
            }
        };
        Ok(self.client.change_setting(change).await?)
    }

    /// Wake-up pushes on (`on` true) or "No wake-up pushes (maximum privacy)".
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`].
    pub async fn set_wake_pushes(&self, on: bool) -> Result<(), CoreError> {
        use pingers_client::snapshot::WakePushes;
        let setting = if on {
            WakePushes::On
        } else {
            WakePushes::NoWakePushes
        };
        Ok(self.client.set_wake_pushes(setting).await?)
    }

    /// The push registration changed.
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`].
    pub async fn push_tokens_changed(&self) -> Result<(), CoreError> {
        Ok(self.client.push_tokens_changed().await?)
    }

    /// Asks a member to see their exact location.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`], [`CoreError::Network`].
    pub async fn ask_location(
        &self,
        meetup: MeetupId,
        peer: MemberId,
    ) -> Result<LocationRequestId, CoreError> {
        Ok(self
            .client
            .ask_location((&meetup).try_into()?, peer.into())
            .await?
            .into())
    }

    /// Answers a location request: `Some` allows with that answer, `None` is "Not now".
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`], [`CoreError::Network`].
    pub async fn answer_location_request(
        &self,
        meetup: MeetupId,
        request: LocationRequestId,
        answer: Option<AnswerDraft>,
    ) -> Result<(), CoreError> {
        let answer = answer.map_or(pingers_meetup::LocationAnswer::NotNow, Into::into);
        Ok(self
            .client
            .answer_location_request((&meetup).try_into()?, (&request).try_into()?, answer)
            .await?)
    }

    /// Stops sharing this device's exact location with a member.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`], [`CoreError::Network`].
    pub async fn revoke_location(&self, meetup: MeetupId, peer: MemberId) -> Result<(), CoreError> {
        Ok(self
            .client
            .revoke_location((&meetup).try_into()?, peer.into())
            .await?)
    }

    /// Sends a chat message, or a reply in `parent`'s thread.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an empty or too long body; [`CoreError::Network`].
    pub async fn send_chat(
        &self,
        meetup: MeetupId,
        body: String,
        parent: Option<MessageId>,
    ) -> Result<MessageId, CoreError> {
        let parent = parent.map(|id| (&id).try_into()).transpose()?;
        let body = pingers_proto::ChatBody::new(body)?;
        Ok(self
            .client
            .send_chat((&meetup).try_into()?, body, parent)
            .await?
            .into())
    }

    /// Shares a meet point.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`], [`CoreError::Network`].
    pub async fn set_meet_point(
        &self,
        meetup: MeetupId,
        point: MeetPointChoice,
    ) -> Result<(), CoreError> {
        Ok(self
            .client
            .set_meet_point((&meetup).try_into()?, point.try_into()?)
            .await?)
    }

    /// Leaves a Meetup ("Hide me here").
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`], [`CoreError::Network`].
    pub async fn leave_meetup(&self, meetup: MeetupId) -> Result<(), CoreError> {
        Ok(self.client.leave_meetup((&meetup).try_into()?).await?)
    }

    /// A fix from the platform.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an impossible fix.
    pub async fn on_location(&self, fix: Fix) -> Result<(), CoreError> {
        Ok(self.client.on_location(fix.try_into()?).await?)
    }

    /// An OS geofence transition.
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`].
    pub async fn on_os_geofence(
        &self,
        region_id: String,
        transition: GeofenceEvent,
    ) -> Result<(), CoreError> {
        Ok(self
            .client
            .on_os_geofence(region_id, transition.into())
            .await?)
    }

    /// A background wake.
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`].
    pub async fn on_wake(&self) -> Result<(), CoreError> {
        Ok(self.client.on_wake().await?)
    }

    /// A push arrived carrying `handle` (8 bytes).
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a handle of the wrong length; [`CoreError::Network`].
    pub async fn on_push(&self, handle: Vec<u8>) -> Result<(), CoreError> {
        let handle = <[u8; pingers_proto::PushHandle::LENGTH]>::try_from(handle)
            .map_err(|_| CoreError::invalid("a push handle is 8 bytes"))?;
        Ok(self
            .client
            .on_push(pingers_proto::PushHandle::from_bytes(handle))
            .await?)
    }

    /// The Focus or DND state changed.
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`].
    pub async fn on_dnd_changed(&self) -> Result<(), CoreError> {
        Ok(self.client.on_dnd_changed().await?)
    }

    /// The app came on screen (`true`) or left it.
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`].
    pub async fn on_foreground(&self, visible: bool) -> Result<(), CoreError> {
        let foreground = if visible {
            Foreground::Visible
        } else {
            Foreground::Hidden
        };
        Ok(self.client.on_foreground(foreground).await?)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use super::{Core, NewPartyDraft, SettingChange};
    use crate::CoreError;
    use crate::callbacks::{
        Events, FixQuality, FocusState, LocationMode, Notice, OsRegion, Platform, PlatformError,
        PushTokens,
    };
    use crate::ids::{LocationRequestId, MeetupId, MemberId, PartyId};
    use crate::views::PartyColour;

    /// The app's side, as Kotlin would implement it: a secure store holding a database key.
    #[derive(Default)]
    struct App(Mutex<HashMap<String, Vec<u8>>>);

    impl Platform for App {
        fn secure_store_get(&self, key: String) -> Result<Option<Vec<u8>>, PlatformError> {
            Ok(self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&key)
                .cloned())
        }
        fn secure_store_put(&self, key: String, value: Vec<u8>) -> Result<(), PlatformError> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, value);
            Ok(())
        }
        fn set_os_geofences(&self, _: Vec<OsRegion>) {}
        fn request_fix(&self, _: FixQuality, _: u32) {}
        fn set_location_mode(&self, _: LocationMode) {}
        fn focus_state(&self) -> FocusState {
            FocusState::Unknown
        }
        fn schedule_wake(&self, _: u32) {}
        fn push_tokens(&self) -> Option<PushTokens> {
            None
        }
    }

    impl Events for App {
        fn parties_changed(&self) {}
        fn join_requested(&self, _: PartyId) {}
        fn meetup_started(&self, _: MeetupId) {}
        fn meetup_updated(&self, _: MeetupId) {}
        fn meetup_message(&self, _: MeetupId) {}
        fn peer_location(&self, _: MeetupId, _: MemberId) {}
        fn location_request(&self, _: MeetupId, _: MemberId, _: LocationRequestId) {}
        fn location_access_changed(&self, _: MeetupId, _: MemberId) {}
        fn meetup_ended(&self, _: MeetupId) {}
        fn notice(&self, _: Notice) {}
        fn privacy_status_changed(&self) {}
    }

    fn db_path(name: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("pingers-ffi-{}-{name}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path.to_string_lossy().into_owned()
    }

    async fn open(name: &str) -> Result<Arc<Core>, CoreError> {
        let app = Arc::new(App::default());
        app.secure_store_put("pingers.database-key".into(), vec![7; 32])
            .map_err(|_| CoreError::invalid("store"))?;
        let fakes = pingers_testkit::FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        Core::open_with(db_path(name), app.clone(), app, move |_| Ok(fakes.crates())).await
    }

    #[tokio::test]
    async fn calls_reach_the_client_and_its_crates() -> Result<(), CoreError> {
        let core = open("calls").await?;
        let party = core
            .create_party(NewPartyDraft {
                name: "Climbing crew".into(),
                my_name: "Sam".into(),
                colour: PartyColour::Blue,
                post_quantum: false,
            })
            .await?;
        assert_eq!(party.hex, "a1".repeat(16), "FakeCrates' party id");
        core.change_setting(SettingChange::HideTrail { on: true })
            .await?;
        assert!(
            core.snapshot().home().parties.is_empty(),
            "the fake section is empty"
        );
        Ok(())
    }

    #[tokio::test]
    async fn bad_input_is_refused_before_the_client() -> Result<(), CoreError> {
        let core = open("refused").await?;
        let empty = NewPartyDraft {
            name: " ".into(),
            my_name: "Sam".into(),
            colour: PartyColour::Blue,
            post_quantum: false,
        };
        assert!(matches!(
            core.create_party(empty).await,
            Err(CoreError::Invalid { .. })
        ));
        assert!(matches!(
            core.on_push(vec![1, 2]).await,
            Err(CoreError::Invalid { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn without_a_database_key_the_core_does_not_open() {
        let app = Arc::new(App::default());
        let opened = Core::open(db_path("nokey"), app.clone(), app).await;
        assert!(matches!(opened, Err(CoreError::Storage { .. })));
    }
}
