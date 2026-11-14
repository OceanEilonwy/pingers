// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-screens' view models as UniFFI records, one section per screen.
//!
//! Each record mirrors one view model field for field; the only changes are the ones Kotlin
//! needs: typed ids ([`crate::ids`]), coordinates as numbers ([`crate::geo`]), counts as `u32`,
//! measures in metres, and the few strings pingers-words writes that a screen shows verbatim
//! (pause reasons, time ranges, zone schedules, the "What your server sees" lists).

use pingers_screens as screens;
use pingers_words::Language;

use crate::geo::{Coordinate, Fence};
use crate::ids::{
    InviteId, LocationRequestId, MeetupId, MemberId, MessageId, PartyId, SpotId, ZoneId,
};

/// The language every word is written in. Only English exists (pingers-words).
pub(crate) const LANGUAGE: Language = Language::English;

/// A count for Kotlin. Counts on screens never come near `u32::MAX`.
fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Declares a fieldless enum mirroring one from a lower crate, with the conversion.
macro_rules! mirror_enum {
    ($(#[$meta:meta])* $name:ident from $core:path { $($(#[$vmeta:meta])* $variant:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl From<$core> for $name {
            fn from(value: $core) -> Self {
                match value {
                    $(<$core>::$variant => Self::$variant),+
                }
            }
        }

        impl From<$name> for $core {
            fn from(value: $name) -> Self {
                match value {
                    $($name::$variant => Self::$variant),+
                }
            }
        }
    };
}

// ------------------------------------------------------------------------------------ common

mirror_enum!(
    /// Avatar background tints.
    Tint from screens::common::Tint {
        /// Peach.
        Peach,
        /// Sky.
        Sky,
        /// Sage.
        Sage,
        /// Butter.
        Butter,
        /// Lilac.
        Lilac,
        /// Mint.
        Mint,
        /// Stone, for this device.
        Stone,
    }
);

mirror_enum!(
    /// A Party's colour.
    PartyColour from screens::common::PartyColour {
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
);

mirror_enum!(
    /// How private a Party's setup is.
    PrivacyLevel from screens::common::PrivacyLevel {
        /// No two services share an operator.
        Full,
        /// Two services share an operator.
        Reduced,
    }
);

mirror_enum!(
    /// A compass point.
    Direction from screens::common::Direction {
        /// North.
        N,
        /// North-east.
        NE,
        /// East.
        E,
        /// South-east.
        SE,
        /// South.
        S,
        /// South-west.
        SW,
        /// West.
        W,
        /// North-west.
        NW,
    }
);

/// Someone as an avatar shows them.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Person {
    /// Their leaf.
    pub id: MemberId,
    /// Their name.
    pub name: String,
    /// One or two letters.
    pub initials: String,
    /// The avatar's tint.
    pub tint: Tint,
    /// Whether it is this device.
    pub is_me: bool,
}

impl From<&screens::common::Person> for Person {
    fn from(person: &screens::common::Person) -> Self {
        Self {
            id: person.leaf.into(),
            name: person.name.as_str().to_owned(),
            initials: person.initials.as_str().to_owned(),
            tint: person.tint.into(),
            is_me: person.me == screens::common::Me::Me,
        }
    }
}

fn people(people: &[screens::common::Person]) -> Vec<Person> {
    people.iter().map(Into::into).collect()
}

fn names(names: &[screens::common::Name]) -> Vec<String> {
    names.iter().map(|name| name.as_str().to_owned()).collect()
}

// -------------------------------------------------------------------------------------- Home

/// Why sharing is suppressed (pingers-gate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum SuppressReason {
    /// The app-wide switch is off.
    LocationSharingOff,
    /// Paused by hand.
    Paused,
    /// A Focus or DND mode is on.
    Focus,
    /// Inside quiet hours.
    QuietHours,
    /// Inside an exclusion zone.
    ExclusionZone,
    /// A reason this version of the app does not know.
    Other,
}

impl From<pingers_gate::SuppressReason> for SuppressReason {
    fn from(reason: pingers_gate::SuppressReason) -> Self {
        match reason {
            pingers_gate::SuppressReason::LocationSharingOff => Self::LocationSharingOff,
            pingers_gate::SuppressReason::Paused => Self::Paused,
            pingers_gate::SuppressReason::Focus => Self::Focus,
            pingers_gate::SuppressReason::QuietHours => Self::QuietHours,
            pingers_gate::SuppressReason::ExclusionZone => Self::ExclusionZone,
            _ => Self::Other,
        }
    }
}

/// Whether this device shares its presence now.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Sharing {
    /// Sharing.
    On,
    /// The switch is off.
    Off,
    /// Switched on but suppressed.
    Paused {
        /// Why.
        reason: SuppressReason,
        /// Why, in words ("Sleep Focus is on").
        because: String,
    },
}

impl From<screens::home::Sharing> for Sharing {
    fn from(sharing: screens::home::Sharing) -> Self {
        match sharing {
            screens::home::Sharing::On => Self::On,
            screens::home::Sharing::Off => Self::Off,
            screens::home::Sharing::Paused(reason) => Self::Paused {
                reason: reason.into(),
                because: pingers_words::settings::paused_because(LANGUAGE, reason),
            },
        }
    }
}

/// The sharing card on Home.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SharingSummary {
    /// Whether sharing is on.
    pub sharing: Sharing,
    /// How many Parties.
    pub party_count: u32,
    /// Whether Focus pauses sharing.
    pub pauses_in_focus: bool,
    /// How many exclusion zones.
    pub exclusion_zones: u32,
}

/// The live Meetup card on Home.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LiveMeetupCard {
    /// The Meetup.
    pub meetup: MeetupId,
    /// Its Party's name.
    pub party_name: String,
    /// Its Spot's name.
    pub spot_name: String,
    /// Who is there, others first.
    pub people: Vec<Person>,
}

/// A Party row on Home.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PartyRow {
    /// The Party.
    pub id: PartyId,
    /// Its name.
    pub name: String,
    /// Its initials.
    pub initials: String,
    /// Its colour.
    pub colour: PartyColour,
    /// How many members.
    pub members: u32,
    /// How many Spots.
    pub spots: u32,
    /// Whether a Meetup of it is live.
    pub live: bool,
}

/// A co-presence alert shown in the app.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoPresenceAlert {
    /// The Meetup.
    pub meetup: MeetupId,
    /// The Party's name.
    pub party_name: String,
    /// Who else is there.
    pub others: Vec<String>,
    /// The Spot's name.
    pub spot_name: String,
}

impl From<screens::home::CoPresenceAlert> for CoPresenceAlert {
    fn from(alert: screens::home::CoPresenceAlert) -> Self {
        Self {
            meetup: alert.meetup.into(),
            party_name: alert.party_name.as_str().to_owned(),
            others: names(&alert.others),
            spot_name: alert.spot_name.as_str().to_owned(),
        }
    }
}

