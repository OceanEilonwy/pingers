// SPDX-License-Identifier: AGPL-3.0-only
//! What the client needs from each L3 crate, one trait per crate.
//!
//! These traits are the client's side of each L3 crate's contract: the composition root calls
//! only these, each [`Client`](crate::Client) method calls exactly one of them, and the crates
//! plug in through [`Crates`]: [`Crates::real`] over pingers-parties, pingers-wake and
//! pingers-meetup-session (pingers-sync's parts answer `Unavailable` until it merges), or
//! testkit's `FakeCrates` in tests. Every method is async so an implementation can move
//! SQLCipher work off the runtime (style guide §10).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pingers_gate::{FocusState, GateDecision, LocationSharing, PauseState};
use pingers_geo::{Fence, Fix};
use pingers_meetup::{LocationAnswer, LocationVisibility, MeetPointDraft};
use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, PartyId, PushHandle,
    SpotId,
};

use crate::snapshot::{
    EndedMeetup, ExclusionZone, HideTrail, InviteLink, InviteUrl, Joining, LocationRequest, Meetup,
    Name, OwnPosition, Party, PartyColour, PauseInFocus, PostQuantum, QuietHoursPreference,
    RelayStatus, Settings, WakePushes,
};
use crate::{ClientError, Fallback, FixQuality, LocationMode, Notice, OsRegion, PushTokens};

/// The four L3 crates, built over the opened store.
#[derive(Clone)]
pub struct Crates {
    /// pingers-parties.
    pub parties: Arc<dyn PartiesPort>,
    /// pingers-sync.
    pub sync: Arc<dyn SyncPort>,
    /// pingers-wake.
    pub wake: Arc<dyn WakePort>,
    /// pingers-meetup-session.
    pub meetups: Arc<dyn MeetupsPort>,
}

/// A new Party (New party screen, Advanced).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyDraft {
    /// The Party's name.
    pub name: Name,
    /// The name this device's user goes by in it.
    pub my_name: Name,
    /// Its colour.
    pub colour: PartyColour,
    /// Whether it uses the X-Wing suite.
    pub post_quantum: PostQuantum,
}

/// A Spot to create (no id) or replace (its id).
#[derive(Clone, Debug, PartialEq)]
pub struct SpotDraft {
    /// The Spot being edited, or `None` for a new one.
    pub id: Option<SpotId>,
    /// Its name.
    pub name: Name,
    /// Its shape.
    pub fence: Fence,
}

/// The inviter's answer once the joiner read out their code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JoinDecision {
    /// The codes matched: let them in ("Codes match · Let Ben in").
    Admit,
    /// Turn them away ("Don't let in"); nothing is sent.
    Refuse,
}

/// A change to a Party's location allow-list ("Always allow").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AllowListChange {
    /// Approve this member's requests automatically.
    Add,
    /// Ask again each time.
    Remove,
}

/// A change to a device-wide privacy setting (Privacy settings, Exclusion zones).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SettingChange {
    /// The app-wide sharing switch.
    LocationSharing(LocationSharing),
    /// The manual pause.
    Pause(PauseState),
    /// "Pause during Focus".
    PauseInFocus(PauseInFocus),
    /// Quiet hours.
    QuietHours(QuietHoursPreference),
    /// "Hide trail near zones".
    HideTrail(HideTrail),
    /// Who sees the exact spot in Meetups.
    Visibility(LocationVisibility),
    /// The whole list of exclusion zones.
    Zones(Vec<ExclusionZone>),
}

/// An OS geofence transition, as the shell reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GeofenceTransition {
    /// Entered the region.
    Enter,
    /// Stayed inside for the loitering delay.
    Dwell,
    /// Left the region.
    Exit,
}

/// Whether the app is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Foreground {
    /// On screen.
    Visible,
    /// In the background.
    Hidden,
}

/// One pipeline entry point (design E.2), handed to pingers-wake as is.
#[derive(Clone, Debug, PartialEq)]
pub enum WakeInput {
    /// A location fix.
    Fix(Fix),
    /// An OS geofence transition.
    Geofence {
        /// The region id the client registered.
        region: String,
        /// What happened.
        transition: GeofenceTransition,
    },
    /// A scheduled background wake.
    Scheduled,
    /// A push woke the device.
    Push(PushHandle),
    /// The interruption filter changed; this is its new state, read fresh.
    FocusChanged(FocusState),
    /// The app came on screen or left it.
    Foreground(Foreground),
}

