// SPDX-License-Identifier: AGPL-3.0-only
//! [`ClientSnapshot`]: the read-only state every screen is computed from.
//!
//! pingers-client publishes the state (architecture §3) and pingers-screens re-exports these
//! types. The shape uses lower-layer domain types plus the few checked values no lower crate
//! defines for display ([`Name`], [`Host`], [`Operator`], [`PartyColour`], [`Me`]); the
//! presentation helpers built on them (initials, tints, people rows) stay in pingers-screens. Nothing here holds a
//! key, a token or a location history: the own position is the latest fix, in memory only.

use core::fmt;

use pingers_gate::{FocusState, GateDecision, LocationSharing, MinuteOfDay};
use pingers_geo::{Bearing, Coordinate, Fence, Fix};
use pingers_meetup::{LocationVisibility, TargetPosition};
use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, MeetPointLabel,
    PartyId, SpotId,
};
use pingers_time::UnixSeconds;

use crate::Fallback;

/// Why a snapshot value was rejected or a lookup in the snapshot failed. Never carries names,
/// coordinates or identifiers (style guide §9).
///
/// Kept apart from [`ClientError`](crate::ClientError) on purpose: pingers-screens converts
/// this one into its own error, while only pingers-ffi sees the client's. Not
/// `#[non_exhaustive]`, so a new variant breaks pingers-screens' conversion until it is mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    /// The snapshot has no Party with this id.
    #[error("no such party")]
    UnknownParty,
    /// The snapshot has no Meetup with this id.
    #[error("no such meetup")]
    UnknownMeetup,
    /// The Meetup has no member with this leaf.
    #[error("no such meetup member")]
    UnknownMember,
    /// A name was empty, only spaces, or too long.
    #[error("name is empty or too long")]
    InvalidName,
    /// A server or relay host was empty or contained spaces.
    #[error("host is not valid")]
    InvalidHost,
    /// An operator name was empty.
    #[error("operator name is empty")]
    InvalidOperator,
    /// A safety code was not groups of digits.
    #[error("safety code is not valid")]
    InvalidSafetyCode,
    /// An invite link was empty or contained spaces.
    #[error("invite link is not valid")]
    InvalidInviteLink,
    /// A daily window that starts where it ends.
    #[error("window is empty")]
    EmptyWindow,
}

/// A name a person gave something: a Party, a Spot, a member, an exclusion zone. Never empty,
/// trimmed, at most [`Name::MAX_CHARACTERS`]. `Debug` does not print it (style guide §9).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Name(String);

impl Name {
    /// The longest name, in characters.
    pub const MAX_CHARACTERS: usize = 80;

    /// Trims and checks a name.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::InvalidName`] if it is empty after trimming or too long.
    pub fn new(text: &str) -> Result<Self, SnapshotError> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.chars().count() > Self::MAX_CHARACTERS {
            return Err(SnapshotError::InvalidName);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Name(redacted)")
    }
}

/// The five Party colours offered on New party.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PartyColour {
    /// Blue.
    Blue,
    /// Orange.
    Orange,
    /// Ink.
    Ink,
    /// Teal.
    Teal,
    /// Plum.
    Plum,
}

impl PartyColour {
    /// Every colour, in the order New party offers them.
    pub const ALL: [Self; 5] = [Self::Blue, Self::Orange, Self::Ink, Self::Teal, Self::Plum];
}

/// Whether a member is this device's user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Me {
    /// This device.
    Me,
    /// Someone else.
    Other,
}

/// A server or relay host name, lower-cased ("pingers.flat7.org").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Host(String);

impl Host {
    /// Trims, lower-cases and checks a host.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::InvalidHost`] if it is empty or contains spaces.
    pub fn new(text: &str) -> Result<Self, SnapshotError> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
            return Err(SnapshotError::InvalidHost);
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    /// The host.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Who declares to run a server or relay ("Flat 7"). Two operators are the same when their
/// names match ignoring case, which is how coinciding operators are found.
#[derive(Clone, Debug)]
pub struct Operator(String);

impl Operator {
    /// Trims and checks an operator name.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::InvalidOperator`] if it is empty.
    pub fn new(text: &str) -> Result<Self, SnapshotError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(SnapshotError::InvalidOperator);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// The name as declared.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialEq for Operator {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_lowercase() == other.0.to_lowercase()
    }
}

impl Eq for Operator {}