/// Home.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HomeView {
    /// The sharing card.
    pub sharing: SharingSummary,
    /// The live Meetup, if any.
    pub live_meetup: Option<LiveMeetupCard>,
    /// The Parties.
    pub parties: Vec<PartyRow>,
    /// A co-presence alert, if one arrived while the app was open.
    pub alert: Option<CoPresenceAlert>,
}

impl From<screens::home::Home> for HomeView {
    fn from(home: screens::home::Home) -> Self {
        Self {
            sharing: SharingSummary {
                sharing: home.sharing.sharing.into(),
                party_count: count(home.sharing.party_count),
                pauses_in_focus: home.sharing.pause_in_focus == screens::snapshot::PauseInFocus::On,
                exclusion_zones: count(home.sharing.exclusion_zones),
            },
            live_meetup: home.live_meetup.map(|card| LiveMeetupCard {
                meetup: card.meetup.into(),
                party_name: card.party_name.as_str().to_owned(),
                spot_name: card.spot_name.as_str().to_owned(),
                people: people(&card.people),
            }),
            parties: home
                .parties
                .into_iter()
                .map(|row| PartyRow {
                    id: row.id.into(),
                    name: row.name.as_str().to_owned(),
                    initials: row.initials.as_str().to_owned(),
                    colour: row.colour.into(),
                    members: count(row.members),
                    spots: count(row.spots),
                    live: row.live,
                })
                .collect(),
            alert: home.alert.map(Into::into),
        }
    }
}

// ------------------------------------------------------------------- New party and Advanced

mirror_enum!(
    /// Two services that declare the same operator.
    Overlap from screens::party::Overlap {
        /// The server's operator runs the privacy relay.
        ServerRunsRelay,
        /// The server's operator runs the push relay.
        ServerRunsPushRelay,
        /// The privacy relay's operator runs the push relay.
        RelayRunsPushRelay,
    }
);

/// Whether a server or relay answered, and who it says runs it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Reachability {
    /// Not answered yet.
    Checking,
    /// Answered.
    Reachable {
        /// The operator it declares.
        operator: String,
    },
    /// Did not answer.
    Unreachable,
}

impl From<&screens::party::Reachability> for Reachability {
    fn from(reachability: &screens::party::Reachability) -> Self {
        match reachability {
            screens::party::Reachability::Checking => Self::Checking,
            screens::party::Reachability::Reachable(operator) => Self::Reachable {
                operator: operator.as_str().to_owned(),
            },
            screens::party::Reachability::Unreachable => Self::Unreachable,
        }
    }
}

impl TryFrom<&Reachability> for screens::party::Reachability {
    type Error = crate::CoreError;

    fn try_from(reachability: &Reachability) -> Result<Self, crate::CoreError> {
        Ok(match reachability {
            Reachability::Checking => Self::Checking,
            Reachability::Reachable { operator } => {
                Self::Reachable(screens::common::Operator::new(operator)?)
            }
            Reachability::Unreachable => Self::Unreachable,
        })
    }
}

/// A server or relay as typed, and its check.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ServiceDraft {
    /// The host as typed.
    pub host: String,
    /// Whether it answered.
    pub check: Reachability,
}

impl From<&screens::party::ServiceDraft> for ServiceDraft {
    fn from(draft: &screens::party::ServiceDraft) -> Self {
        Self {
            host: draft.host.as_str().to_owned(),
            check: (&draft.check).into(),
        }
    }
}

impl TryFrom<&ServiceDraft> for screens::party::ServiceDraft {
    type Error = crate::CoreError;

    fn try_from(draft: &ServiceDraft) -> Result<Self, crate::CoreError> {
        Ok(Self {
            host: screens::common::DraftText::new(draft.host.clone()),
            check: (&draft.check).try_into()?,
        })
    }
}

/// The push relay chosen.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PushRelayChoice {
    /// The app publisher's relay.
    Official,
    /// A custom URL.
    Custom {
        /// The URL as typed.
        url: String,
    },
}

/// What New party and Advanced have so far.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PartyDraft {
    /// The name as typed.
    pub name: String,
    /// The colour.
    pub colour: PartyColour,
    /// The server.
    pub server: ServiceDraft,
    /// The privacy relay.
    pub relay: ServiceDraft,
    /// The push relay.
    pub push_relay: PushRelayChoice,
    /// The push relay's declared operator, once known.
    pub push_relay_operator: Option<String>,
    /// Whether to use the X-Wing MLS suite.
    pub post_quantum: bool,
}

impl TryFrom<&PartyDraft> for screens::party::PartyDraft {
    type Error = crate::CoreError;

    fn try_from(draft: &PartyDraft) -> Result<Self, crate::CoreError> {
        Ok(Self {
            name: screens::common::DraftText::new(draft.name.clone()),
            colour: draft.colour.into(),
            server: (&draft.server).try_into()?,
            relay: (&draft.relay).try_into()?,
            push_relay: match &draft.push_relay {
                PushRelayChoice::Official => screens::party::PushRelayChoice::Official,
                PushRelayChoice::Custom { url } => screens::party::PushRelayChoice::Custom(
                    screens::common::DraftText::new(url.clone()),
                ),
            },
            push_relay_operator: draft
                .push_relay_operator
                .as_deref()
                .map(screens::common::Operator::new)
                .transpose()?,
            post_quantum: if draft.post_quantum {
                screens::snapshot::PostQuantum::On
            } else {
                screens::snapshot::PostQuantum::Off
            },
        })
    }
}

/// How private a draft Party would be.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DraftPrivacy {
    /// Some operators are not known yet.
    Checking,
    /// Known.
    Known {
        /// The level.
        level: PrivacyLevel,
        /// Which services overlap.
        overlaps: Vec<Overlap>,
    },
}

impl From<screens::party::DraftPrivacy> for DraftPrivacy {
    fn from(privacy: screens::party::DraftPrivacy) -> Self {
        match privacy {
            screens::party::DraftPrivacy::Checking => Self::Checking,
            screens::party::DraftPrivacy::Known { level, overlaps } => Self::Known {
                level: level.into(),
                overlaps: overlaps.into_iter().map(Into::into).collect(),
            },
        }
    }
}

/// New party.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NewPartyView {
    /// The name as typed.
    pub name: String,
    /// The colour.
    pub colour: PartyColour,
    /// The server as typed.
    pub server: String,
    /// How private it would be.
    pub privacy: DraftPrivacy,
    /// Whether Create is enabled.
    pub can_create: bool,
}

impl From<screens::party::NewParty> for NewPartyView {
    fn from(view: screens::party::NewParty) -> Self {
        Self {
            name: view.name.as_str().to_owned(),
            colour: view.colour.into(),
            server: view.server.as_str().to_owned(),
            privacy: view.privacy.into(),
            can_create: view.can_create,
        }
    }
}

