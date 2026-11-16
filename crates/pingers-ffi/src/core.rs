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
use pingers_screens as screens;
use pingers_words::Language;

use crate::CoreError;
use crate::callbacks::{Events, EventsAdapter, Platform, PlatformAdapter, PushPriority};
use crate::geo::{Coordinate, Fence, Fix};
use crate::ids::{
    InviteId, LocationRequestId, MeetupId, MemberId, MessageId, PartyId, SpotId, ZoneId,
};
use crate::maps::{
    ListenerAdapter, MapCatalog, MapDownloadListener, MapDownloadRequest, MapManifest, MapSettings,
};
use crate::snapshot::Snapshot;
use crate::views::{AnswerDraft, PartyColour, RelayCheck, TimeOfDay, Visibility, ZoneTimes};

impl From<ClientError> for CoreError {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "ClientError is non-exhaustive; a new error is reported as unavailable"
    )]
    fn from(error: ClientError) -> Self {
        if let Some(issue) = crate::servers::ServerIssue::of(&error) {
            return issue.error();
        }
        match &error {
            ClientError::RegistrationCodeRequired => {
                return Self::CodeNeeded {
                    reason: pingers_words::errors::code_needed(Language::English),
                };
            }
            ClientError::RegistrationCodeRefused => {
                return Self::CodeRefused {
                    reason: pingers_words::errors::code_refused(Language::English),
                };
            }
            _ => {}
        }
        let reason = match &error {
            // Say what the call waits for, so the app's log names the missing piece.
            ClientError::Unavailable(waiting) => format!("{error}: {waiting}"),
            _ => error.to_string(),
        };
        match error {
            ClientError::Maps(error) => Self::Map {
                failure: (&error).into(),
                reason,
            },

            ClientError::Platform(_)
            | ClientError::NoDatabaseKey
            | ClientError::MalformedDatabaseKey
            | ClientError::Store(_)
            | ClientError::Records(_) => Self::Storage { reason },
            ClientError::Parties(_)
            | ClientError::Sync(_)
            | ClientError::Wake(_)
            | ClientError::MeetupSession(_) => Self::Network { reason },
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
    /// "Another server", by host or origin; `None` for the app's own server. It must be one
    /// this device trusts: `probe_server`, then `trust_server` ("Trust this server's keys"),
    /// or an invite.
    pub other_server: Option<String>,
    /// A registration code the user typed ("This server needs a code").
    pub registration_code: Option<String>,
}

impl TryFrom<NewPartyDraft> for ports::PartyDraft {
    type Error = CoreError;

    fn try_from(draft: NewPartyDraft) -> Result<Self, CoreError> {
        Ok(Self {
            server: draft
                .other_server
                .as_deref()
                .map(str::trim)
                .filter(|address| !address.is_empty())
                .map(pingers_client::server_address)
                .transpose()?,
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

/// Quiet hours as the user set them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct QuietHoursDraft {
    /// Whether they apply.
    pub on: bool,
    /// Where the window starts.
    pub from: TimeOfDay,
    /// Where it ends (past midnight if earlier than `from`).
    pub to: TimeOfDay,
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

/// The Pingers instance the app talks to, from the app's build configuration
/// (`PINGERS_SERVER_HOST`, `PINGERS_OHTTP_RELAY`, `PINGERS_PUSH_RELAY`, the key pins, and in
/// debug builds `PINGERS_EXTRA_CA` and `PINGERS_DEV_REGISTRATION_CODE`) or an invite.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InstanceConfig {
    /// The server and OHTTP gateway, `https://...`.
    pub server: String,
    /// The OHTTP relay, `https://...`.
    pub ohttp_relay: String,
    /// The push relay, `https://...`, if push is set up.
    pub push_relay: Option<String>,
    /// `PINGERS_GATEWAY_KEY_PINS`: the gateway key config hashes, base64url SHA-256,
    /// comma-separated (the key served now and the next one). `None` pins on first use.
    pub gateway_key_pins: Option<String>,
    /// `PINGERS_PUSH_RELAY_KEY_PINS`, in the same form. `None` pins on first use.
    pub push_relay_key_pins: Option<String>,
    /// `PINGERS_EXTRA_CA`: a PEM bundle trusted instead of Mozilla's roots, for a local test
    /// stack. The app passes it only in debug builds; release builds pass `None`.
    pub extra_ca_pem: Option<String>,
    /// `PINGERS_DEV_REGISTRATION_CODE`: the operator's registration code for this device's first
    /// registration, for the first owner of a server that requires one. The app passes it only
    /// in debug builds; release builds pass `None` (other devices get the code from an invite).
    pub registration_code: Option<String>,
}

impl TryFrom<InstanceConfig> for pingers_client::Instance {
    type Error = CoreError;

    fn try_from(config: InstanceConfig) -> Result<Self, CoreError> {
        let server = pingers_proto::ServerId::new(config.server)
            .map_err(|_invalid| CoreError::invalid("the server must be an https:// URL"))?;
        let pins = |text: Option<String>, what: &'static str| {
            text.filter(|text| !text.trim().is_empty())
                .map(|text| pingers_client::KeyPins::parse(&text))
                .transpose()
                .map_err(|_invalid| CoreError::invalid(what))
        };
        Ok(Self {
            server,
            relay: config.ohttp_relay,
            push_relay: config.push_relay,
            gateway_pins: pins(
                config.gateway_key_pins,
                "the gateway key pins must be base64url SHA-256 hashes",
            )?,
            push_relay_pins: pins(
                config.push_relay_key_pins,
                "the push relay key pins must be base64url SHA-256 hashes",
            )?,
            trust: match config.extra_ca_pem.filter(|pem| !pem.trim().is_empty()) {
                None => pingers_client::Trust::Mozilla,
                Some(pem) => pingers_client::Trust::from_pem(&pem).map_err(|_invalid| {
                    CoreError::invalid("the extra CA is not a PEM certificate")
                })?,
            },
            registration_code: config
                .registration_code
                .filter(|code| !code.trim().is_empty())
                .map(pingers_proto::SecretText::new),
        })
    }
}

/// The Rust core: one per process.
#[derive(uniffi::Object)]
pub struct Core {
    client: Client,
    /// The app's events, also told what each wake changed (`notifications.rs`).
    events: Arc<dyn Events>,
}

impl Core {
    /// Lets in or turns away a join request.
    async fn decide(
        &self,
        party: PartyId,
        request: InviteId,
        decision: ports::JoinDecision,
    ) -> Result<(), CoreError> {
        Ok(self
            .client
            .decide_join((&party).try_into()?, (&request).try_into()?, decision)
            .await?)
    }

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
                instance: None,
            },
            Arc::new(PlatformAdapter(platform)),
            Arc::new(EventsAdapter(Arc::clone(&events))),
            build,
        )
        .await?;
        Ok(Arc::new(Self { client, events }))
    }
}