/// Who the server and the instance's push relay declare to run them (`server_operator`, and the
/// listed push relay's `operator` in `/v1/params`), for New party's operator checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerOperators {
    /// The server's operator.
    pub server: Operator,
    /// The push relay's operator, if the server lists the instance's push relay.
    pub push_relay: Option<Operator>,
    /// The first privacy relay the server lists (`ohttp_relays`), which Parties on the server
    /// default to.
    pub relay_host: Option<Host>,
    /// Where the instance's push relay is, if the server lists it.
    pub push_relay_host: Option<Host>,
}

impl PartyConfig {
    /// Fills in who runs what from the server's published parameters. An OHTTP privacy relay
    /// publishes no operator of its own, so its operator stays unknown; the screens count it
    /// as the server's, the cautious reading.
    pub fn declare(&mut self, operators: &ServerOperators) {
        self.server.operator = Some(operators.server.clone());
        if let Some(host) = &operators.relay_host {
            self.relay.host = host.clone();
        }
        self.push_relay.operator.clone_from(&operators.push_relay);
        self.push_relay.host.clone_from(&operators.push_relay_host);
    }
}

/// Everything the screens read, at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientSnapshot {
    /// Device-wide settings.
    pub settings: Settings,
    /// What the gate (pingers-gate) answers now: why sharing is suppressed, if it is.
    pub gate: GateDecision,
    /// The Parties this device is in.
    pub parties: Vec<Party>,
    /// Exclusion zones, stored only on this device.
    pub zones: Vec<ExclusionZone>,
    /// Live Meetups this device is in.
    pub meetups: Vec<Meetup>,
    /// A co-presence alert that arrived while the app was open: the Meetup it is for.
    pub alert: Option<GroupId>,
    /// Requests from Meetup members to see this device's exact location, not yet answered.
    pub location_requests: Vec<LocationRequest>,
    /// The summary of the Meetup that ended last, kept until the user dismisses it. Its keys
    /// and content are already deleted.
    pub ended_meetup: Option<EndedMeetup>,
    /// The join this device started from an invite link: asked and waiting, or just let in.
    pub joining: Option<Joining>,
    /// The name this device's user goes by in Parties they create or join from now on, once
    /// they set one.
    pub display_name: Option<Name>,
    /// The device's latest fix and heading, in memory only.
    pub own_position: Option<OwnPosition>,
    /// Runtime fallbacks in effect (design §0).
    pub fallbacks: Vec<Fallback>,
    /// Whether anonymous traffic can reach the server now.
    pub relay: RelayStatus,
    /// The map host the server advertises (`map_host` in `/v1/params`), for the app's map
    /// host chain: after the user's override, before the publisher's default.
    pub map_host: Option<pingers_proto::MapHostUrl>,
    /// Who runs the server and its push relay, once `/v1/params` has been read this process.
    pub operators: Option<ServerOperators>,
    /// Whether wake-up pushes reach this phone (design B.4.9).
    pub push_delivery: PushDelivery,
}

/// Device-wide settings (design C.2 `settings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The app-wide sharing switch.
    pub location_sharing: LocationSharing,
    /// "Pause during Focus".
    pub pause_in_focus: PauseInFocus,
    /// The platform's Focus state, read fresh.
    pub focus: FocusState,
    /// Quiet hours, with the window kept while switched off.
    pub quiet_hours: QuietHoursPreference,
    /// Who sees the exact spot in Meetups.
    pub visibility: LocationVisibility,
    /// Whether wake-up pushes are registered.
    pub wake_pushes: WakePushes,
    /// "Hide trail near zones": stay hidden for five minutes after leaving a zone.
    pub hide_trail: HideTrail,
}

/// "Pause during Focus".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PauseInFocus {
    /// Sharing pauses while a Focus or DND mode is on.
    On,
    /// Focus does not pause sharing.
    Off,
}

/// A switch with no more specific meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Toggle {
    /// On.
    On,
    /// Off.
    Off,
}

/// Quiet hours as the settings screen shows them: the window stays when the switch is off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuietHoursPreference {
    /// Whether quiet hours apply.
    pub state: Toggle,
    /// The daily window.
    pub window: DailyWindow,
}

/// A daily window from a start minute (inside) to an end minute (outside), which may wrap past
/// midnight: pingers-gate's `QuietHours`, which exposes its ends, under the name the screens
/// use for quiet hours and zone times alike. `DailyWindow::new` reports an empty window as
/// `GateError::EmptyQuietHours`; [`daily_window`] reports it in the screens' terms.
pub type DailyWindow = pingers_gate::QuietHours;