/// Advanced.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AdvancedView {
    /// How private it would be.
    pub privacy: DraftPrivacy,
    /// The server.
    pub server: ServiceDraft,
    /// The privacy relay.
    pub relay: ServiceDraft,
    /// The push relay.
    pub push_relay: PushRelayChoice,
    /// Whether X-Wing is on.
    pub post_quantum: bool,
}

impl From<screens::party::Advanced> for AdvancedView {
    fn from(view: screens::party::Advanced) -> Self {
        Self {
            privacy: view.privacy.into(),
            server: (&view.server).into(),
            relay: (&view.relay).into(),
            push_relay: match view.push_relay {
                screens::party::PushRelayChoice::Official => PushRelayChoice::Official,
                screens::party::PushRelayChoice::Custom(url) => PushRelayChoice::Custom {
                    url: url.as_str().to_owned(),
                },
            },
            post_quantum: view.post_quantum == screens::snapshot::PostQuantum::On,
        }
    }
}

// ------------------------------------------------------------------------------------- Party

/// A Spot's size.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Enum)]
pub enum SpotShape {
    /// A circle.
    Circle {
        /// Its radius in metres.
        radius_m: f64,
    },
    /// A drawn area.
    Area {
        /// Its area in square metres.
        square_m: f64,
    },
}

/// A Spot row on a Party.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SpotRow {
    /// The Spot.
    pub id: SpotId,
    /// Its name.
    pub name: String,
    /// Its size.
    pub shape: SpotShape,
    /// Its shape on the map.
    pub fence: Fence,
    /// Whether the live Meetup is at it.
    pub in_meetup: bool,
}

/// A Party.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct PartyView {
    /// The Party.
    pub id: PartyId,
    /// Its name.
    pub name: String,
    /// Its members.
    pub members: Vec<Person>,
    /// Its Spots.
    pub spots: Vec<SpotRow>,
    /// Its server's host.
    pub server: String,
    /// How private its setup is.
    pub privacy: PrivacyLevel,
    /// Its live Meetup, if any.
    pub meetup: Option<MeetupId>,
}

impl From<screens::party::PartyView> for PartyView {
    fn from(view: screens::party::PartyView) -> Self {
        Self {
            id: view.id.into(),
            name: view.name.as_str().to_owned(),
            members: people(&view.members),
            spots: view
                .spots
                .into_iter()
                .map(|row| SpotRow {
                    id: row.id.into(),
                    name: row.name.as_str().to_owned(),
                    shape: match row.shape {
                        screens::party::SpotShape::Circle(radius) => SpotShape::Circle {
                            radius_m: radius.get(),
                        },
                        screens::party::SpotShape::Area(area) => SpotShape::Area {
                            square_m: area.get(),
                        },
                    },
                    fence: (&row.fence).into(),
                    in_meetup: row.in_meetup,
                })
                .collect(),
            server: view.server.as_str().to_owned(),
            privacy: view.privacy.into(),
            meetup: view.meetup.map(Into::into),
        }
    }
}

// ----------------------------------------------------------------------------------- Invites

/// An invite this device made.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Invitation {
    /// Shared, not opened.
    Pending {
        /// The invite.
        id: InviteId,
        /// Hours since it was shared.
        shared_hours_ago: u32,
        /// Hours until it stops working.
        expires_in_hours: u32,
    },
    /// Someone asked to join: compare codes, then open [`JoinRequestView`].
    Opened {
        /// The request.
        id: InviteId,
        /// The name they gave.
        name: String,
        /// The join code.
        code: String,
    },
    /// Joined.
    Joined {
        /// The invite.
        id: InviteId,
        /// Who joined.
        person: Person,
    },
}

/// Invite.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InviteView {
    /// The Party's name.
    pub party_name: String,
    /// The live link, if any.
    pub link: Option<String>,
    /// Hours until the link stops working.
    pub expires_in_hours: Option<u32>,
    /// The invites made.
    pub invitations: Vec<Invitation>,
}

impl From<screens::invite::InviteView> for InviteView {
    fn from(view: screens::invite::InviteView) -> Self {
        Self {
            party_name: view.party_name.as_str().to_owned(),
            link: view.link.map(|link| link.as_str().to_owned()),
            expires_in_hours: view.expires_in.map(screens::clock::Hours::get),
            invitations: view
                .invitations
                .into_iter()
                .map(|invitation| match invitation {
                    screens::invite::Invitation::Pending {
                        id,
                        shared_ago,
                        expires_in,
                    } => Invitation::Pending {
                        id: id.into(),
                        shared_hours_ago: shared_ago.get(),
                        expires_in_hours: expires_in.get(),
                    },
                    screens::invite::Invitation::Opened { id, name, code } => Invitation::Opened {
                        id: id.into(),
                        name: name.as_str().to_owned(),
                        code: code.as_str().to_owned(),
                    },
                    screens::invite::Invitation::Joined { id, person } => Invitation::Joined {
                        id: id.into(),
                        person: (&person).into(),
                    },
                })
                .collect(),
        }
    }
}

/// "Ben asked to join" (canvas `InviteApprove`), with its words from pingers-words.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JoinRequestView {
    /// The request, for `Core::let_in` and `Core::turn_away`.
    pub id: InviteId,
    /// "BE".
    pub avatar: String,
    /// "Ben asked to join".
    pub title: String,
    /// "Climbing crew · from the link you shared 2 h ago".
    pub subtitle: String,
    /// "Ask Ben to read their code. It should be:".
    pub ask: String,
    /// "4821 0937 5512".
    pub code: String,
    /// "A different code means someone else opened your link. Don't let them in."
    pub warning: String,
    /// "When you let Ben in".
    pub when_heading: String,
    /// What letting them in does.
    pub when_body: String,
    /// "Codes match · Let Ben in".
    pub let_in: String,
    /// "Don't let in".
    pub turn_away: String,
}

impl From<screens::invite::JoinRequestView> for JoinRequestView {
    fn from(view: screens::invite::JoinRequestView) -> Self {
        let text = pingers_words::invite::join_request(LANGUAGE, &view);
        Self {
            id: view.id.into(),
            avatar: text.avatar,
            title: text.title,
            subtitle: text.subtitle,
            ask: text.ask,
            code: text.code,
            warning: text.warning,
            when_heading: text.when_heading,
            when_body: text.when_body,
            let_in: text.let_in,
            turn_away: text.turn_away,
        }
    }
}

/// Open invite (canvas `JoinA1`): nothing about the Party, only the name to go by.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct OpenInviteView {
    /// "You've been invited to a Party".
    pub title: String,
    /// "You'll see its name and who's in it once the person who invited you lets you in."
    pub subtitle: String,
    /// "What joining means".
    pub what_joining_means: String,
    /// The points under it.
    pub points: Vec<String>,
    /// "Your name in this Party".
    pub name_label: String,
    /// The name to prefill, or empty.
    pub name: String,
    /// "Ask to join".
    pub ask: String,
    /// "Not now".
    pub not_now: String,
}

