// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]: the one async API. Every method calls one crate, then republishes the snapshot.

use std::path::PathBuf;
use std::sync::Arc;

use pingers_geo::Fix;
use pingers_meetup::{LocationAnswer, MeetPointDraft};
use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, PartyId, PushHandle,
    SpotId,
};
use pingers_store::{Database, DatabaseKey};
use tokio::sync::watch;
use zeroize::Zeroizing;

use crate::ports::{
    AllowListChange, ClientEvent, Crates, Foreground, GeofenceTransition, JoinDecision, PartyDraft,
    SettingChange, SpotDraft, WakeInput, WakeOutcome,
};
use crate::snapshot::{ClientSnapshot, InviteLink, InviteUrl, Name, RelayStatus, WakePushes};
use crate::{ClientError, Events, Instance, Outside, Platform, SecureStoreKey};

/// Where the client keeps its data.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// The SQLCipher database file.
    pub database: PathBuf,
    /// The instance to talk to, or `None` to stay offline.
    pub instance: Option<Instance>,
}

/// The Pingers client: the L3 crates wired to one [`Platform`] and one [`Events`] sink.
pub struct Client {
    platform: Arc<dyn Platform>,
    events: Arc<dyn Events>,
    crates: Crates,
    snapshot: watch::Sender<Arc<ClientSnapshot>>,
}

impl Client {
    /// Opens the store with the database key from the platform's secure store, builds the
    /// real crates over it ([`Crates::real`] with [`Outside::system`]), and publishes the
    /// first snapshot.
    ///
    /// # Errors
    ///
    /// As [`Client::open_with`].
    pub async fn open(
        config: ClientConfig,
        platform: Arc<dyn Platform>,
        events: Arc<dyn Events>,
    ) -> Result<Self, ClientError> {
        let crates_platform = Arc::clone(&platform);
        let instance = config.instance.clone();
        Self::open_with(config, platform, events, move |database| {
            Crates::real(
                database,
                crates_platform,
                Outside::system(),
                instance.as_ref(),
            )
        })
        .await
    }

    /// Opens the store as [`Client::open`] does, but builds the crates over it with `build`
    /// (tests pass testkit's `FakeCrates`, or [`Crates::real`] with a manual clock).
    ///
    /// # Errors
    ///
    /// [`ClientError::Platform`], [`ClientError::NoDatabaseKey`] or
    /// [`ClientError::MalformedDatabaseKey`] for the key; [`ClientError::Store`] if the
    /// database does not open; whatever `build` or a crate's snapshot section returns.
    pub async fn open_with(
        config: ClientConfig,
        platform: Arc<dyn Platform>,
        events: Arc<dyn Events>,
        build: impl FnOnce(Database) -> Result<Crates, ClientError>,
    ) -> Result<Self, ClientError> {
        let key = database_key(platform.as_ref())?;
        let database = tokio::task::spawn_blocking(move || Database::open(&config.database, key))
            .await
            .map_err(|_panicked| ClientError::Internal)??;
        let crates = build(database)?;
        let snapshot = Arc::new(assemble(&crates).await?);
        Ok(Self {
            platform,
            events,
            crates,
            snapshot: watch::channel(snapshot).0,
        })
    }