/// The daily window from `start` (inside) to `end` (outside), as the screens take it.
///
/// # Errors
///
/// [`SnapshotError::EmptyWindow`] if they are the same minute (pingers-gate's
/// `GateError::EmptyQuietHours`, the only way `QuietHours::new` fails).
pub fn daily_window(start: MinuteOfDay, end: MinuteOfDay) -> Result<DailyWindow, SnapshotError> {
    DailyWindow::new(start, end).map_err(|_empty_quiet_hours| SnapshotError::EmptyWindow)
}

/// "No wake-up pushes (maximum privacy)" (design §0, B.4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WakePushes {
    /// A sealed push token is registered; the relay wakes the phone for knocks.
    On,
    /// No token is registered; knocks are found on the phone's own wakes.
    NoWakePushes,
}

/// Whether wake-up pushes reach this phone as the design intends (design B.4.9, B.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PushDelivery {
    /// A sealed push token is registered and pushes arrive at the priority they were sent with.
    Full,
    /// Knocks and alerts arrive late, or only on the phone's own wakes.
    Downgraded(PushDowngrade),
}

/// Why wake-up pushes are downgraded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PushDowngrade {
    /// "No wake-up pushes (maximum privacy)" is on.
    WakePushesOff,
    /// The platform has no push token: no UnifiedPush distributor, or no FCM registration.
    NoPushToken,
    /// The build names no push relay, or the server does not list it.
    NoPushRelay,
    /// The push relay's key does not match its pins: no token is sealed to it.
    RelayKeyMismatch,
    /// FCM has been delivering high-priority pushes at normal priority (`Notice::PushDowngraded`).
    PriorityLowered,
}

/// "Hide trail near zones".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HideTrail {
    /// Stay hidden for five minutes after leaving a zone.
    On,
    /// Resume as soon as the zone is left.
    Off,
}

/// A Party, decrypted from its MLS group state.
#[derive(Clone, Debug, PartialEq)]
pub struct Party {
    /// The Party.
    pub id: PartyId,
    /// Its name.
    pub name: Name,
    /// Its colour.
    pub colour: PartyColour,
    /// Its members, this device included.
    pub members: Vec<Member>,
    /// Its Spots.
    pub spots: Vec<Spot>,
    /// Who runs what for it.
    pub config: PartyConfig,
    /// The invites this device made, newest first.
    pub invites: Vec<Invite>,
    /// The current invite link to share, if one has been made.
    pub invite_link: Option<InviteLink>,
    /// People who asked to join through this device's invites, oldest first.
    pub join_requests: Vec<JoinRequest>,
}

/// A member of a Party.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Their leaf in the Party group.
    pub leaf: LeafIndex,
    /// Their name.
    pub name: Name,
    /// Whether it is this device.
    pub me: Me,
    /// Whether this device compared safety codes with them.
    pub checked: SafetyCheck,
}

/// Whether this device compared safety codes with a member and they matched (design B.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SafetyCheck {
    /// "Checked".
    Checked,
    /// "Not checked", the default.
    NotChecked,
}

/// A Spot.
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    /// The Spot.
    pub id: SpotId,
    /// Its name.
    pub name: Name,
    /// Its shape.
    pub fence: Fence,
}

/// The services a Party uses and who declares to run them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyConfig {
    /// The Pingers server the Party lives on (its origin).
    pub origin: pingers_proto::ServerId,
    /// The Pingers server.
    pub server: Service,
    /// Whether this device reaches the Party's server through its privacy relay now.
    pub relay_status: RelayStatus,
    /// How this device came to trust the server's keys.
    pub keys: KeyTrust,
    /// How the server is run.
    pub hosting: Hosting,
    /// The OHTTP privacy relay this device uses.
    pub relay: Service,
    /// The push relay.
    pub push_relay: PushRelay,
    /// Whether the Party uses the X-Wing MLS suite.
    pub post_quantum: PostQuantum,
}

/// How this device came to trust a server's keys. Nothing is pinned on first use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyTrust {
    /// The app's built-in server: its keys are pinned in the app.
    BuiltIn,
    /// The invite link carried the keys, from an inviter who already trusted them.
    FromInvite,
    /// The user looked at the keys and trusted them when creating a Party.
    Confirmed,
}