impl From<screens::invite::OpenInvite> for OpenInviteView {
    fn from(view: screens::invite::OpenInvite) -> Self {
        let text = pingers_words::invite::open_invite(LANGUAGE, &view);
        Self {
            title: text.title,
            subtitle: text.subtitle,
            what_joining_means: text.what_joining_means,
            points: text.points.into(),
            name_label: text.name_label,
            name: text.name,
            ask: text.ask,
            not_now: text.not_now,
        }
    }
}

/// Asked to join (canvas `JoinA2`): the code to read to the inviter.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AskedToJoinView {
    /// "Asked to join".
    pub title: String,
    /// What happens next.
    pub subtitle: String,
    /// "Read this code to them, in person or on a call".
    pub read_code: String,
    /// "4821 0937 5512".
    pub code: String,
    /// "If their code is different, someone else may have your link. Tell them not to let you
    /// in."
    pub warning: String,
    /// "You can leave this screen. We'll notify you when you're in."
    pub leave_note: String,
    /// "Done".
    pub done: String,
    /// "Withdraw request".
    pub withdraw: String,
}

impl From<screens::invite::AskedToJoin> for AskedToJoinView {
    fn from(view: screens::invite::AskedToJoin) -> Self {
        let text = pingers_words::invite::asked_to_join(LANGUAGE, &view);
        Self {
            title: text.title,
            subtitle: text.subtitle,
            read_code: text.read_code,
            code: text.code,
            warning: text.warning,
            leave_note: text.leave_note,
            done: text.done,
            withdraw: text.withdraw,
        }
    }
}

/// You're in (canvas `JoinA3`): the Party, now that this device was let in.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LetInView {
    /// The Party, to open or leave.
    pub party: PartyId,
    /// Its colour.
    pub colour: PartyColour,
    /// "CC".
    pub party_initials: String,
    /// "Ana let you in".
    pub let_in_by: String,
    /// "Climbing crew".
    pub party_name: String,
    /// The first few other members, as avatars.
    pub preview: Vec<Person>,
    /// "+2", when there are more members than avatars.
    pub more: Option<String>,
    /// "6 members · 3 spots".
    pub counts: String,
    /// "Code 4821 0937 5512 was checked before Ana let you in. …"
    pub checked: String,
    /// "Open Climbing crew".
    pub open: String,
    /// "Leave Party".
    pub leave: String,
    /// The notification to post when `Events::let_in` arrives: "Ana let you in".
    pub notification_title: String,
    /// "Climbing crew".
    pub notification_body: String,
}

impl From<screens::invite::LetIn> for LetInView {
    fn from(view: screens::invite::LetIn) -> Self {
        let text = pingers_words::invite::let_in(LANGUAGE, &view);
        Self {
            party: view.party.into(),
            colour: view.colour.into(),
            party_initials: text.party_initials,
            let_in_by: text.let_in_by,
            party_name: text.party_name,
            preview: people(&view.preview),
            more: text.more,
            counts: text.counts,
            checked: text.checked,
            open: text.open,
            leave: text.leave,
            notification_title: text.notification_title,
            notification_body: text.notification_body,
        }
    }
}

// --------------------------------------------------------------------------------- Spot editor

mirror_enum!(
    /// A Spot's shape kind.
    ShapeKind from screens::spot::ShapeKind {
        /// A circle.
        Circle,
        /// A drawn area.
        Area,
    }
);

/// What the Spot editor has so far.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SpotDraft {
    /// The name as typed.
    pub name: String,
    /// The shape kind.
    pub shape: ShapeKind,
    /// The circle's radius in metres.
    pub radius_m: f64,
    /// The drawn area's vertices, once one is drawn.
    pub area: Option<Vec<Coordinate>>,
}

impl TryFrom<&SpotDraft> for screens::spot::SpotDraft {
    type Error = crate::CoreError;

    fn try_from(draft: &SpotDraft) -> Result<Self, crate::CoreError> {
        Ok(Self {
            name: screens::common::DraftText::new(draft.name.clone()),
            shape: draft.shape.into(),
            radius: pingers_geo::Meters::new(draft.radius_m)?,
            area: draft
                .area
                .as_deref()
                .map(|vertices| crate::geo::coordinates(vertices).map(pingers_geo::Polygon::new))
                .transpose()?
                .transpose()?,
        })
    }
}

/// The Spot editor.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SpotEditorView {
    /// The Party's name.
    pub party_name: String,
    /// The name as typed.
    pub name: String,
    /// The shape kind.
    pub shape: ShapeKind,
    /// The radius in metres, clamped.
    pub radius_m: f64,
    /// The smallest radius (100 m).
    pub min_radius_m: f64,
    /// The largest radius.
    pub max_radius_m: f64,
    /// The drawn area in square metres.
    pub area_square_m: Option<f64>,
    /// Whether Save is enabled.
    pub can_save: bool,
}

impl From<screens::spot::SpotEditor> for SpotEditorView {
    fn from(view: screens::spot::SpotEditor) -> Self {
        Self {
            party_name: view.party_name.as_str().to_owned(),
            name: view.name.as_str().to_owned(),
            shape: view.shape.into(),
            radius_m: view.radius.get(),
            min_radius_m: view.min_radius.get(),
            max_radius_m: view.max_radius.get(),
            area_square_m: view.area.map(pingers_geo::SquareMeters::get),
            can_save: view.can_save,
        }
    }
}

// ----------------------------------------------------------------------------------- Draw area

mirror_enum!(
    /// What is wrong with a drawn area.
    ShapeProblem from screens::draw::ShapeProblem {
        /// Edges cross.
        Crossing,
        /// No area.
        NoArea,
        /// Too large.
        TooLarge,
    }
);

/// What the draw-area editor tells the user to do next.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DrawHint {
    /// Tap the first point.
    FirstPoint,
    /// Keep tapping.
    KeepTapping,
    /// Tap the first point to close.
    CloseShape,
    /// Fix this.
    Fix {
        /// What.
        problem: ShapeProblem,
    },
    /// A point is selected.
    Selected {
        /// Its index.
        index: u32,
    },
    /// Drag to adjust.
    Adjust,
}

/// Draw area.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per control, as pingers-screens' DrawArea has them"
)]
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct DrawAreaView {
    /// The Spot's name.
    pub spot_name: String,
    /// The points.
    pub points: Vec<Coordinate>,
    /// Where a "+" sits on each edge of a closed shape.
    pub midpoints: Vec<Coordinate>,
    /// Whether the shape is closed.
    pub closed: bool,
    /// The selected point.
    pub selected: Option<u32>,
    /// Whether tapping the first point closes the shape.
    pub closable: bool,
    /// Whether Undo is enabled.
    pub can_undo: bool,
    /// Whether Delete is enabled.
    pub can_delete: bool,
    /// The area in square metres, once closed and valid.
    pub area_square_m: Option<f64>,
    /// What is wrong, if anything.
    pub problem: Option<ShapeProblem>,
    /// Whether "Use this area" is enabled.
    pub can_use: bool,
    /// The hint.
    pub hint: DrawHint,
    /// The hint, in words.
    pub hint_text: String,
}