impl Core {
    /// Runs a wake (a push, a fix, a geofence, a scheduled wake...) and tells the app what it
    /// changed: notifications to post and the events the screens follow.
    async fn woke(
        &self,
        wake: impl core::future::Future<Output = Result<(), ClientError>>,
    ) -> Result<(), CoreError> {
        let before = self.client.snapshot();
        let result = wake.await;
        let after = self.client.snapshot();
        crate::notifications::tell(
            self.events.as_ref(),
            &pingers_screens::notifications::changes(&before, &after),
        );
        Ok(result?)
    }
}

impl Core {
    /// The client, for the exports in other modules.
    pub(crate) const fn client(&self) -> &Client {
        &self.client
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
    /// holds under `pingers.database-key`, talking to `instance` (from the app's build
    /// configuration, or an invite).
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] if the instance's server or relay is not an `https://` URL;
    /// [`CoreError::Storage`] if the key is missing or the database does not open.
    #[uniffi::constructor]
    pub async fn open(
        db_path: String,
        instance: InstanceConfig,
        platform: Arc<dyn Platform>,
        events: Arc<dyn Events>,
    ) -> Result<Arc<Self>, CoreError> {
        let client = Client::open(
            ClientConfig {
                database: PathBuf::from(db_path),
                instance: Some(instance.try_into()?),
            },
            Arc::new(PlatformAdapter(platform)),
            Arc::new(EventsAdapter(Arc::clone(&events))),
        )
        .await?;
        Ok(Arc::new(Self { client, events }))
    }