/// A server or relay and its declared operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    /// Where it is.
    pub host: Host,
    /// Who declares to run it, once the server's parameters were read.
    pub operator: Option<Operator>,
}

/// How a Party's server is run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hosting {
    /// Run by a member of the Party, or someone they know.
    SelfHosted,
    /// A server someone else runs for the public.
    Public,
}

/// The push relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRelay {
    /// Which relay.
    pub kind: PushRelayKind,
    /// Who declares to run it, once the server's parameters were read.
    pub operator: Option<Operator>,
    /// Where it is, once the server's parameters were read.
    pub host: Option<Host>,
}

/// Which push relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushRelayKind {
    /// The official app publisher's relay.
    Official,
    /// A custom URL: UnifiedPush or an own build.
    Custom(Host),
}

/// The Party's MLS ciphersuite choice (design B.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PostQuantum {
    /// X-Wing hybrid.
    On,
    /// The classical default.
    Off,
}

/// An invite this device made, and where it got to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    /// The invite's mailbox.
    pub id: MailboxId,
    /// Where it got to.
    pub status: InviteStatus,
}

/// Where an invite got to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InviteStatus {
    /// Shared, not opened yet.
    Pending {
        /// When it was shared.
        shared_at: UnixSeconds,
        /// When it stops working.
        expires_at: UnixSeconds,
    },
    /// They joined after the codes were verified.
    Joined {
        /// Their leaf in the Party group.
        leaf: LeafIndex,
    },
}

/// The current invite link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteLink {
    /// The link, with its secret in the fragment.
    pub url: InviteUrl,
    /// When it stops working.
    pub expires_at: UnixSeconds,
}

/// An invite URL. Its fragment holds the invite secret, so `Debug` does not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct InviteUrl(String);

impl InviteUrl {
    /// Checks a link: not empty, no spaces.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::InvalidInviteLink`].
    pub fn new(text: &str) -> Result<Self, SnapshotError> {
        if text.is_empty() || text.contains(char::is_whitespace) {
            return Err(SnapshotError::InvalidInviteLink);
        }
        Ok(Self(text.to_owned()))
    }

    /// The link, to put in the QR code and the share sheet.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for InviteUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InviteUrl(redacted)")
    }
}

/// The code two people compare when one joins ("4821 0937 5512").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyCode(String);

impl SafetyCode {
    /// Checks a code: groups of ASCII digits separated by single spaces.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::InvalidSafetyCode`].
    pub fn new(text: &str) -> Result<Self, SnapshotError> {
        let valid = !text.is_empty()
            && text
                .split(' ')
                .all(|group| !group.is_empty() && group.bytes().all(|byte| byte.is_ascii_digit()));
        if valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(SnapshotError::InvalidSafetyCode)
        }
    }

    /// The code as both screens show it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A join this device started from an invite link (join flow A). The link says nothing about
/// the Party, so nothing about it shows until the inviter lets this device in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Joining {
    /// Asked to join, waiting for the inviter ("Asked to join").
    Asked {
        /// The code to read to the inviter.
        code: SafetyCode,
        /// When this device asked.
        asked_at: UnixSeconds,
    },
    /// Let in ("You're in"), until the user opens the Party.
    LetIn {
        /// The Party, now in [`ClientSnapshot::parties`].
        party: PartyId,
        /// Who let this device in.
        inviter: Name,
        /// The code that was checked.
        code: SafetyCode,
    },
}

/// Someone who asked to join through this device's invite ("Ben asked to join").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
    /// The request, to let in or turn away.
    pub id: MailboxId,
    /// The name they gave.
    pub name: Name,
    /// The code their phone shows; it must match.
    pub code: SafetyCode,
    /// When this device read the request.
    pub asked_at: UnixSeconds,
    /// When the link they used was shared.
    pub shared_at: UnixSeconds,
}

/// A local identifier for an exclusion zone. Zones never leave the device, so this is a row id,
/// not a wire identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ZoneId(u32);

impl ZoneId {
    /// Wraps a row id from storage.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The row id.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// An exclusion zone (design B.11).
#[derive(Clone, Debug, PartialEq)]
pub struct ExclusionZone {
    /// The zone.
    pub id: ZoneId,
    /// Its name.
    pub name: Name,
    /// Its shape.
    pub fence: Fence,
    /// When and for whom it applies.
    pub schedule: ZoneSchedule,
}

/// When and for whom an exclusion zone applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneSchedule {
    /// When.
    pub times: ZoneTimes,
    /// For which Parties.
    pub parties: ZoneParties,
}