fn index(index: screens::draw::PointIndex) -> u32 {
    count(index.get())
}

impl From<screens::draw::DrawArea> for DrawAreaView {
    fn from(view: screens::draw::DrawArea) -> Self {
        Self {
            spot_name: view.spot_name.as_str().to_owned(),
            points: view.points.iter().copied().map(Into::into).collect(),
            midpoints: view.midpoints.iter().copied().map(Into::into).collect(),
            closed: view.closed,
            selected: view.selected.map(index),
            closable: view.closable,
            can_undo: view.can_undo,
            can_delete: view.can_delete,
            area_square_m: view.area.map(pingers_geo::SquareMeters::get),
            problem: view.problem.map(Into::into),
            can_use: view.can_use,
            hint_text: pingers_words::spot::hint(LANGUAGE, view.hint),
            hint: match view.hint {
                screens::draw::DrawHint::FirstPoint => DrawHint::FirstPoint,
                screens::draw::DrawHint::KeepTapping => DrawHint::KeepTapping,
                screens::draw::DrawHint::CloseShape => DrawHint::CloseShape,
                screens::draw::DrawHint::Fix(problem) => DrawHint::Fix {
                    problem: problem.into(),
                },
                screens::draw::DrawHint::Selected(selected) => DrawHint::Selected {
                    index: index(selected),
                },
                screens::draw::DrawHint::Adjust => DrawHint::Adjust,
            },
        }
    }
}

// ------------------------------------------------------------------------------ Exclusion zones

/// An exclusion zone's size.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Enum)]
pub enum ZoneShape {
    /// A circle.
    Circle {
        /// Its radius in metres.
        radius_m: f64,
    },
    /// A drawn area.
    Area,
}

/// An exclusion zone row.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct ZoneRow {
    /// The zone.
    pub id: ZoneId,
    /// Its name.
    pub name: String,
    /// Its size.
    pub shape: ZoneShape,
    /// Its shape on the map.
    pub fence: Fence,
    /// When and for whom, in words ("150 m circle · all parties").
    pub detail: String,
    /// Where its label sits.
    pub label_at: Coordinate,
    /// When it applies, for an editor to start from.
    pub times: ZoneTimes,
}

/// A time of day on a 24-hour clock, as a picker shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Record)]
pub struct TimeOfDay {
    /// 0 to 23.
    pub hour: u8,
    /// 0 to 59.
    pub minute: u8,
}

impl From<screens::clock::LocalTime> for TimeOfDay {
    fn from(time: screens::clock::LocalTime) -> Self {
        // A LocalTime's hour is below 24 and its minute below 60.
        Self {
            hour: u8::try_from(time.hour()).unwrap_or(0),
            minute: u8::try_from(time.minute()).unwrap_or(0),
        }
    }
}

impl TryFrom<TimeOfDay> for screens::clock::LocalTime {
    type Error = crate::CoreError;

    fn try_from(time: TimeOfDay) -> Result<Self, crate::CoreError> {
        Ok(Self::new(time.hour, time.minute)?)
    }
}

mirror_enum!(
    /// Days of the week a zone applies.
    ZoneDays from screens::snapshot::Days {
        /// Every day.
        EveryDay,
        /// Monday to Friday.
        Weekdays,
        /// Saturday and Sunday.
        Weekends,
    }
);

/// When an exclusion zone applies: shown on its row, and sent back to change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ZoneTimes {
    /// All the time.
    Always,
    /// On these days, from one time to another (past midnight if `to` is earlier).
    Window {
        /// Which days.
        days: ZoneDays,
        /// Where the window starts.
        from: TimeOfDay,
        /// Where it ends.
        to: TimeOfDay,
    },
}

impl From<screens::snapshot::ZoneTimes> for ZoneTimes {
    fn from(times: screens::snapshot::ZoneTimes) -> Self {
        match times {
            screens::snapshot::ZoneTimes::Always => Self::Always,
            screens::snapshot::ZoneTimes::Window { days, window } => Self::Window {
                days: days.into(),
                from: screens::clock::LocalTime::from_minute_of_day(window.start()).into(),
                to: screens::clock::LocalTime::from_minute_of_day(window.end()).into(),
            },
        }
    }
}

impl TryFrom<ZoneTimes> for screens::zones::ZoneTimesDraft {
    type Error = crate::CoreError;

    fn try_from(times: ZoneTimes) -> Result<Self, crate::CoreError> {
        Ok(match times {
            ZoneTimes::Always => Self::Always,
            ZoneTimes::Window { days, from, to } => Self::Window {
                days: days.into(),
                from: from.try_into()?,
                to: to.try_into()?,
            },
        })
    }
}

/// Exclusion zones.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct ExclusionZonesView {
    /// The zones.
    pub zones: Vec<ZoneRow>,
    /// Whether "Hide trail near zones" is on.
    pub hide_trail: bool,
    /// This device, if a fix is held.
    pub me: Option<Coordinate>,
}

impl From<screens::zones::ExclusionZones> for ExclusionZonesView {
    fn from(view: screens::zones::ExclusionZones) -> Self {
        let text = pingers_words::zones::exclusion_zones(LANGUAGE, &view);
        Self {
            zones: view
                .zones
                .iter()
                .zip(text.zones)
                .map(|(zone, text)| ZoneRow {
                    id: zone.id.into(),
                    name: zone.name.as_str().to_owned(),
                    shape: match zone.shape {
                        screens::zones::ZoneShape::Circle(radius) => ZoneShape::Circle {
                            radius_m: radius.get(),
                        },
                        screens::zones::ZoneShape::Area => ZoneShape::Area,
                    },
                    fence: (&zone.fence).into(),
                    detail: text.detail,
                    label_at: zone.label_at.into(),
                    times: zone.times.into(),
                })
                .collect(),
            hide_trail: view.hide_trail == screens::snapshot::HideTrail::On,
            me: view.me.map(Into::into),
        }
    }
}

// ------------------------------------------------------------------------------------ Privacy

mirror_enum!(
    /// Who sees the exact spot in Meetups.
    Visibility from pingers_meetup::LocationVisibility {
        /// Ask each time.
        AskEachTime,
        /// Everyone in the Meetup.
        Everyone,
        /// Nobody.
        Nobody,
    }
);