    /// The latest published state, for the screens.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Snapshot::new(self.client.snapshot())
    }

    /// Creates a Party, on the app's server or on another one this device trusts, with the
    /// registration code the user typed if the server asked for one.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an empty name; [`CoreError::CodeNeeded`] or
    /// [`CoreError::CodeRefused`]; [`CoreError::Server`] for an address that is not one, or a
    /// server whose keys were not trusted (`NotLookedAt`); [`CoreError::Network`].
    pub async fn create_party(&self, draft: NewPartyDraft) -> Result<PartyId, CoreError> {
        let code = draft
            .registration_code
            .as_deref()
            .map(str::trim)
            .filter(|code| !code.is_empty())
            .map(|code| pingers_proto::SecretText::new(code.to_owned()));
        let draft: ports::PartyDraft = draft.try_into()?;
        // A typed code goes to the chosen server first; refused, no Party is created.
        if let Some(code) = code {
            self.client
                .submit_registration_code(draft.server.clone(), code)
                .await?;
        }
        Ok(self.client.create_party(draft).await?.into())
    }

    /// The safety code between this device and a member (Safety codes): twelve groups of
    /// five digits.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an id that is not one; [`CoreError::Network`] for a Party
    /// or member this device does not hold.
    pub async fn safety_code(&self, party: PartyId, member: MemberId) -> Result<String, CoreError> {
        let code = self
            .client
            .safety_code((&party).try_into()?, member.into())
            .await?;
        Ok(code.as_str().to_owned())
    }

    /// Marks a member checked once the codes matched ("Codes match · Mark Ana as checked").
    ///
    /// # Errors
    ///
    /// As [`Core::safety_code`].
    pub async fn mark_checked(&self, party: PartyId, member: MemberId) -> Result<(), CoreError> {
        Ok(self
            .client
            .mark_checked((&party).try_into()?, member.into())
            .await?)
    }

    /// Deletes an exclusion zone ("Delete zone").
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] for a zone that is gone; [`CoreError::Storage`].
    pub async fn delete_zone(&self, zone: ZoneId) -> Result<(), CoreError> {
        let zones = screens::zones::without(&self.client.snapshot(), zone.into())?;
        Ok(self
            .client
            .change_setting(ports::SettingChange::Zones(zones))
            .await?)
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

    /// Asks to join through an invite link, under the name this device's user goes by in that
    /// Party (Open invite's "Ask to join"): `Snapshot::asked_to_join` then shows the code.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a link that is not one or an empty name.
    pub async fn open_invite(&self, link: String, my_name: String) -> Result<(), CoreError> {
        let link = pingers_client::snapshot::InviteUrl::new(&link)?;
        let name = pingers_client::snapshot::Name::new(&my_name)?;
        Ok(self.client.open_invite(link, name).await?)
    }

    /// Withdraws the join this device asked for (Asked to join's "Withdraw request").
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`] with no join under way, or if the withdrawal cannot be stored.
    pub async fn withdraw_join(&self) -> Result<(), CoreError> {
        Ok(self.client.withdraw_join().await?)
    }

    /// Forgets You're in once the user opened the Party (its "Open" button).
    ///
    /// # Errors
    ///
    /// [`CoreError::Storage`].
    pub async fn dismiss_join(&self) -> Result<(), CoreError> {
        Ok(self.client.dismiss_join().await?)
    }

    /// Reads what joins wait for now. Asked to join calls it every few seconds while it is on
    /// screen: without wake-up pushes (or before the platform's push token arrives) nothing else
    /// tells the app it was let in.
    ///
    /// # Errors
    ///
    /// [`CoreError::Storage`].
    pub async fn check_joins(&self) -> Result<(), CoreError> {
        Ok(self.client.check_joins().await?)
    }

    /// Lets in the person who asked to join with `request`, once they read out the code the
    /// screen shows ("Codes match · Let Ben in").
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an id that is not one; [`CoreError::Network`] once it was
    /// decided or withdrawn, or the invite expired.
    pub async fn let_in(&self, party: PartyId, request: InviteId) -> Result<(), CoreError> {
        self.decide(party, request, ports::JoinDecision::Admit)
            .await
    }

    /// Turns away the person who asked to join with `request` ("Don't let in"); nothing is
    /// sent to them.
    ///
    /// # Errors
    ///
    /// As [`Core::let_in`].
    pub async fn turn_away(&self, party: PartyId, request: InviteId) -> Result<(), CoreError> {
        self.decide(party, request, ports::JoinDecision::Refuse)
            .await
    }

    /// Cancels an invite that has not been opened yet (Invite people's ✕).
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an id that is not one; [`CoreError::Network`] if the invite
    /// is not this Party's.
    pub async fn cancel_invite(&self, party: PartyId, invite: InviteId) -> Result<(), CoreError> {
        Ok(self
            .client
            .cancel_invite((&party).try_into()?, (&invite).try_into()?)
            .await?)
    }

    /// Sets the name this device's user goes by in Parties they create or join from now on.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an empty or too long name; [`CoreError::Storage`].
    pub async fn set_display_name(&self, name: String) -> Result<(), CoreError> {
        let name = pingers_client::snapshot::Name::new(&name)?;
        Ok(self.client.set_display_name(name).await?)
    }

    /// Checks whether this device reaches its server through the privacy relay. The answer
    /// is only the outcome, and the snapshot's "What your server sees" shows it too.
    ///
    /// # Errors
    ///
    /// [`CoreError::Unavailable`] until pingers-sync merges.
    pub async fn check_reachability(&self) -> Result<RelayCheck, CoreError> {
        Ok(self.client.check_reachability().await?.into())
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

    /// Sets quiet hours: on or off, and the daily window.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] if the window starts where it ends ("The start and end times must
    /// differ.") or a time is not on a 24-hour clock; [`CoreError::Storage`].
    pub async fn set_quiet_hours(&self, quiet_hours: QuietHoursDraft) -> Result<(), CoreError> {
        let preference = screens::settings::quiet_hours(
            if quiet_hours.on {
                pingers_client::snapshot::Toggle::On
            } else {
                pingers_client::snapshot::Toggle::Off
            },
            quiet_hours.from.try_into()?,
            quiet_hours.to.try_into()?,
        )?;
        Ok(self
            .client
            .change_setting(ports::SettingChange::QuietHours(preference))
            .await?)
    }

    /// Sets when an exclusion zone applies.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] for a zone that is gone; [`CoreError::Invalid`] for a window
    /// that starts where it ends or a time not on a 24-hour clock; [`CoreError::Storage`].
    pub async fn set_zone_times(&self, zone: ZoneId, times: ZoneTimes) -> Result<(), CoreError> {
        let zones =
            screens::zones::with_times(&self.client.snapshot(), zone.into(), times.try_into()?)?;
        Ok(self
            .client
            .change_setting(ports::SettingChange::Zones(zones))
            .await?)
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

    /// What the map host chain offers: the user's host, the server's and the publisher's,
    /// directly or through the privacy relay as `settings` say. Hosts that fail are listed
    /// in the catalog.
    ///
    /// # Errors
    ///
    /// [`CoreError::Storage`] if the server's map host cannot be read.
    pub async fn map_catalog(&self, settings: MapSettings) -> Result<MapCatalog, CoreError> {
        let catalog = self
            .client
            .map_catalog(settings.hosts(), settings.route())
            .await?;
        Ok((&catalog).into())
    }

    /// Downloads the request's offer into its directory (the app's maps directory), checks it
    /// against its manifest and installs it, telling `listener` as it goes. Cancelled with the
    /// calling coroutine; a later download resumes it.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an offer that is not a catalog's; [`CoreError::Map`] with
    /// why the download failed.
    pub async fn download_map(
        &self,
        request: MapDownloadRequest,
        listener: Arc<dyn MapDownloadListener>,
    ) -> Result<MapManifest, CoreError> {
        let job = ports::MapDownload {
            offer: (&request.offer).try_into()?,
            route: request.settings.route(),
            directory: PathBuf::from(request.directory),
        };
        let manifest = self
            .client
            .download_map(job, Arc::new(ListenerAdapter(listener)))
            .await?;
        Ok((&manifest).into())
    }

    /// The regions installed in `directory`, for the map screens to draw.
    ///
    /// # Errors
    ///
    /// [`CoreError::Map`] if the directory cannot be read.
    pub async fn installed_maps(&self, directory: String) -> Result<Vec<MapManifest>, CoreError> {
        let installed = self.client.installed_maps(PathBuf::from(directory)).await?;
        Ok(installed.iter().map(Into::into).collect())
    }

    /// Removes region `id` from `directory`.
    ///
    /// # Errors
    ///
    /// [`CoreError::Map`] for an id that is not a region's, or a file that cannot be removed.
    pub async fn delete_map(&self, directory: String, id: String) -> Result<(), CoreError> {
        Ok(self.client.delete_map(PathBuf::from(directory), id).await?)
    }

    /// Trusts the map host's current gateway key for relayed downloads, after it changed.
    ///
    /// # Errors
    ///
    /// [`CoreError::Storage`] if the pin cannot be removed.
    pub async fn forget_map_host_key(&self) -> Result<(), CoreError> {
        Ok(self.client.forget_map_host_key().await?)
    }

    /// A fix from the platform.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an impossible fix.
    pub async fn on_location(&self, fix: Fix) -> Result<(), CoreError> {
        let fix = fix.try_into()?;
        self.woke(self.client.on_location(fix)).await
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
        self.woke(self.client.on_os_geofence(region_id, transition.into()))
            .await
    }

    /// A background wake.
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`].
    pub async fn on_wake(&self) -> Result<(), CoreError> {
        self.woke(self.client.on_wake()).await
    }

    /// A push arrived carrying `handle` (8 bytes).
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a handle of the wrong length; [`CoreError::Network`].
    pub async fn on_push(&self, handle: Vec<u8>) -> Result<(), CoreError> {
        let handle = <[u8; pingers_proto::PushHandle::LENGTH]>::try_from(handle)
            .map_err(|_| CoreError::invalid("a push handle is 8 bytes"))?;
        let handle = pingers_proto::PushHandle::from_bytes(handle);
        self.woke(self.client.on_push(handle)).await
    }

    /// The priority FCM delivered a message at, against the one it was sent with: the core
    /// counts downgrades and marks wake-up pushes downgraded when they become frequent
    /// (design B.4.9).
    ///
    /// # Errors
    ///
    /// [`CoreError::Storage`] if the count cannot be stored.
    pub async fn report_push_priority(
        &self,
        original: PushPriority,
        delivered: PushPriority,
    ) -> Result<(), CoreError> {
        Ok(self
            .client
            .report_push_priority(original.into(), delivered.into())
            .await?)
    }

    /// The Focus or DND state changed.
    ///
    /// # Errors
    ///
    /// [`CoreError::Network`].
    pub async fn on_dnd_changed(&self) -> Result<(), CoreError> {
        self.woke(self.client.on_dnd_changed()).await
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
        self.woke(self.client.on_foreground(foreground)).await
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use pingers_client::ports::{self, JoinDecision, ServerSection};
    use pingers_testkit::{CrateCall, FakeCrates};

    use super::{Core, InstanceConfig, NewPartyDraft, QuietHoursDraft, SettingChange};
    use crate::CoreError;
    use crate::callbacks::{
        Events, FixQuality, FocusState, LocationMode, Notice, OsRegion, Platform, PlatformError,
        PushTokens,
    };
    use crate::ids::{InviteId, LocationRequestId, MeetupId, MemberId, PartyId, ZoneId};
    use crate::views::{PartyColour, RelayStatus, TimeOfDay, ZoneDays, ZoneTimes};

    /// The app's side, as Kotlin would implement it: a secure store holding a database key.
    /// The app: a secure store, and what it was told (events by name, notifications by title
    /// and text).
    #[derive(Default)]
    struct App(Mutex<HashMap<String, Vec<u8>>>, Mutex<Vec<String>>);

    impl App {
        fn note(&self, told: String) {
            self.1
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(told);
        }

        fn told(&self) -> Vec<String> {
            self.1
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

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

    /// Records the join events that reach the app.
    #[derive(Default)]
    struct JoinEvents(Mutex<Vec<&'static str>>);

    impl Events for JoinEvents {
        fn parties_changed(&self) {}
        fn join_requested(&self, _: PartyId) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push("join_requested");
        }
        fn let_in(&self, _: PartyId) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push("let_in");
        }
        fn meetup_started(&self, _: MeetupId) {}
        fn meetup_updated(&self, _: MeetupId) {}
        fn meetup_message(&self, _: MeetupId) {}
        fn peer_location(&self, _: MeetupId, _: MemberId) {}
        fn location_request(&self, _: MeetupId, _: MemberId, _: LocationRequestId) {}
        fn location_access_changed(&self, _: MeetupId, _: MemberId) {}
        fn meetup_ended(&self, _: MeetupId) {}
        fn notice(&self, _: Notice) {}
        fn privacy_status_changed(&self) {}
        fn notify(&self, _: crate::notifications::CoreNotification) {}
    }

    #[test]
    fn join_events_reach_the_app() {
        let events = Arc::new(JoinEvents::default());
        let adapter = crate::callbacks::EventsAdapter(events.clone());
        let party = pingers_proto::PartyId::from_bytes([1; 16]);
        pingers_client::Events::join_requested(&adapter, party);
        pingers_client::Events::let_in(&adapter, party);
        assert_eq!(
            *events.0.lock().unwrap_or_else(PoisonError::into_inner),
            ["join_requested", "let_in"]
        );
    }

    impl Events for App {
        fn parties_changed(&self) {
            self.note("parties changed".into());
        }
        fn join_requested(&self, _: PartyId) {
            self.note("join requested".into());
        }
        fn let_in(&self, _: PartyId) {
            self.note("let in".into());
        }
        fn meetup_started(&self, _: MeetupId) {
            self.note("meetup started".into());
        }
        fn meetup_updated(&self, _: MeetupId) {}
        fn meetup_message(&self, _: MeetupId) {
            self.note("meetup message".into());
        }
        fn peer_location(&self, _: MeetupId, _: MemberId) {}
        fn location_request(&self, _: MeetupId, from: MemberId, _: LocationRequestId) {
            self.note(format!("location request from {}", from.leaf));
        }
        fn location_access_changed(&self, _: MeetupId, _: MemberId) {}
        fn meetup_ended(&self, _: MeetupId) {
            self.note("meetup ended".into());
        }
        fn notice(&self, _: Notice) {}
        fn privacy_status_changed(&self) {}
        fn notify(&self, notification: crate::notifications::CoreNotification) {
            self.note(format!(
                "{:?} {:#x}: {} / {}",
                notification.kind, notification.id, notification.title, notification.body
            ));
        }
    }

    fn db_path(name: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("pingers-ffi-{}-{name}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path.to_string_lossy().into_owned()
    }

    /// An instance on reserved names: nothing is contacted while the core opens.
    fn instance() -> InstanceConfig {
        InstanceConfig {
            server: "https://pingers.invalid".into(),
            ohttp_relay: "https://relay.pingers.invalid".into(),
            push_relay: None,
            gateway_key_pins: None,
            push_relay_key_pins: None,
            extra_ca_pem: None,
            registration_code: None,
        }
    }

    /// Two pins as `cargo xtask pins` prints them.
    const TWO_PINS: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE,\
                            AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI";

    #[test]
    fn build_pins_become_pin_sets_and_blank_ones_mean_first_use() -> Result<(), CoreError> {
        let pinned = pingers_client::Instance::try_from(InstanceConfig {
            gateway_key_pins: Some(TWO_PINS.into()),
            push_relay_key_pins: Some(" ".into()),
            ..instance()
        })?;
        let hashes: Vec<[u8; 32]> = pinned
            .gateway_pins
            .as_ref()
            .map(|pins| pins.hashes().iter().map(|hash| *hash.as_bytes()).collect())
            .unwrap_or_default();
        assert_eq!(hashes, [[1; 32], [2; 32]]);
        assert_eq!(pinned.push_relay_pins, None);
        assert_eq!(pinned.trust, pingers_client::Trust::Mozilla);

        let unpinned = pingers_client::Instance::try_from(instance())?;
        assert_eq!(unpinned.gateway_pins, None);
        Ok(())
    }

    #[test]
    fn a_blank_registration_code_is_none() -> Result<(), CoreError> {
        let with = pingers_client::Instance::try_from(InstanceConfig {
            registration_code: Some("first-owner".into()),
            ..instance()
        })?;
        assert_eq!(
            with.registration_code
                .as_ref()
                .map(pingers_proto::SecretText::expose),
            Some("first-owner")
        );
        for blank in [None, Some(String::new()), Some(" ".into())] {
            let without = pingers_client::Instance::try_from(InstanceConfig {
                registration_code: blank,
                ..instance()
            })?;
            assert!(without.registration_code.is_none());
        }
        Ok(())
    }

    #[test]
    fn malformed_pins_or_ca_are_refused() {
        for config in [
            InstanceConfig {
                gateway_key_pins: Some("not-a-pin".into()),
                ..instance()
            },
            InstanceConfig {
                push_relay_key_pins: Some(format!("{TWO_PINS},")),
                ..instance()
            },
            InstanceConfig {
                extra_ca_pem: Some(
                    "-----BEGIN CERTIFICATE-----\n!\n-----END CERTIFICATE-----".into(),
                ),
                ..instance()
            },
        ] {
            assert!(matches!(
                pingers_client::Instance::try_from(config),
                Err(CoreError::Invalid { .. })
            ));
        }
    }

    #[tokio::test]
    async fn a_server_that_is_not_https_is_refused() {
        let app = Arc::new(App::default());
        let bad = InstanceConfig {
            server: "pingers.invalid".into(),
            ..instance()
        };
        let opened = Core::open(db_path("bad"), bad, app.clone(), app).await;
        assert!(matches!(opened, Err(CoreError::Invalid { .. })));
    }

    async fn open(name: &str) -> Result<Arc<Core>, CoreError> {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        open_on(name, &fakes).await
    }

    /// A core over `fakes`, so a test can script them and read the calls.
    async fn open_on(name: &str, fakes: &FakeCrates) -> Result<Arc<Core>, CoreError> {
        let app = Arc::new(App::default());
        app.secure_store_put("pingers.database-key".into(), vec![7; 32])
            .map_err(|_| CoreError::invalid("store"))?;
        let crates = fakes.crates();
        Core::open_with(db_path(name), app.clone(), app, move |_| Ok(crates)).await
    }

    #[tokio::test]
    async fn a_push_that_starts_a_meetup_posts_its_notifications_and_events()
    -> Result<(), CoreError> {
        use pingers_client::ports::{MeetupsSection, PartiesSection};
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let app = Arc::new(App::default());
        app.secure_store_put("pingers.database-key".into(), vec![7; 32])
            .map_err(|_| CoreError::invalid("store"))?;
        let crates = fakes.crates();
        let core = Core::open_with(db_path("notify"), app.clone(), app.clone(), move |_| {
            Ok(crates)
        })
        .await?;
        // What the push leads to: the canvas's Meetup, its chat and Ben's request.
        let fixture =
            pingers_screens::fixtures::snapshot().map_err(|_| CoreError::invalid("fixture"))?;
        fakes.set_parties(PartiesSection {
            parties: fixture.parties.clone(),
            ..PartiesSection::default()
        });
        core.on_dnd_changed().await?;
        assert_eq!(
            app.told(),
            [
                "parties changed",
                "join requested",
                "JoinRequest 0x3323232: Climbing crew / Ben opened your invite. Compare the code \
                 on both phones, then let them in.",
            ]
        );
        fakes.set_meetups(MeetupsSection {
            meetups: fixture.meetups.clone(),
            location_requests: fixture.location_requests.clone(),
            ..MeetupsSection::default()
        });
        core.on_push(vec![1; 8]).await?;
        assert_eq!(
            app.told().get(3..),
            Some(
                [
                    "meetup started".to_owned(),
                    "MeetupStarted 0x1141414: Climbing crew / Ben and Chloé are at Riverside \
                     Park too. Say hi?"
                        .to_owned(),
                    "meetup message".to_owned(),
                    "ChatMessage 0x2141414: Ben · Riverside Park / Coffee after?".to_owned(),
                    "meetup message".to_owned(),
                    "ChatMessage 0x2141414: Chloé · Riverside Park / Yes please".to_owned(),
                    "location request from 3".to_owned(),
                    "LocationRequest 0x41e1e1e: Riverside Park / Ben asks to see where you are."
                        .to_owned(),
                ]
                .as_slice()
            )
        );
        // The Meetup ends on a later wake.
        fakes.set_meetups(MeetupsSection::default());
        core.on_wake().await?;
        assert_eq!(app.told().last().map(String::as_str), Some("meetup ended"));
        Ok(())
    }

    #[tokio::test]
    async fn push_priorities_reach_the_client_and_downgrades_reach_privacy() -> Result<(), CoreError>
    {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let core = open_on("priority", &fakes).await?;
        core.report_push_priority(crate::PushPriority::High, crate::PushPriority::Normal)
            .await?;
        assert!(
            fakes
                .calls()
                .contains(&pingers_testkit::CrateCall::ReportPushPriority(
                    pingers_client::PushPriority::High,
                    pingers_client::PushPriority::Normal,
                ))
        );
        assert_eq!(core.snapshot().privacy().push_downgrade, None);
        fakes.set_server(ServerSection {
            fallbacks: Vec::new(),
            relay: pingers_client::snapshot::RelayStatus::Reachable,
            map_host: None,
            operators: None,
            push_delivery: pingers_client::snapshot::PushDelivery::Downgraded(
                pingers_client::snapshot::PushDowngrade::PriorityLowered,
            ),
        });
        core.on_wake().await?;
        let privacy = core.snapshot().privacy();
        assert_eq!(
            privacy.push_downgrade,
            Some(crate::views::PushDowngrade::PriorityLowered)
        );
        assert!(
            privacy
                .push_delivery
                .is_some_and(|text| text.contains("Keep Pingers running"))
        );
        Ok(())
    }

    const PARTY_HEX: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

    struct Silent;

    impl crate::MapDownloadListener for Silent {
        fn downloaded(&self, _done: u64, _total: u64) {}
        fn preparing(&self, _retry_after_seconds: u64) {}
    }

    #[tokio::test]
    async fn map_calls_reach_the_maps_port_with_the_settings_route() -> Result<(), CoreError> {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let core = open_on("maps", &fakes).await?;
        let settings = crate::MapSettings {
            user_override: Some(" https://mine.example ".into()),
            publisher: Some("http://not-https.example".into()),
            through_relay: true,
        };
        let catalog = core.map_catalog(settings.clone()).await?;
        assert!(catalog.offers.is_empty());
        let mine = pingers_proto::MapHostUrl::new("https://mine.example".to_owned())?;
        assert_eq!(
            fakes.calls().last(),
            Some(&CrateCall::MapCatalog(
                ports::MapHosts {
                    user_override: Some(mine),
                    publisher: None,
                },
                ports::MapRoute::Relay
            ))
        );
        let offer = crate::MapOffer {
            entry: crate::MapEntry::OnDemand {
                id: "oceania/test".into(),
                name: "Test".into(),
                group: None,
            },
            host: crate::MapHostInfo {
                url: "https://mine.example".into(),
                source: crate::MapHostSource::UserOverride,
            },
        };
        let directory = db_path("maps-dir");
        let failed = core
            .download_map(
                crate::MapDownloadRequest {
                    offer,
                    settings: crate::MapSettings {
                        through_relay: false,
                        ..settings
                    },
                    directory: directory.clone(),
                },
                Arc::new(Silent),
            )
            .await;
        assert!(matches!(
            failed,
            Err(CoreError::Map {
                failure: crate::MapFailure::NotOffered,
                ..
            })
        ));
        assert!(matches!(
            fakes.calls().last(),
            Some(CrateCall::DownloadMap(job)) if job.route == ports::MapRoute::Direct
        ));
        assert!(core.installed_maps(directory.clone()).await?.is_empty());
        // A region on disk ("abc" as its pack, an empty index: the standard SHA-256 vectors).
        let dir = std::path::PathBuf::from(&directory);
        let io = |_: std::io::Error| CoreError::invalid("io");
        std::fs::create_dir_all(&dir).map_err(io)?;
        std::fs::write(dir.join("oceania.test.pmtiles"), b"abc").map_err(io)?;
        std::fs::write(dir.join("oceania.test.places.tsv"), b"").map_err(io)?;
        std::fs::write(
            dir.join("oceania.test.json"),
            r#"{"format":1,"id":"oceania/test","name":"Test","bbox":[1,1,2,2],"max_zoom":10,
               "build_date":"2026-11-12",
               "pack":{"file":"oceania.test.pmtiles","size":3,
                       "sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"},
               "index":{"file":"oceania.test.places.tsv","size":0,
                        "sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}}"#,
        )
        .map_err(io)?;
        let installed = core.installed_maps(directory.clone()).await?;
        assert_eq!(
            installed.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["oceania/test"]
        );
        core.delete_map(directory.clone(), "oceania/test".into())
            .await?;
        assert!(matches!(
            core.delete_map(directory, "../x".into()).await,
            Err(CoreError::Map { .. })
        ));
        core.forget_map_host_key().await?;
        assert_eq!(fakes.calls().last(), Some(&CrateCall::ForgetMapHostKey));
        Ok(())
    }

    #[tokio::test]
    async fn servers_are_looked_at_trusted_and_given_codes_through_the_client()
    -> Result<(), CoreError> {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let core = open_on("servers", &fakes).await?;
        let server = pingers_proto::ServerId::new("https://b.example".to_owned())?;
        assert!(matches!(
            core.probe_server("b.example".into()).await,
            Err(CoreError::Server {
                issue: crate::ServerIssue::NotLookedAt,
                ..
            })
        ));
        fakes.set_probe(Some(pingers_client::ServerProbe {
            server: server.clone(),
            operator: Some(pingers_client::snapshot::Operator::new("B")?),
            relay: pingers_client::snapshot::Host::new("relay.b.example")?,
            push_relay: None,
            gateway_key: pingers_proto::KeyConfigHash::from_bytes([0x12; 32]),
            known: false,
        }));
        let look = core.probe_server("b.example".into()).await?;
        assert_eq!(look.server, "https://b.example");
        assert_eq!(look.operator.as_deref(), Some("B"));
        assert!(look.gateway_key.starts_with("1212 1212"));
        assert_eq!(
            fakes.calls().last(),
            Some(&CrateCall::ProbeServer("b.example".into()))
        );

        core.trust_server("https://b.example".into()).await?;
        assert_eq!(
            fakes.calls().last(),
            Some(&CrateCall::TrustServer(server.clone()))
        );
        assert!(matches!(
            core.trust_server("http://b.example".into()).await,
            Err(CoreError::Server {
                issue: crate::ServerIssue::InvalidAddress,
                ..
            })
        ));

        // A Party on B, by host, with the code B asked for: the code goes to B first.
        let on_b = NewPartyDraft {
            name: "Book club".into(),
            my_name: "Sam".into(),
            colour: PartyColour::Blue,
            post_quantum: false,
            other_server: Some(" b.example ".into()),
            registration_code: Some(" friends ".into()),
        };
        core.create_party(on_b.clone()).await?;
        let calls = fakes.calls();
        let last_two = calls.get(calls.len().saturating_sub(2)..);
        assert!(matches!(
            last_two,
            Some([
                CrateCall::SubmitRegistrationCode(Some(to), typed),
                CrateCall::CreateParty(draft),
            ]) if *to == server && typed == "friends" && draft.server.as_ref() == Some(&server)
        ));
        // An empty code is not sent.
        let before = fakes.calls().len();
        core.create_party(NewPartyDraft {
            registration_code: Some("  ".into()),
            ..on_b
        })
        .await?;
        assert_eq!(fakes.calls().len(), before.saturating_add(1));
        Ok(())
    }

    #[tokio::test]
    async fn join_requests_are_decided_and_invites_cancelled_through_the_client()
    -> Result<(), CoreError> {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let core = open_on("invites", &fakes).await?;
        let party = PartyId {
            hex: PARTY_HEX.into(),
        };
        let invite = InviteId {
            hex: "09".repeat(16),
        };
        let mailbox = pingers_proto::MailboxId::from_bytes([9; 16]);
        let party_id = pingers_proto::PartyId::from_bytes([0xa1; 16]);
        core.let_in(party.clone(), invite.clone()).await?;
        assert_eq!(
            fakes.calls().last(),
            Some(&CrateCall::DecideJoin(
                party_id,
                mailbox,
                JoinDecision::Admit
            ))
        );
        core.turn_away(party.clone(), invite.clone()).await?;
        assert_eq!(
            fakes.calls().last(),
            Some(&CrateCall::DecideJoin(
                party_id,
                mailbox,
                JoinDecision::Refuse
            ))
        );
        core.withdraw_join().await?;
        assert_eq!(fakes.calls().last(), Some(&CrateCall::WithdrawJoin));
        core.dismiss_join().await?;
        assert_eq!(fakes.calls().last(), Some(&CrateCall::DismissJoin));
        core.check_joins().await?;
        assert_eq!(fakes.calls().last(), Some(&CrateCall::CheckJoins));
        core.open_invite("https://pingers.test/j#k".into(), "Ben".into())
            .await?;
        assert!(matches!(
            fakes.calls().last(),
            Some(CrateCall::OpenInvite(link, name))
                if link.as_str() == "https://pingers.test/j#k" && name.as_str() == "Ben"
        ));
        core.cancel_invite(party.clone(), invite).await?;
        assert_eq!(
            fakes.calls().last(),
            Some(&CrateCall::CancelInvite(party_id, mailbox))
        );
        let short = InviteId { hex: "09".into() };
        assert!(matches!(
            core.cancel_invite(party, short).await,
            Err(CoreError::Invalid { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn safety_codes_and_registration_codes_reach_the_client() -> Result<(), CoreError> {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let core = open_on("safety", &fakes).await?;
        let party = PartyId {
            hex: PARTY_HEX.into(),
        };
        let party_id = pingers_proto::PartyId::from_bytes([0xa1; 16]);
        let ana = MemberId { leaf: 0 };
        let digits = core.safety_code(party.clone(), ana).await?;
        assert_eq!(digits, FakeCrates::SAFETY_CODE);
        core.mark_checked(party, ana).await?;
        assert_eq!(
            fakes.calls().last(),
            Some(&CrateCall::MarkChecked(
                party_id,
                pingers_proto::LeafIndex::new(0)
            ))
        );
        core.create_party(NewPartyDraft {
            name: "Climbing crew".into(),
            my_name: "Sam".into(),
            colour: PartyColour::Blue,
            post_quantum: false,
            other_server: None,
            registration_code: Some(" spring-otter-4471 ".into()),
        })
        .await?;
        let calls = fakes.calls();
        let last_two = calls.get(calls.len().saturating_sub(2)..);
        assert!(matches!(
            last_two,
            Some([CrateCall::SubmitRegistrationCode(None, typed), CrateCall::CreateParty(_)])
                if typed == "spring-otter-4471"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn the_display_name_is_trimmed_stored_and_shown() -> Result<(), CoreError> {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let core = open_on("name", &fakes).await?;
        assert_eq!(core.snapshot().your_name().name, None);
        core.set_display_name("  Sam ".into()).await?;
        assert_eq!(core.snapshot().your_name().name.as_deref(), Some("Sam"));
        let calls = fakes.calls();
        let Some(CrateCall::SetDisplayName(name)) = calls.last() else {
            return Err(CoreError::invalid("no call"));
        };
        assert_eq!(name.as_str(), "Sam");
        assert!(matches!(
            core.set_display_name("   ".into()).await,
            Err(CoreError::Invalid { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn a_reachability_check_answers_only_the_outcome() -> Result<(), CoreError> {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let core = open_on("reach", &fakes).await?;
        fakes.set_server(ServerSection {
            fallbacks: Vec::new(),
            relay: pingers_client::snapshot::RelayStatus::KeyMismatch,
            map_host: Some(
                pingers_proto::MapHostUrl::new("https://maps.pingers.invalid".into())
                    .map_err(|_| CoreError::invalid("map host"))?,
            ),
            operators: Some(pingers_client::snapshot::ServerOperators {
                server: pingers_client::snapshot::Operator::new("Flat 7")
                    .map_err(|_| CoreError::invalid("operator"))?,
                push_relay: Some(
                    pingers_client::snapshot::Operator::new("Push Co")
                        .map_err(|_| CoreError::invalid("operator"))?,
                ),
                relay_host: None,
                push_relay_host: None,
            }),
            push_delivery: pingers_client::snapshot::PushDelivery::Full,
        });
        let check = core.check_reachability().await?;
        assert_eq!(check.status, RelayStatus::KeyMismatch);
        // The server's map host reaches the app's map host chain through the snapshot.
        assert_eq!(
            core.snapshot().server_map_host().as_deref(),
            Some("https://maps.pingers.invalid")
        );
        assert_eq!(core.snapshot().server_operator().as_deref(), Some("Flat 7"));
        assert_eq!(
            core.snapshot().push_relay_operator().as_deref(),
            Some("Push Co")
        );
        assert!(
            check
                .text
                .starts_with("The privacy relay's key doesn't match")
        );
        assert_eq!(fakes.calls().last(), Some(&CrateCall::CheckReachability));
        Ok(())
    }

    #[tokio::test]
    async fn quiet_hours_and_zone_times_reach_the_client_checked() -> Result<(), CoreError> {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let core = open_on("times", &fakes).await?;
        let time = |hour, minute| TimeOfDay { hour, minute };
        core.set_quiet_hours(QuietHoursDraft {
            on: true,
            from: time(22, 30),
            to: time(6, 45),
        })
        .await?;
        let calls = fakes.calls();
        let Some(CrateCall::ChangeSetting(ports::SettingChange::QuietHours(set))) = calls.last()
        else {
            return Err(CoreError::invalid("no quiet hours"));
        };
        assert_eq!(set.state, pingers_client::snapshot::Toggle::On);
        assert_eq!(
            (set.window.start().get(), set.window.end().get()),
            (1_350, 405)
        );
        let empty = core
            .set_quiet_hours(QuietHoursDraft {
                on: false,
                from: time(7, 0),
                to: time(7, 0),
            })
            .await;
        let Err(CoreError::Invalid { reason }) = empty else {
            return Err(CoreError::invalid("an empty window was taken"));
        };
        assert_eq!(reason, "The start and end times must differ.");
        assert!(matches!(
            core.set_quiet_hours(QuietHoursDraft {
                on: true,
                from: time(24, 0),
                to: time(7, 0),
            })
            .await,
            Err(CoreError::Invalid { .. })
        ));

        let mut pipeline = pingers_client::ports::WakePort::section(&fakes)
            .await
            .map_err(CoreError::from)?;
        let fixture =
            pingers_screens::fixtures::snapshot().map_err(|_| CoreError::invalid("fixture"))?;
        pipeline.zones.clone_from(&fixture.zones);
        fakes.set_pipeline(pipeline);
        core.on_wake().await?;
        let weekends = ZoneTimes::Window {
            days: ZoneDays::Weekends,
            from: time(9, 0),
            to: time(17, 0),
        };
        core.set_zone_times(ZoneId { row: 2 }, weekends).await?;
        let calls = fakes.calls();
        let Some(CrateCall::ChangeSetting(ports::SettingChange::Zones(zones))) = calls.last()
        else {
            return Err(CoreError::invalid("no zones"));
        };
        assert_eq!(zones.first(), fixture.zones.first());
        let times = zones
            .get(1)
            .map(|zone| ZoneTimes::from(zone.schedule.times));
        assert_eq!(times, Some(weekends));
        assert!(matches!(
            core.set_zone_times(ZoneId { row: 9 }, ZoneTimes::Always)
                .await,
            Err(CoreError::NotFound { .. })
        ));
        Ok(())
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
                other_server: None,
                registration_code: None,
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
    async fn every_wake_entry_point_reaches_the_wake_crate() -> Result<(), CoreError> {
        let fakes = FakeCrates::new().map_err(|_| CoreError::invalid("fakes"))?;
        let core = open_on("wakes", &fakes).await?;
        let wakes = || {
            fakes
                .calls()
                .into_iter()
                .filter(|call| matches!(call, pingers_testkit::CrateCall::Wake(_)))
                .count()
        };
        core.on_location(crate::Fix {
            coordinate: crate::Coordinate {
                latitude: 51.5,
                longitude: -0.12,
            },
            accuracy_m: 10.0,
            taken_at: 1_800_000_000,
        })
        .await?;
        assert_eq!(wakes(), 1);
        core.on_os_geofence("spot".into(), crate::GeofenceEvent::Enter)
            .await?;
        assert_eq!(wakes(), 2);
        core.on_foreground(true).await?;
        assert_eq!(wakes(), 3);
        core.on_dnd_changed().await?;
        assert_eq!(wakes(), 4);
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
            other_server: None,
            registration_code: None,
        };
        let elsewhere = NewPartyDraft {
            name: "Crew".into(),
            other_server: Some("pingers.flat7.org/path".into()),
            ..empty.clone()
        };
        assert!(matches!(
            core.create_party(elsewhere).await,
            Err(CoreError::Server {
                issue: crate::ServerIssue::InvalidAddress,
                ..
            })
        ));
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
    async fn with_a_database_key_the_core_opens_on_the_real_crates() -> Result<(), CoreError> {
        let app = Arc::new(App::default());
        app.secure_store_put("pingers.database-key".into(), vec![7; 32])
            .map_err(|_| CoreError::invalid("store"))?;
        let core = Core::open(db_path("real"), instance(), app.clone(), app).await?;
        assert!(core.snapshot().home().parties.is_empty(), "a fresh store");
        // The relay is on a reserved name that never resolves: the check runs and says so.
        assert_eq!(
            core.check_reachability().await?.status,
            RelayStatus::Unreachable
        );
        let party = PartyId {
            hex: PARTY_HEX.into(),
        };
        let invite = InviteId {
            hex: "09".repeat(16),
        };
        // Nobody asked to join, so there is nothing to let in.
        assert!(matches!(
            core.let_in(party, invite).await,
            Err(CoreError::Network { .. })
        ));
        core.set_display_name("Sam".into()).await?;
        assert_eq!(core.snapshot().your_name().name.as_deref(), Some("Sam"));
        Ok(())
    }

    #[tokio::test]
    async fn without_a_database_key_the_core_does_not_open() {
        let app = Arc::new(App::default());
        let opened = Core::open(db_path("nokey"), instance(), app.clone(), app).await;
        assert!(matches!(opened, Err(CoreError::Storage { .. })));
    }
}