impl ZoneSchedule {
    /// Always, for every Party.
    pub const ALWAYS: Self = Self {
        times: ZoneTimes::Always,
        parties: ZoneParties::All,
    };
}

/// When an exclusion zone applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneTimes {
    /// All the time.
    Always,
    /// On some days, inside a daily window.
    Window {
        /// Which days.
        days: Days,
        /// The window.
        window: DailyWindow,
    },
}

/// Days of the week a zone applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Days {
    /// Every day.
    EveryDay,
    /// Monday to Friday.
    Weekdays,
    /// Saturday and Sunday.
    Weekends,
}

/// Which Parties an exclusion zone hides this device from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZoneParties {
    /// Every Party.
    All,
    /// Only these.
    Only(Vec<PartyId>),
}

/// A live Meetup.
#[derive(Clone, Debug, PartialEq)]
pub struct Meetup {
    /// The Meetup group.
    pub id: GroupId,
    /// The Party it belongs to.
    pub party: PartyId,
    /// The Spot it is at.
    pub spot: SpotId,
    /// When it started.
    pub started_at: UnixSeconds,
    /// This device in the Meetup.
    pub me: MeetupSelf,
    /// Everyone else in it.
    pub others: Vec<MeetupPeer>,
    /// The meet point, if one is set.
    pub meet_point: Option<MeetPointSnapshot>,
    /// The chat, oldest first.
    pub chat: Vec<ChatEntry>,
    /// The members who hold this device's location key.
    pub sharing_with: Vec<LeafIndex>,
}

/// This device in a Meetup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetupSelf {
    /// Its leaf in the Meetup group.
    pub leaf: LeafIndex,
    /// The user's name.
    pub name: Name,
}

/// Another member of a Meetup.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetupPeer {
    /// Their leaf in the Meetup group.
    pub leaf: LeafIndex,
    /// Their name.
    pub name: Name,
    /// Whether their exact location can be seen.
    pub access: PeerAccess,
}

/// Whether a member's exact location can be seen (design B.6.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PeerAccess {
    /// Their location key is held; this is their latest decrypted position.
    Shared(TargetPosition),
    /// Not held: no pin, distance or bearing.
    Private,
    /// Not held, and this device has asked.
    Asked,
}

/// The current meet point.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetPointSnapshot {
    /// Where it is.
    pub place: MeetPlace,
    /// Who set it.
    pub set_by: LeafIndex,
}

/// Where a meet point is.
#[derive(Clone, Debug, PartialEq)]
pub enum MeetPlace {
    /// A fixed place.
    Fixed {
        /// Where.
        position: Coordinate,
        /// What to call it.
        label: Option<MeetPointLabel>,
    },
    /// A member's live position ("Meet at me").
    AtMember(LeafIndex),
}

/// One entry in a Meetup chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatEntry {
    /// A member joined.
    Joined {
        /// Who.
        leaf: LeafIndex,
        /// When.
        at: UnixSeconds,
    },
    /// A message in the main thread, with its replies.
    Message(ChatMessage),
    /// Someone set the meet point.
    MeetPointSet {
        /// Who.
        author: LeafIndex,
        /// The label they gave it, if any.
        label: Option<MeetPointLabel>,
    },
}

/// A chat message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    /// The message.
    pub id: ChatMessageId,
    /// Who wrote it.
    pub author: LeafIndex,
    /// What it says.
    pub body: ChatBody,
    /// When it was sent.
    pub at: UnixSeconds,
    /// Its thread (one level: replies have no replies).
    pub replies: Vec<ChatReply>,
}

/// A reply in a message's thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatReply {
    /// The reply.
    pub id: ChatMessageId,
    /// Who wrote it.
    pub author: LeafIndex,
    /// What it says.
    pub body: ChatBody,
    /// When it was sent.
    pub at: UnixSeconds,
}

/// A member asks to see this device's exact location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocationRequest {
    /// The request.
    pub id: LocationRequestId,
    /// The Meetup it was made in.
    pub meetup: GroupId,
    /// Who asks.
    pub from: LeafIndex,
    /// When it lapses unanswered.
    pub expires_at: UnixSeconds,
}