/// Privacy settings.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per switch on the screen"
)]
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PrivacyView {
    /// Why sharing is paused, if it is.
    pub paused: Option<SuppressReason>,
    /// Why, in words ("Paused · Focus is on").
    pub paused_because: Option<String>,
    /// The app-wide switch.
    pub sharing: bool,
    /// "Pause during Focus".
    pub pause_in_focus: bool,
    /// Whether quiet hours apply.
    pub quiet_hours: bool,
    /// The window, in words ("23:00–07:00").
    pub quiet_hours_range: String,
    /// Where the window starts, for a picker to start from.
    pub quiet_hours_from: TimeOfDay,
    /// Where it ends.
    pub quiet_hours_to: TimeOfDay,
    /// How many exclusion zones.
    pub exclusion_zones: u32,
    /// Who sees the exact spot.
    pub visibility: Visibility,
    /// "No wake-up pushes".
    pub no_wake_pushes: bool,
    /// Why wake-ups reach this phone late or not at all, if they do (design B.4.9).
    pub push_downgrade: Option<PushDowngrade>,
    /// What that means and what to do, for under the delivery row; `None` when wake-ups
    /// arrive as they should or "No wake-up pushes" is on.
    pub push_delivery: Option<String>,
}

/// Why wake-up pushes are downgraded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum PushDowngrade {
    /// "No wake-up pushes" is on.
    WakePushesOff,
    /// No push service: no UnifiedPush distributor, or no FCM registration.
    NoPushToken,
    /// The build names no push relay, or the server does not list it.
    NoPushRelay,
    /// The push relay's key does not match its pins.
    RelayKeyMismatch,
    /// FCM has been delivering high-priority pushes at normal priority.
    PriorityLowered,
}

impl From<screens::snapshot::PushDowngrade> for PushDowngrade {
    fn from(reason: screens::snapshot::PushDowngrade) -> Self {
        match reason {
            screens::snapshot::PushDowngrade::WakePushesOff => Self::WakePushesOff,
            screens::snapshot::PushDowngrade::NoPushToken => Self::NoPushToken,
            screens::snapshot::PushDowngrade::NoPushRelay => Self::NoPushRelay,
            screens::snapshot::PushDowngrade::RelayKeyMismatch => Self::RelayKeyMismatch,
            screens::snapshot::PushDowngrade::PriorityLowered => Self::PriorityLowered,
        }
    }
}

impl From<screens::settings::Privacy> for PrivacyView {
    fn from(view: screens::settings::Privacy) -> Self {
        let text = pingers_words::settings::privacy(LANGUAGE, &view);
        Self {
            paused: view.paused_because.map(Into::into),
            paused_because: text.paused,
            sharing: view.sharing == pingers_gate::LocationSharing::On,
            pause_in_focus: view.pause_in_focus == screens::snapshot::PauseInFocus::On,
            quiet_hours: view.quiet_hours.state == screens::snapshot::Toggle::On,
            quiet_hours_range: text.quiet_hours_range,
            quiet_hours_from: view.quiet_hours.from.into(),
            quiet_hours_to: view.quiet_hours.to.into(),
            exclusion_zones: count(view.exclusion_zones),
            visibility: view.visibility.into(),
            no_wake_pushes: view.wake_pushes == screens::snapshot::WakePushes::NoWakePushes,
            push_downgrade: match view.push_delivery {
                screens::snapshot::PushDelivery::Full => None,
                screens::snapshot::PushDelivery::Downgraded(reason) => Some(reason.into()),
            },
            push_delivery: text.push_delivery,
        }
    }
}

/// The name this device's user goes by in Parties they create or join.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct YourNameView {
    /// The name set, if any.
    pub name: Option<String>,
    /// "Your name".
    pub title: String,
    /// The empty field's hint.
    pub placeholder: String,
    /// Who sees it.
    pub help: String,
}

impl From<screens::settings::YourName> for YourNameView {
    fn from(view: screens::settings::YourName) -> Self {
        let text = pingers_words::settings::your_name(LANGUAGE, &view);
        Self {
            name: text.name,
            title: text.title,
            placeholder: text.placeholder,
            help: text.help,
        }
    }
}

mirror_enum!(
    /// Whether this device reaches its server through the privacy relay.
    RelayStatus from screens::snapshot::RelayStatus {
        /// The server answered through the relay.
        Reachable,
        /// It did not: anonymous requests wait.
        Unreachable,
        /// The relay's key does not match its pin: anonymous traffic stopped.
        KeyMismatch,
    }
);

/// What a reachability check found: the outcome and the sentence to show, nothing more.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RelayCheck {
    /// The outcome.
    pub status: RelayStatus,
    /// It, in words.
    pub text: String,
}

impl From<screens::snapshot::RelayStatus> for RelayCheck {
    fn from(status: screens::snapshot::RelayStatus) -> Self {
        Self {
            status: status.into(),
            text: pingers_words::server::relay_check(LANGUAGE, status),
        }
    }
}

/// What your server sees, with the lists pingers-words writes from the configuration.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ServerSeesView {
    /// The server's host.
    pub server: String,
    /// Whether it is self-hosted.
    pub self_hosted: bool,
    /// How private the setup is.
    pub privacy: PrivacyLevel,
    /// Which services overlap.
    pub overlaps: Vec<Overlap>,
    /// "It can see" lines.
    pub can_see: Vec<String>,
    /// "It never sees" lines.
    pub never_sees: Vec<String>,
    /// Notes about the relays.
    pub notes: Vec<String>,
}

impl From<screens::server::ServerSees> for ServerSeesView {
    fn from(view: screens::server::ServerSees) -> Self {
        let text = pingers_words::server::server_sees(LANGUAGE, &view);
        Self {
            server: view.server.as_str().to_owned(),
            self_hosted: view.hosting == screens::snapshot::Hosting::SelfHosted,
            privacy: view.privacy.into(),
            overlaps: view
                .configuration
                .overlaps
                .iter()
                .copied()
                .map(Into::into)
                .collect(),
            can_see: text.can_see,
            never_sees: text.never_sees,
            notes: text.notes,
        }
    }
}

// ------------------------------------------------------------------------------------- Meetup

/// What a meet point is called.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MeetPointName {
    /// The label given.
    Label {
        /// The label.
        label: String,
    },
    /// A dropped pin with no label.
    Pin,
    /// "At me": a member's live position.
    AtMember {
        /// Who.
        person: Person,
    },
}

impl From<&screens::meetup::MeetPointName> for MeetPointName {
    fn from(name: &screens::meetup::MeetPointName) -> Self {
        match name {
            screens::meetup::MeetPointName::Label(label) => Self::Label {
                label: label.as_str().to_owned(),
            },
            screens::meetup::MeetPointName::Pin => Self::Pin,
            screens::meetup::MeetPointName::AtMember(person) => Self::AtMember {
                person: person.into(),
            },
        }
    }
}

/// The meet point card.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MeetPointCard {
    /// What it is called.
    pub name: MeetPointName,
    /// What it is called, in words.
    pub title: String,
    /// Who set it.
    pub set_by: Person,
    /// How far, once both positions are known.
    pub distance_m: Option<f64>,
    /// Where, once known.
    pub at: Option<Coordinate>,
}

/// A member's exact location as this device sees it.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum PeerLocation {
    /// Shared.
    Shared {
        /// Where.
        at: Coordinate,
        /// How far, once this device has a fix.
        distance_m: Option<f64>,
        /// Which way.
        direction: Option<Direction>,
        /// "Updated 40 s ago", "Updated just now"; `None` when the age is not known.
        updated: Option<String>,
    },
    /// Not shared.
    Private,
    /// Not shared, and asked.
    Asked,
}