/// A fix the pipeline wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixRequest {
    /// How hard to try.
    pub quality: FixQuality,
    /// How long to wait.
    pub timeout: Duration,
}

/// What one wake asks of the shell and the user. The client passes each part on unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WakeOutcome {
    /// The OS geofences to register, if they changed.
    pub geofences: Option<Vec<OsRegion>>,
    /// The location mode, if it changed.
    pub location_mode: Option<LocationMode>,
    /// A fix to ask for.
    pub fix: Option<FixRequest>,
    /// When to wake next.
    pub next_wake: Option<Duration>,
    /// What to tell the shell.
    pub events: Vec<ClientEvent>,
}

/// One [`Events`](crate::Events) call, as a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientEvent {
    /// [`Events::parties_changed`](crate::Events::parties_changed).
    PartiesChanged,
    /// [`Events::join_requested`](crate::Events::join_requested).
    JoinRequested(PartyId),
    /// [`Events::let_in`](crate::Events::let_in).
    LetIn(PartyId),
    /// [`Events::meetup_started`](crate::Events::meetup_started).
    MeetupStarted(GroupId),
    /// [`Events::meetup_updated`](crate::Events::meetup_updated).
    MeetupUpdated(GroupId),
    /// [`Events::meetup_ended`](crate::Events::meetup_ended).
    MeetupEnded(GroupId),
    /// [`Events::notice`](crate::Events::notice).
    Notice(Notice),
    /// [`Events::privacy_status_changed`](crate::Events::privacy_status_changed).
    PrivacyStatusChanged,
}

/// pingers-parties' part of the snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartiesSection {
    /// The Parties.
    pub parties: Vec<Party>,
    /// The join this device started, if any.
    pub joining: Option<Joining>,
    /// The name this device's user goes by in Parties they create or join, once set.
    pub display_name: Option<Name>,
}

/// pingers-wake's part of the snapshot: the gate's inputs and answer.
#[derive(Clone, Debug, PartialEq)]
pub struct PipelineSection {
    /// Device-wide settings.
    pub settings: Settings,
    /// What the gate answers now.
    pub gate: GateDecision,
    /// Exclusion zones.
    pub zones: Vec<ExclusionZone>,
    /// The latest fix, in memory only.
    pub own_position: Option<OwnPosition>,
}

/// pingers-sync's part of the snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerSection {
    /// Runtime fallbacks in effect.
    pub fallbacks: Vec<Fallback>,
    /// Whether anonymous traffic can reach the server.
    pub relay: RelayStatus,
    /// The map host the server advertises (`map_host` in `/v1/params`), once read.
    pub map_host: Option<pingers_proto::MapHostUrl>,
    /// Who runs the server and its push relay, once read.
    pub operators: Option<crate::snapshot::ServerOperators>,
    /// Whether wake-up pushes reach this phone.
    pub push_delivery: crate::snapshot::PushDelivery,
}

/// pingers-meetup-session's part of the snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeetupsSection {
    /// Live Meetups.
    pub meetups: Vec<Meetup>,
    /// The Meetup a co-presence alert is for.
    pub alert: Option<GroupId>,
    /// Unanswered location requests.
    pub location_requests: Vec<LocationRequest>,
    /// The last ended Meetup's summary.
    pub ended_meetup: Option<EndedMeetup>,
}

/// pingers-parties: Parties, invites, rosters, Spots and per-member consent.
#[async_trait]
pub trait PartiesPort: Send + Sync {
    /// Creates a Party with this device as its only member.
    async fn create_party(&self, draft: PartyDraft) -> Result<PartyId, ClientError>;
    /// Makes an invite link.
    async fn create_invite(&self, party: PartyId) -> Result<InviteLink, ClientError>;
    /// Asks to join through an invite link; the snapshot then shows the code to read out.
    async fn open_invite(&self, link: InviteUrl, my_name: Name) -> Result<(), ClientError>;
    /// Withdraws the join this device asked for.
    async fn withdraw_join(&self) -> Result<(), ClientError>;
    /// Forgets the "You're in" state once the user opened the Party.
    async fn dismiss_join(&self) -> Result<(), ClientError>;
    /// Lets in or turns away someone who asked to join, by their request.
    async fn decide_join(
        &self,
        party: PartyId,
        request: MailboxId,
        decision: JoinDecision,
    ) -> Result<(), ClientError>;
    /// Cancels an invite this device made: it takes no more join requests, and its mailbox is
    /// deleted on the server.
    async fn cancel_invite(&self, party: PartyId, invite: MailboxId) -> Result<(), ClientError>;
    /// Sets the name this device's user goes by in Parties they create or join from now on.
    async fn set_display_name(&self, name: Name) -> Result<(), ClientError>;
    /// Removes a member.
    async fn remove_member(&self, party: PartyId, member: LeafIndex) -> Result<(), ClientError>;
    /// Leaves a Party.
    async fn leave_party(&self, party: PartyId) -> Result<(), ClientError>;
    /// Creates or replaces a Spot.
    async fn save_spot(&self, party: PartyId, spot: SpotDraft) -> Result<SpotId, ClientError>;
    /// Deletes a Spot.
    async fn delete_spot(&self, party: PartyId, spot: SpotId) -> Result<(), ClientError>;
    /// Adds a member to, or takes them off, the location allow-list.
    async fn set_always_allow(
        &self,
        party: PartyId,
        member: LeafIndex,
        choice: AllowListChange,
    ) -> Result<(), ClientError>;
    /// The current Parties.
    async fn section(&self) -> Result<PartiesSection, ClientError>;
}

