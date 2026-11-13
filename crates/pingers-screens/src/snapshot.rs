// SPDX-License-Identifier: AGPL-3.0-only
//! [`ClientSnapshot`]: the read-only state every screen is computed from.
//!
//! pingers-client publishes the state (architecture §3); this module fixes its shape in terms
//! of lower-layer domain types, so the composition root only fills it in. Nothing here holds a
//! key, a token or a location history: the own position is the latest fix, in memory only.

use pingers_client::Fallback;
use pingers_gate::{FocusState, GateDecision, LocationSharing};
use pingers_geo::{Bearing, Coordinate, Fence, Fix};
use pingers_meetup::{LocationVisibility, TargetPosition};
use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, MeetPointLabel,
    PartyId, SpotId,
};
use pingers_time::UnixSeconds;

use crate::ScreenError;
use crate::common::{Host, Me, Name, Operator, PartyColour, Person};

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
    /// An invite link opened on this device, waiting for the user to compare codes.
    pub join_preview: Option<JoinPreview>,
    /// The device's latest fix and heading, in memory only.
    pub own_position: Option<OwnPosition>,
    /// Runtime fallbacks in effect (design §0).
    pub fallbacks: Vec<Fallback>,
    /// Whether anonymous traffic can reach the server now.
    pub relay: RelayStatus,
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
/// midnight: pingers-gate's `QuietHours`, which now exposes its ends, under the name the
/// screens use for quiet hours and zone times alike. `DailyWindow::new` reports an empty
/// window as `GateError::EmptyQuietHours`.
pub type DailyWindow = pingers_gate::QuietHours;

/// "No wake-up pushes (maximum privacy)" (design §0, B.4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WakePushes {
    /// A sealed push token is registered; the relay wakes the phone for knocks.
    On,
    /// No token is registered; knocks are found on the phone's own wakes.
    NoWakePushes,
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
    /// The Pingers server.
    pub server: Service,
    /// How the server is run.
    pub hosting: Hosting,
    /// The OHTTP privacy relay this device uses.
    pub relay: Service,
    /// The push relay.
    pub push_relay: PushRelay,
    /// Whether the Party uses the X-Wing MLS suite.
    pub post_quantum: PostQuantum,
}

/// A server or relay and its declared operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    /// Where it is.
    pub host: Host,
    /// Who declares to run it.
    pub operator: Operator,
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
    /// Who declares to run it.
    pub operator: Operator,
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
    /// Someone opened the link; the codes must be compared before they join.
    Opened {
        /// The name they gave.
        name: Name,
        /// The code both screens show.
        code: SafetyCode,
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
    /// [`ScreenError::InvalidInviteLink`].
    pub fn new(text: &str) -> Result<Self, ScreenError> {
        if text.is_empty() || text.contains(char::is_whitespace) {
            return Err(ScreenError::InvalidInviteLink);
        }
        Ok(Self(text.to_owned()))
    }

    /// The link, to put in the QR code and the share sheet.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for InviteUrl {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("InviteUrl(redacted)")
    }
}

/// The safety code two people compare when one joins ("4729 1836").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyCode(String);

impl SafetyCode {
    /// Checks a code: groups of ASCII digits separated by single spaces.
    ///
    /// # Errors
    ///
    /// [`ScreenError::InvalidSafetyCode`].
    pub fn new(text: &str) -> Result<Self, ScreenError> {
        let valid = !text.is_empty()
            && text
                .split(' ')
                .all(|group| !group.is_empty() && group.bytes().all(|byte| byte.is_ascii_digit()));
        if valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(ScreenError::InvalidSafetyCode)
        }
    }

    /// The code as both screens show it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What an opened invite link shows before joining.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinPreview {
    /// Who invited.
    pub inviter: Name,
    /// The Party's name.
    pub party_name: Name,
    /// Its colour.
    pub colour: PartyColour,
    /// Its members' names, in roster order.
    pub members: Vec<Name>,
    /// How many Spots it has.
    pub spot_count: usize,
    /// The code to compare with the inviter's screen.
    pub safety_code: SafetyCode,
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
    /// [`ScreenError::UnknownParty`].
    pub fn party(&self, id: PartyId) -> Result<&Party, ScreenError> {
        self.parties
            .iter()
            .find(|party| party.id == id)
            .ok_or(ScreenError::UnknownParty)
    }

    /// The Meetup with this id.
    ///
    /// # Errors
    ///
    /// [`ScreenError::UnknownMeetup`].
    pub fn meetup(&self, id: GroupId) -> Result<&Meetup, ScreenError> {
        self.meetups
            .iter()
            .find(|meetup| meetup.id == id)
            .ok_or(ScreenError::UnknownMeetup)
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
    /// The member with this leaf, this device included, as an avatar shows them.
    ///
    /// # Errors
    ///
    /// [`ScreenError::UnknownMember`].
    pub fn person(&self, leaf: LeafIndex) -> Result<Person, ScreenError> {
        if leaf == self.me.leaf {
            return Ok(Person::new(leaf, &self.me.name, Me::Me));
        }
        self.peer(leaf)
            .map(|peer| Person::new(leaf, &peer.name, Me::Other))
    }

    /// Everyone in the Meetup, this device first.
    #[must_use]
    pub fn people(&self) -> Vec<Person> {
        core::iter::once(Person::new(self.me.leaf, &self.me.name, Me::Me))
            .chain(
                self.others
                    .iter()
                    .map(|peer| Person::new(peer.leaf, &peer.name, Me::Other)),
            )
            .collect()
    }

    /// Another member, by leaf.
    ///
    /// # Errors
    ///
    /// [`ScreenError::UnknownMember`].
    pub fn peer(&self, leaf: LeafIndex) -> Result<&MeetupPeer, ScreenError> {
        self.others
            .iter()
            .find(|peer| peer.leaf == leaf)
            .ok_or(ScreenError::UnknownMember)
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

    #[test]
    fn safety_codes_are_groups_of_digits() -> Result<(), ScreenError> {
        assert_eq!(SafetyCode::new("4729 1836")?.as_str(), "4729 1836");
        assert!(SafetyCode::new("47291836").is_ok());
        for bad in ["", "4729  1836", "4729 18a6", " 4729", "4729 "] {
            assert_eq!(
                SafetyCode::new(bad),
                Err(ScreenError::InvalidSafetyCode),
                "{bad:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn invite_links_hide_their_secret() -> Result<(), ScreenError> {
        let link = InviteUrl::new("https://pingers.app/j#secret")?;
        assert_eq!(link.as_str(), "https://pingers.app/j#secret");
        assert_eq!(format!("{link:?}"), "InviteUrl(redacted)");
        assert_eq!(InviteUrl::new(""), Err(ScreenError::InvalidInviteLink));
        assert_eq!(InviteUrl::new("a b"), Err(ScreenError::InvalidInviteLink));
        Ok(())
    }
}