/// A Meetup member row.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MeetupMember {
    /// Who.
    pub person: Person,
    /// Where, if shared.
    pub location: PeerLocation,
}

/// A Meetup.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MeetupView {
    /// The Meetup.
    pub id: MeetupId,
    /// The Party's name.
    pub party_name: String,
    /// The Spot's name.
    pub spot_name: String,
    /// How many are here, this device included.
    pub here_count: u32,
    /// The meet point, if set.
    pub meet_point: Option<MeetPointCard>,
    /// Everyone else.
    pub members: Vec<MeetupMember>,
    /// This device, if a fix is held.
    pub me: Option<Coordinate>,
    /// How many chat messages.
    pub chat_count: u32,
    /// Whom this device's exact spot is shared with.
    pub shared_with: Vec<String>,
}

impl From<screens::meetup::MeetupView> for MeetupView {
    fn from(view: screens::meetup::MeetupView) -> Self {
        Self {
            id: view.id.into(),
            party_name: view.party_name.as_str().to_owned(),
            spot_name: view.spot_name.as_str().to_owned(),
            here_count: count(view.here_count),
            meet_point: view.meet_point.map(|card| MeetPointCard {
                title: pingers_words::meetup::meet_point_name(LANGUAGE, &card.name),
                name: (&card.name).into(),
                set_by: (&card.set_by).into(),
                distance_m: card.distance.map(pingers_geo::Meters::get),
                at: card.at.map(Into::into),
            }),
            members: view
                .members
                .iter()
                .map(|member| MeetupMember {
                    person: (&member.person).into(),
                    location: match member.location {
                        screens::meetup::PeerLocation::Shared {
                            at,
                            distance,
                            direction,
                            age,
                        } => PeerLocation::Shared {
                            at: at.into(),
                            distance_m: distance.map(pingers_geo::Meters::get),
                            direction: direction.map(Into::into),
                            updated: age.map(|age| pingers_words::common::updated(LANGUAGE, age)),
                        },
                        screens::meetup::PeerLocation::Private => PeerLocation::Private,
                        screens::meetup::PeerLocation::Asked => PeerLocation::Asked,
                    },
                })
                .collect(),
            me: view.me.map(Into::into),
            chat_count: count(view.chat_count),
            shared_with: names(&view.shared_with),
        }
    }
}

/// Meetup ended.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MeetupEndedView {
    /// The Spot's name.
    pub spot_name: String,
    /// How long it lasted, in minutes.
    pub duration_minutes: u32,
    /// When it started, in words ("18:40").
    pub from: String,
    /// When it ended, in words.
    pub to: String,
    /// How many took part.
    pub people: u32,
    /// How many messages.
    pub messages: u32,
}

impl From<screens::meetup::MeetupEnded> for MeetupEndedView {
    fn from(view: screens::meetup::MeetupEnded) -> Self {
        Self {
            spot_name: view.spot_name.as_str().to_owned(),
            duration_minutes: view.duration.get(),
            from: pingers_words::common::time(LANGUAGE, view.from),
            to: pingers_words::common::time(LANGUAGE, view.to),
            people: count(view.people),
            messages: count(view.messages),
        }
    }
}

// --------------------------------------------------------------------------------------- Chat

/// A chat message.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatMessage {
    /// The message.
    pub id: MessageId,
    /// Who wrote it.
    pub author: Person,
    /// What it says.
    pub body: String,
    /// When, in words ("18:42").
    pub time: String,
    /// Its replies.
    pub replies: Vec<ChatMessage>,
}

impl From<&screens::chat::ChatMessageView> for ChatMessage {
    fn from(message: &screens::chat::ChatMessageView) -> Self {
        Self {
            id: message.id.into(),
            author: (&message.author).into(),
            body: message.body.as_str().to_owned(),
            time: pingers_words::common::time(LANGUAGE, message.at),
            replies: message.replies.iter().map(Into::into).collect(),
        }
    }
}

/// One entry in a chat.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChatItem {
    /// Someone joined.
    Joined {
        /// Who.
        person: Person,
        /// When, in words.
        time: String,
    },
    /// A message.
    Message {
        /// The message.
        message: ChatMessage,
    },
    /// Someone set the meet point.
    MeetPointSet {
        /// Who.
        author: Person,
        /// The label, if any.
        label: Option<String>,
    },
}

/// A Meetup chat.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatView {
    /// The Spot's name.
    pub spot_name: String,
    /// Who is in it, others first.
    pub participants: Vec<Person>,
    /// The entries, oldest first.
    pub items: Vec<ChatItem>,
    /// The message being replied to.
    pub replying_to: Option<ChatMessage>,
}

impl From<screens::chat::ChatView> for ChatView {
    fn from(view: screens::chat::ChatView) -> Self {
        Self {
            spot_name: view.spot_name.as_str().to_owned(),
            participants: people(&view.participants),
            items: view
                .items
                .iter()
                .map(|item| match item {
                    screens::chat::ChatItem::Joined { person, at } => ChatItem::Joined {
                        person: person.into(),
                        time: pingers_words::common::time(LANGUAGE, *at),
                    },
                    screens::chat::ChatItem::Message(message) => ChatItem::Message {
                        message: message.into(),
                    },
                    screens::chat::ChatItem::MeetPointSet { author, label } => {
                        ChatItem::MeetPointSet {
                            author: author.into(),
                            label: label.as_ref().map(|label| label.as_str().to_owned()),
                        }
                    }
                })
                .collect(),
            replying_to: view.replying_to.as_ref().map(Into::into),
        }
    }
}

// --------------------------------------------------------------------------------- Meet point

mirror_enum!(
    /// How the meet point is chosen.
    MeetPointMode from screens::meet_point::MeetPointMode {
        /// A dropped pin.
        Pin,
        /// "At me": this device's live position.
        AtMe,
        /// The middle of everyone who shares.
        Midpoint,
    }
);

/// What the meet point editor has so far.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MeetPointDraft {
    /// The mode.
    pub mode: MeetPointMode,
    /// The label as typed.
    pub label: String,
    /// Where the pin is.
    pub pin: Coordinate,
}

impl TryFrom<&MeetPointDraft> for screens::meet_point::MeetPointDraft {
    type Error = crate::CoreError;

    fn try_from(draft: &MeetPointDraft) -> Result<Self, crate::CoreError> {
        Ok(Self {
            mode: draft.mode.into(),
            label: screens::common::DraftText::new(draft.label.clone()),
            pin: draft.pin.try_into()?,
        })
    }
}

/// A member's distance to the meet point.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MeetPointDistance {
    /// Who.
    pub person: Person,
    /// How far, if known.
    pub distance_m: Option<f64>,
}

/// A member on the meet point map.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MemberOnMap {
    /// Who.
    pub person: Person,
    /// Where.
    pub at: Coordinate,
}