/// pingers-sync: push registration and the server status.
#[async_trait]
pub trait SyncPort: Send + Sync {
    /// Re-seals the push registration for mailboxes and subscriptions.
    async fn push_tokens_changed(&self, tokens: Option<PushTokens>) -> Result<(), ClientError>;
    /// Turns wake-up pushes on (with the registration) or off.
    async fn set_wake_pushes(
        &self,
        setting: WakePushes,
        tokens: Option<PushTokens>,
    ) -> Result<(), ClientError>;
    /// Records the priority a push was sent with and the one FCM delivered it at; returns
    /// `Notice::PushDowngraded` when downgrades have just become frequent (design B.4.9).
    async fn report_push_priority(
        &self,
        original: pingers_wake::PushPriority,
        delivered: pingers_wake::PushPriority,
    ) -> Result<Option<crate::Notice>, ClientError>;
    /// Checks whether this device reaches its server through the privacy relay, and records
    /// the answer as the section's [`RelayStatus`].
    ///
    /// It must leak nothing new: the check is a request of the anonymous class that the core
    /// already makes (the gateway key config fetched through the relay, checked against its
    /// pin), never sent direct and carrying no account, Party or mailbox. With no relay
    /// configured it answers [`RelayStatus::Unreachable`] without sending anything. The answer
    /// is only the status, with no host, address, timing or server text in it.
    async fn check_reachability(&self) -> Result<RelayStatus, ClientError>;
    /// The server status.
    async fn section(&self) -> Result<ServerSection, ClientError>;
}

/// pingers-wake: the background pipeline and the gate's inputs.
#[async_trait]
pub trait WakePort: Send + Sync {
    /// Runs one wake for this input.
    async fn run(&self, input: WakeInput) -> Result<WakeOutcome, ClientError>;
    /// Changes a privacy setting the gate reads.
    async fn change_setting(&self, change: SettingChange) -> Result<(), ClientError>;
    /// The gate's inputs and answer.
    async fn section(&self) -> Result<PipelineSection, ClientError>;
}

/// pingers-meetup-session: live Meetups.
#[async_trait]
pub trait MeetupsPort: Send + Sync {
    /// Sends a chat message, or a reply to `parent`.
    async fn send_chat(
        &self,
        meetup: GroupId,
        body: ChatBody,
        parent: Option<ChatMessageId>,
    ) -> Result<ChatMessageId, ClientError>;
    /// Sets or clears the meet point.
    async fn set_meet_point(
        &self,
        meetup: GroupId,
        point: MeetPointDraft,
    ) -> Result<(), ClientError>;
    /// Asks a member to see their exact location.
    async fn ask_location(
        &self,
        meetup: GroupId,
        peer: LeafIndex,
    ) -> Result<LocationRequestId, ClientError>;
    /// Answers a member's location request.
    async fn answer_location_request(
        &self,
        meetup: GroupId,
        request: LocationRequestId,
        answer: LocationAnswer,
    ) -> Result<(), ClientError>;
    /// Stops sharing this device's location with a member.
    async fn revoke_location(&self, meetup: GroupId, peer: LeafIndex) -> Result<(), ClientError>;
    /// Leaves a Meetup; its keys and content are deleted.
    async fn leave_meetup(&self, meetup: GroupId) -> Result<(), ClientError>;
    /// The live Meetups.
    async fn section(&self) -> Result<MeetupsSection, ClientError>;
}