/// What is kept of a Meetup once it ended, for the "Meetup ended" screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndedMeetup {
    /// The Spot's name.
    pub spot_name: Name,
    /// When it started.
    pub started_at: UnixSeconds,
    /// When it ended.
    pub ended_at: UnixSeconds,
    /// How many people took part, this device included.
    pub people: usize,
    /// How many chat messages were sent.
    pub messages: usize,
}

/// The device's latest fix and compass heading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OwnPosition {
    /// The latest fix.
    pub fix: Fix,
    /// The heading, when the compass sensor runs.
    pub heading: Option<Bearing>,
}

/// Whether anonymous traffic can reach the server now (design B.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RelayStatus {
    /// The privacy relay answered.
    Reachable,
    /// No relay reachable: anonymous requests wait.
    Unreachable,
    /// A relay key did not match its pin: anonymous traffic stopped (`Notice::RelayKeyMismatch`).
    KeyMismatch,
}

impl ClientSnapshot {
    /// The Party with this id.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::UnknownParty`].
    pub fn party(&self, id: PartyId) -> Result<&Party, SnapshotError> {
        self.parties
            .iter()
            .find(|party| party.id == id)
            .ok_or(SnapshotError::UnknownParty)
    }

    /// The Meetup with this id.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::UnknownMeetup`].
    pub fn meetup(&self, id: GroupId) -> Result<&Meetup, SnapshotError> {
        self.meetups
            .iter()
            .find(|meetup| meetup.id == id)
            .ok_or(SnapshotError::UnknownMeetup)
    }

    /// The device's own coordinate, if a fix is held.
    #[must_use]
    pub fn own_coordinate(&self) -> Option<Coordinate> {
        self.own_position.map(|position| position.fix.coordinate)
    }
}

impl Party {
    /// The Spot with this id, if the Party has it.
    #[must_use]
    pub fn spot(&self, id: SpotId) -> Option<&Spot> {
        self.spots.iter().find(|spot| spot.id == id)
    }
}

impl Meetup {
    /// Another member, by leaf.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::UnknownMember`].
    pub fn peer(&self, leaf: LeafIndex) -> Result<&MeetupPeer, SnapshotError> {
        self.others
            .iter()
            .find(|peer| peer.leaf == leaf)
            .ok_or(SnapshotError::UnknownMember)
    }