/// Meet point.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MeetPointView {
    /// The mode.
    pub mode: MeetPointMode,
    /// The label as typed.
    pub label: String,
    /// Where it would be, if known.
    pub point: Option<Coordinate>,
    /// Each member's distance.
    pub distances: Vec<MeetPointDistance>,
    /// Members with a known position.
    pub members: Vec<MemberOnMap>,
    /// Whether Share is enabled.
    pub can_share: bool,
}

impl From<screens::meet_point::MeetPointView> for MeetPointView {
    fn from(view: screens::meet_point::MeetPointView) -> Self {
        Self {
            mode: view.mode.into(),
            label: view.label.as_str().to_owned(),
            point: view.point.map(Into::into),
            distances: view
                .distances
                .iter()
                .map(|row| MeetPointDistance {
                    person: (&row.person).into(),
                    distance_m: row.distance.map(pingers_geo::Meters::get),
                })
                .collect(),
            members: view
                .members
                .iter()
                .map(|member| MemberOnMap {
                    person: (&member.person).into(),
                    at: member.at.into(),
                })
                .collect(),
            can_share: view.can_share,
        }
    }
}

// ------------------------------------------------------------------------------------ Compass

/// What the compass points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum CompassSelection {
    /// The meet point.
    MeetPoint,
    /// A member.
    Member {
        /// Who.
        id: MemberId,
    },
}

impl From<screens::compass::CompassSelection> for CompassSelection {
    fn from(selection: screens::compass::CompassSelection) -> Self {
        match selection {
            screens::compass::CompassSelection::MeetPoint => Self::MeetPoint,
            screens::compass::CompassSelection::Member(leaf) => Self::Member { id: leaf.into() },
        }
    }
}

impl From<CompassSelection> for screens::compass::CompassSelection {
    fn from(selection: CompassSelection) -> Self {
        match selection {
            CompassSelection::MeetPoint => Self::MeetPoint,
            CompassSelection::Member { id } => Self::Member(id.into()),
        }
    }
}

/// A compass target chip.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CompassTarget {
    /// The meet point.
    MeetPoint {
        /// What it is called.
        name: MeetPointName,
        /// What it is called, in words.
        label: String,
    },
    /// A member.
    Member {
        /// Who.
        person: Person,
        /// Whether their location is not shared.
        locked: bool,
    },
}

impl From<&screens::compass::CompassTarget> for CompassTarget {
    fn from(target: &screens::compass::CompassTarget) -> Self {
        match target {
            screens::compass::CompassTarget::MeetPoint(name) => Self::MeetPoint {
                label: pingers_words::meetup::meet_point_name(LANGUAGE, name),
                name: name.into(),
            },
            screens::compass::CompassTarget::Member { person, locked } => Self::Member {
                person: person.into(),
                locked: *locked,
            },
        }
    }
}

/// Where the target is from here.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct CompassReading {
    /// Degrees clockwise from true north.
    pub bearing_deg: f64,
    /// Degrees clockwise from the device's heading, when known.
    pub relative_bearing_deg: Option<f64>,
    /// The compass point.
    pub direction: Direction,
    /// How far, in metres.
    pub distance_m: f64,
    /// Minutes on foot.
    pub walk_minutes: u32,
    /// "Updated 40 s ago", "Updated just now", under the distance; `None` for the meet point,
    /// which does not move.
    pub updated: Option<String>,
}

/// The compass.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct CompassView {
    /// The chips.
    pub targets: Vec<CompassTarget>,
    /// Which is selected.
    pub selected: CompassSelection,
    /// The selected target.
    pub target: CompassTarget,
    /// Where it is, once both positions are known.
    pub reading: Option<CompassReading>,
    /// The device's GPS accuracy in metres.
    pub gps_accuracy_m: Option<f64>,
    /// Who sees that this device is heading over.
    pub watchers: Vec<String>,
}

impl From<screens::compass::CompassView> for CompassView {
    fn from(view: screens::compass::CompassView) -> Self {
        Self {
            targets: view.targets.iter().map(Into::into).collect(),
            selected: view.selected.into(),
            target: (&view.target).into(),
            reading: view.reading.map(|reading| CompassReading {
                bearing_deg: reading.bearing.degrees(),
                relative_bearing_deg: reading.relative_bearing.map(pingers_geo::Bearing::degrees),
                direction: reading.direction.into(),
                distance_m: reading.distance.get(),
                walk_minutes: reading.walk.get(),
                updated: reading
                    .age
                    .map(|age| pingers_words::common::updated(LANGUAGE, age)),
            }),
            gps_accuracy_m: view.gps_accuracy.map(pingers_geo::Meters::get),
            watchers: names(&view.watchers),
        }
    }
}

// --------------------------------------------------------------------------- Location request

mirror_enum!(
    /// How long an answer shares for.
    ShareScope from pingers_meetup::GrantScope {
        /// Until the Meetup ends.
        ThisMeetup,
        /// Fifteen minutes.
        FifteenMinutes,
    }
);

/// The answer being put together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AnswerDraft {
    /// How long.
    pub scope: ShareScope,
    /// Whether to ask to see theirs back.
    pub ask_back: bool,
    /// Whether to always allow this member.
    pub always_allow: bool,
}

impl From<AnswerDraft> for screens::location_request::AnswerDraft {
    fn from(draft: AnswerDraft) -> Self {
        Self {
            scope: draft.scope.into(),
            ask_back: if draft.ask_back {
                pingers_meetup::AskBack::AskBack
            } else {
                pingers_meetup::AskBack::DoNotAskBack
            },
            always: if draft.always_allow {
                pingers_meetup::AllowListChoice::AddToAllowList
            } else {
                pingers_meetup::AllowListChoice::JustThisTime
            },
        }
    }
}

impl From<screens::location_request::AnswerDraft> for AnswerDraft {
    fn from(draft: screens::location_request::AnswerDraft) -> Self {
        Self {
            scope: draft.scope.into(),
            ask_back: draft.ask_back == pingers_meetup::AskBack::AskBack,
            always_allow: draft.always == pingers_meetup::AllowListChoice::AddToAllowList,
        }
    }
}

/// A location request.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LocationRequestView {
    /// The request.
    pub id: LocationRequestId,
    /// Who asks.
    pub from: Person,
    /// The Spot's name.
    pub spot_name: String,
    /// The Party's name.
    pub party_name: String,
    /// The answer so far.
    pub answer: AnswerDraft,
    /// When it lapses, in Unix seconds.
    pub expires_at: u64,
}

impl From<screens::location_request::LocationRequestView> for LocationRequestView {
    fn from(view: screens::location_request::LocationRequestView) -> Self {
        Self {
            id: view.id.into(),
            from: (&view.from).into(),
            spot_name: view.spot_name.as_str().to_owned(),
            party_name: view.party_name.as_str().to_owned(),
            answer: view.answer.into(),
            expires_at: view.expires_at.get(),
        }
    }
}