    /// The latest published snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Arc<ClientSnapshot> {
        self.snapshot.borrow().clone()
    }

    /// A receiver that sees every snapshot published from now on.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Arc<ClientSnapshot>> {
        self.snapshot.subscribe()
    }

    /// Reads every crate's section and publishes a new snapshot.
    ///
    /// # Errors
    ///
    /// Whatever a crate's section returns; the previous snapshot stays published.
    pub async fn publish_snapshot(&self) -> Result<Arc<ClientSnapshot>, ClientError> {
        let snapshot = Arc::new(assemble(&self.crates).await?);
        self.snapshot.send_replace(snapshot.clone());
        Ok(snapshot)
    }

    /// Republishes after a command that succeeded.
    async fn published<T>(&self, result: Result<T, ClientError>) -> Result<T, ClientError> {
        let value = result?;
        self.publish_snapshot().await?;
        Ok(value)
    }

    // Parties and Spots: pingers-parties.

    /// Creates a Party (New party).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn create_party(&self, draft: PartyDraft) -> Result<PartyId, ClientError> {
        self.published(self.crates.parties.create_party(draft).await)
            .await
    }

    /// Makes an invite link (Invite).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn create_invite(&self, party: PartyId) -> Result<InviteLink, ClientError> {
        self.published(self.crates.parties.create_invite(party).await)
            .await
    }

    /// Opens an invite link; the snapshot's join preview then shows the code to compare.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn open_invite(&self, link: InviteUrl, my_name: Name) -> Result<(), ClientError> {
        self.published(self.crates.parties.open_invite(link, my_name).await)
            .await
    }

    /// Joins the opened invite's Party once the codes matched (Accept invite).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn accept_invite(&self) -> Result<PartyId, ClientError> {
        self.published(self.crates.parties.accept_invite().await)
            .await
    }

    /// Admits or refuses someone who opened this device's invite.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn decide_join(
        &self,
        party: PartyId,
        invite: MailboxId,
        decision: JoinDecision,
    ) -> Result<(), ClientError> {
        let result = self.crates.parties.decide_join(party, invite, decision);
        self.published(result.await).await
    }

    /// Cancels an invite this device made (Invite people's ✕ on a pending invite).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn cancel_invite(
        &self,
        party: PartyId,
        invite: MailboxId,
    ) -> Result<(), ClientError> {
        self.published(self.crates.parties.cancel_invite(party, invite).await)
            .await
    }

    /// Sets the name this device's user goes by in Parties they create or join from now on.
    ///
    /// # Errors
    ///
    /// [`ClientError::Records`] or [`ClientError::Store`] if it cannot be stored.
    pub async fn set_display_name(&self, name: Name) -> Result<(), ClientError> {
        self.published(self.crates.parties.set_display_name(name).await)
            .await
    }

    /// Removes a member from a Party.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn remove_member(
        &self,
        party: PartyId,
        member: LeafIndex,
    ) -> Result<(), ClientError> {
        self.published(self.crates.parties.remove_member(party, member).await)
            .await
    }

    /// Leaves a Party.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn leave_party(&self, party: PartyId) -> Result<(), ClientError> {
        self.published(self.crates.parties.leave_party(party).await)
            .await
    }

    /// Creates or replaces a Spot (Spot editor).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn save_spot(&self, party: PartyId, spot: SpotDraft) -> Result<SpotId, ClientError> {
        self.published(self.crates.parties.save_spot(party, spot).await)
            .await
    }

    /// Deletes a Spot.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn delete_spot(&self, party: PartyId, spot: SpotId) -> Result<(), ClientError> {
        self.published(self.crates.parties.delete_spot(party, spot).await)
            .await
    }

    // Settings: pingers-wake reads the gate's inputs, pingers-sync owns push registration.

    /// Changes a device-wide privacy setting.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn change_setting(&self, change: SettingChange) -> Result<(), ClientError> {
        self.published(self.crates.wake.change_setting(change).await)
            .await
    }

    /// Turns wake-up pushes on or off ("No wake-up pushes").
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] until pingers-sync merges.
    pub async fn set_wake_pushes(&self, setting: WakePushes) -> Result<(), ClientError> {
        let tokens = self.platform.push_tokens();
        self.published(self.crates.sync.set_wake_pushes(setting, tokens).await)
            .await
    }

    /// The shell's push registration changed; re-seal it.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] until pingers-sync merges.
    pub async fn push_tokens_changed(&self) -> Result<(), ClientError> {
        let tokens = self.platform.push_tokens();
        self.published(self.crates.sync.push_tokens_changed(tokens).await)
            .await
    }

    /// Checks whether this device reaches its server through the privacy relay ("What your
    /// server sees"); the snapshot's relay status then shows the answer.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] until pingers-sync merges.
    pub async fn check_reachability(&self) -> Result<RelayStatus, ClientError> {
        self.published(self.crates.sync.check_reachability().await)
            .await
    }

    // Location consent: the allow-list is per Party member (pingers-parties); asks, answers and
    // revokes happen inside a Meetup (pingers-meetup-session).

    /// Adds a member to, or takes them off, the location allow-list.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn set_always_allow(
        &self,
        party: PartyId,
        member: LeafIndex,
        choice: AllowListChange,
    ) -> Result<(), ClientError> {
        let result = self.crates.parties.set_always_allow(party, member, choice);
        self.published(result.await).await
    }

    /// Asks a Meetup member to see their exact location.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn ask_location(
        &self,
        meetup: GroupId,
        peer: LeafIndex,
    ) -> Result<LocationRequestId, ClientError> {
        self.published(self.crates.meetups.ask_location(meetup, peer).await)
            .await
    }

    /// Answers a member's location request.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn answer_location_request(
        &self,
        meetup: GroupId,
        request: LocationRequestId,
        answer: LocationAnswer,
    ) -> Result<(), ClientError> {
        let result = self
            .crates
            .meetups
            .answer_location_request(meetup, request, answer);
        self.published(result.await).await
    }

    /// Stops sharing this device's exact location with a member.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn revoke_location(
        &self,
        meetup: GroupId,
        peer: LeafIndex,
    ) -> Result<(), ClientError> {
        self.published(self.crates.meetups.revoke_location(meetup, peer).await)
            .await
    }

    // Meetups: pingers-meetup-session.

    /// Sends a chat message, or a reply in `parent`'s thread.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn send_chat(
        &self,
        meetup: GroupId,
        body: ChatBody,
        parent: Option<ChatMessageId>,
    ) -> Result<ChatMessageId, ClientError> {
        let result = self.crates.meetups.send_chat(meetup, body, parent);
        self.published(result.await).await
    }

    /// Sets or clears the meet point.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn set_meet_point(
        &self,
        meetup: GroupId,
        point: MeetPointDraft,
    ) -> Result<(), ClientError> {
        self.published(self.crates.meetups.set_meet_point(meetup, point).await)
            .await
    }

    /// Leaves a Meetup.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn leave_meetup(&self, meetup: GroupId) -> Result<(), ClientError> {
        self.published(self.crates.meetups.leave_meetup(meetup).await)
            .await
    }

    // Pipeline inputs from the shell: pingers-wake. The outcome goes back to the shell as is.

    /// A location fix arrived.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_location(&self, fix: Fix) -> Result<(), ClientError> {
        self.wake(WakeInput::Fix(fix)).await
    }

    /// An OS geofence reported a transition.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_os_geofence(
        &self,
        region: String,
        transition: GeofenceTransition,
    ) -> Result<(), ClientError> {
        self.wake(WakeInput::Geofence { region, transition }).await
    }

    /// A scheduled background wake fell due.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_wake(&self) -> Result<(), ClientError> {
        self.wake(WakeInput::Scheduled).await
    }

    /// A push woke the device, carrying this handle.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_push(&self, handle: PushHandle) -> Result<(), ClientError> {
        self.wake(WakeInput::Push(handle)).await
    }

    /// The interruption filter changed; it is read fresh from the platform.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_dnd_changed(&self) -> Result<(), ClientError> {
        let focus = self.platform.focus_state();
        self.wake(WakeInput::FocusChanged(focus)).await
    }

    /// The app came on screen or left it.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_foreground(&self, foreground: Foreground) -> Result<(), ClientError> {
        self.wake(WakeInput::Foreground(foreground)).await
    }

    async fn wake(&self, input: WakeInput) -> Result<(), ClientError> {
        let outcome = self.crates.wake.run(input).await?;
        self.apply(outcome);
        self.publish_snapshot().await.map(drop)
    }

    fn apply(&self, outcome: WakeOutcome) {
        if let Some(regions) = outcome.geofences {
            self.platform.set_os_geofences(&regions);
        }
        if let Some(mode) = outcome.location_mode {
            self.platform.set_location_mode(mode);
        }
        if let Some(fix) = outcome.fix {
            self.platform.request_fix(fix.quality, fix.timeout);
        }
        if let Some(after) = outcome.next_wake {
            self.platform.schedule_wake(after);
        }
        for event in outcome.events {
            self.emit(event);
        }
    }

    fn emit(&self, event: ClientEvent) {
        let events = self.events.as_ref();
        match event {
            ClientEvent::PartiesChanged => events.parties_changed(),
            ClientEvent::JoinRequested(party) => events.join_requested(party),
            ClientEvent::MeetupStarted(meetup) => events.meetup_started(meetup),
            ClientEvent::MeetupUpdated(meetup) => events.meetup_updated(meetup),
            ClientEvent::MeetupEnded(meetup) => events.meetup_ended(meetup),
            ClientEvent::Notice(notice) => events.notice(notice),
            ClientEvent::PrivacyStatusChanged => events.privacy_status_changed(),
        }
    }
}