    /// How many people are in it, this device included.
    #[must_use]
    pub fn head_count(&self) -> usize {
        self.others.len().saturating_add(1)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error(transparent)]
        Snapshot(#[from] SnapshotError),
        #[error(transparent)]
        Geo(#[from] pingers_geo::GeoError),
        #[error(transparent)]
        Gate(#[from] pingers_gate::GateError),
    }

    fn meetup(id: u8, others: &[u32]) -> Result<Meetup, SnapshotError> {
        Ok(Meetup {
            id: GroupId::from_bytes([id; 32]),
            party: PartyId::from_bytes([id; 16]),
            spot: SpotId::from_bytes([id; 16]),
            started_at: UnixSeconds::new(0),
            me: MeetupSelf {
                leaf: LeafIndex::new(0),
                name: Name::new("Ana")?,
            },
            others: others
                .iter()
                .map(|leaf| {
                    Ok(MeetupPeer {
                        leaf: LeafIndex::new(*leaf),
                        name: Name::new("Ben")?,
                        access: PeerAccess::Private,
                    })
                })
                .collect::<Result<_, SnapshotError>>()?,
            meet_point: None,
            chat: Vec::new(),
            sharing_with: Vec::new(),
        })
    }

    fn party(id: u8, spots: &[u8]) -> Result<Party, TestError> {
        let fence = Fence::Circle(pingers_geo::Circle::new(
            Coordinate::new(51.5, -0.1)?,
            pingers_geo::Meters::from_whole(150),
        )?);
        let service = Service {
            host: Host::new("pingers.test")?,
            operator: Some(Operator::new("Flat 7")?),
        };
        Ok(Party {
            id: PartyId::from_bytes([id; 16]),
            name: Name::new("Crew")?,
            colour: PartyColour::Blue,
            members: Vec::new(),
            spots: spots
                .iter()
                .map(|spot| {
                    Ok(Spot {
                        id: SpotId::from_bytes([*spot; 16]),
                        name: Name::new("Hall")?,
                        fence: fence.clone(),
                    })
                })
                .collect::<Result<_, SnapshotError>>()?,
            config: PartyConfig {
                origin: pingers_proto::ServerId::new("https://hall.example".to_owned())
                    .map_err(|_invalid| SnapshotError::InvalidHost)?,
                server: service.clone(),
                relay_status: RelayStatus::Reachable,
                keys: KeyTrust::Confirmed,
                hosting: Hosting::SelfHosted,
                relay: service.clone(),
                push_relay: PushRelay {
                    kind: PushRelayKind::Official,
                    operator: service.operator,
                    host: None,
                },
                post_quantum: PostQuantum::Off,
            },
            invites: Vec::new(),
            invite_link: None,
            join_requests: Vec::new(),
        })
    }

    fn snapshot(parties: Vec<Party>, meetups: Vec<Meetup>) -> Result<ClientSnapshot, TestError> {
        Ok(ClientSnapshot {
            settings: Settings {
                location_sharing: LocationSharing::On,
                pause_in_focus: PauseInFocus::On,
                focus: FocusState::Off,
                quiet_hours: QuietHoursPreference {
                    state: Toggle::Off,
                    window: DailyWindow::new(MinuteOfDay::new(1)?, MinuteOfDay::new(2)?)?,
                },
                visibility: LocationVisibility::AskEachTime,
                wake_pushes: WakePushes::On,
                hide_trail: HideTrail::On,
            },
            gate: GateDecision::Share,
            parties,
            zones: Vec::new(),
            meetups,
            alert: None,
            location_requests: Vec::new(),
            ended_meetup: None,
            joining: None,
            display_name: None,
            own_position: None,
            fallbacks: Vec::new(),
            relay: RelayStatus::Reachable,
            map_host: None,
            operators: None,
            push_delivery: PushDelivery::Full,
        })
    }

    #[test]
    fn declared_operators_leave_the_relays_unknown() -> Result<(), TestError> {
        let mut config = party(1, &[])?.config;
        config.server.operator = None;
        config.relay.operator = None;
        config.push_relay.operator = None;
        let operators = ServerOperators {
            server: Operator::new("Pingers")?,
            push_relay: Some(Operator::new("Pingers")?),
            relay_host: Some(Host::new("relay.pingers.org")?),
            push_relay_host: Some(Host::new("push.pingers.org")?),
        };
        config.declare(&operators);
        assert_eq!(config.server.operator, Some(Operator::new("pingers")?));
        assert_eq!(config.relay.operator, None);
        assert_eq!(config.relay.host.as_str(), "relay.pingers.org");
        assert_eq!(config.push_relay.operator, Some(Operator::new("Pingers")?));
        assert_eq!(
            config.push_relay.host.as_ref().map(Host::as_str),
            Some("push.pingers.org")
        );
        // A server that lists no relay keeps the Party's own relay host.
        let mut unlisted = party(1, &[])?.config;
        unlisted.declare(&ServerOperators {
            relay_host: None,
            push_relay: None,
            push_relay_host: None,
            ..operators
        });
        assert_eq!(unlisted.relay.host.as_str(), "pingers.test");
        assert_eq!(unlisted.push_relay.operator, None);
        Ok(())
    }

    #[test]
    fn lookups_find_the_matching_party_meetup_spot_and_peer() -> Result<(), TestError> {
        let snapshot = snapshot(
            vec![party(1, &[])?, party(2, &[7, 8])?],
            vec![meetup(3, &[])?, meetup(4, &[5, 6])?],
        )?;
        let second = snapshot.party(PartyId::from_bytes([2; 16]))?;
        assert_eq!(second.id, PartyId::from_bytes([2; 16]));
        assert_eq!(
            snapshot
                .party(PartyId::from_bytes([9; 16]))
                .map(|found| found.id),
            Err(SnapshotError::UnknownParty)
        );
        assert_eq!(
            second.spot(SpotId::from_bytes([8; 16])).map(|spot| spot.id),
            Some(SpotId::from_bytes([8; 16]))
        );
        assert_eq!(second.spot(SpotId::from_bytes([9; 16])), None);
        let meetup = snapshot.meetup(GroupId::from_bytes([4; 32]))?;
        assert_eq!(meetup.id, GroupId::from_bytes([4; 32]));
        assert_eq!(
            snapshot
                .meetup(GroupId::from_bytes([9; 32]))
                .map(|found| found.id),
            Err(SnapshotError::UnknownMeetup)
        );
        assert_eq!(meetup.peer(LeafIndex::new(6))?.leaf, LeafIndex::new(6));
        assert_eq!(
            meetup.peer(LeafIndex::new(0)).map(|peer| peer.leaf),
            Err(SnapshotError::UnknownMember)
        );
        Ok(())
    }

    #[test]
    fn head_count_includes_this_device() -> Result<(), SnapshotError> {
        assert_eq!(meetup(1, &[])?.head_count(), 1);
        assert_eq!(meetup(1, &[2, 3])?.head_count(), 3);
        Ok(())
    }

    #[test]
    fn own_coordinate_is_the_latest_fix() -> Result<(), TestError> {
        let mut snapshot = snapshot(Vec::new(), Vec::new())?;
        assert_eq!(snapshot.own_coordinate(), None);
        let coordinate = Coordinate::new(51.5, -0.1)?;
        snapshot.own_position = Some(OwnPosition {
            fix: Fix {
                coordinate,
                accuracy: pingers_geo::Meters::from_whole(10),
                taken_at: UnixSeconds::new(0),
            },
            heading: None,
        });
        assert_eq!(snapshot.own_coordinate(), Some(coordinate));
        Ok(())
    }

    #[test]
    fn a_window_that_starts_where_it_ends_is_empty() -> Result<(), TestError> {
        let eleven = MinuteOfDay::new(23 * 60)?;
        let seven = MinuteOfDay::new(7 * 60)?;
        let night = daily_window(eleven, seven)?;
        assert_eq!((night.start(), night.end()), (eleven, seven));
        assert_eq!(daily_window(seven, seven), Err(SnapshotError::EmptyWindow));
        Ok(())
    }

    #[test]
    fn zone_ids_keep_their_row_id() {
        assert_eq!(ZoneId::new(42).get(), 42);
    }

    #[test]
    fn safety_codes_are_groups_of_digits() -> Result<(), SnapshotError> {
        assert_eq!(SafetyCode::new("4729 1836")?.as_str(), "4729 1836");
        assert!(SafetyCode::new("47291836").is_ok());
        for bad in ["", "4729  1836", "4729 18a6", " 4729", "4729 "] {
            assert_eq!(
                SafetyCode::new(bad),
                Err(SnapshotError::InvalidSafetyCode),
                "{bad:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn names_are_trimmed_and_never_empty_or_too_long() -> Result<(), SnapshotError> {
        assert_eq!(Name::new("  Uni flat ")?.as_str(), "Uni flat");
        assert_eq!(Name::new("   "), Err(SnapshotError::InvalidName));
        assert!(Name::new(&"a".repeat(Name::MAX_CHARACTERS)).is_ok());
        assert_eq!(
            Name::new(&"a".repeat(Name::MAX_CHARACTERS + 1)),
            Err(SnapshotError::InvalidName)
        );
        assert_eq!(format!("{:?}", Name::new("Home")?), "Name(redacted)");
        Ok(())
    }

    #[test]
    fn hosts_and_operators_are_normalised() -> Result<(), SnapshotError> {
        assert_eq!(
            Host::new(" Pingers.Flat7.org ")?.as_str(),
            "pingers.flat7.org"
        );
        assert_eq!(Host::new("a b"), Err(SnapshotError::InvalidHost));
        assert_eq!(Host::new(" "), Err(SnapshotError::InvalidHost));
        assert_eq!(Operator::new("Flat 7")?, Operator::new(" flat 7 ")?);
        assert_ne!(Operator::new("Flat 7")?, Operator::new("Flat 8")?);
        assert_eq!(Operator::new(" Flat 7 ")?.as_str(), "Flat 7");
        assert_eq!(Operator::new(""), Err(SnapshotError::InvalidOperator));
        Ok(())
    }

    #[test]
    fn invite_links_hide_their_secret() -> Result<(), SnapshotError> {
        let link = InviteUrl::new("https://pingers.app/j#secret")?;
        assert_eq!(link.as_str(), "https://pingers.app/j#secret");
        assert_eq!(format!("{link:?}"), "InviteUrl(redacted)");
        assert_eq!(InviteUrl::new(""), Err(SnapshotError::InvalidInviteLink));
        assert_eq!(InviteUrl::new("a b"), Err(SnapshotError::InvalidInviteLink));
        Ok(())
    }
}