/// The SQLCipher key from the secure store.
fn database_key(platform: &dyn Platform) -> Result<DatabaseKey, ClientError> {
    let stored = platform
        .secure_store_get(&SecureStoreKey::database())?
        .ok_or(ClientError::NoDatabaseKey)?;
    let mut bytes = Zeroizing::new([0_u8; DatabaseKey::LENGTH]);
    if stored.len() != bytes.len() {
        return Err(ClientError::MalformedDatabaseKey);
    }
    bytes.copy_from_slice(&stored);
    Ok(DatabaseKey::from_bytes(bytes))
}

/// One snapshot from the four crates' sections.
async fn assemble(crates: &Crates) -> Result<ClientSnapshot, ClientError> {
    let parties = crates.parties.section().await?;
    let pipeline = crates.wake.section().await?;
    let server = crates.sync.section().await?;
    let meetups = crates.meetups.section().await?;
    Ok(ClientSnapshot {
        settings: pipeline.settings,
        gate: pipeline.gate,
        parties: parties.parties,
        zones: pipeline.zones,
        meetups: meetups.meetups,
        alert: meetups.alert,
        location_requests: meetups.location_requests,
        ended_meetup: meetups.ended_meetup,
        join_preview: parties.join_preview,
        display_name: parties.display_name,
        own_position: pipeline.own_position,
        fallbacks: server.fallbacks,
        relay: server.relay,
        map_host: server.map_host,
        operators: server.operators,
    })
}
