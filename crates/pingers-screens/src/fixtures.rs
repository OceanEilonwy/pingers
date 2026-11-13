// SPDX-License-Identifier: AGPL-3.0-only
//! The canvas's example, as a [`ClientSnapshot`]: Sam is in three Parties, and a Meetup of the
//! Climbing crew is live at Riverside Park with Ben (sharing his exact spot) and Chloé (not).
//! For tests here and in pingers-words (feature `fixtures`).
#![expect(
    clippy::arithmetic_side_effects,
    reason = "fixture times and leaves are small constants"
)]

use pingers_client::Fallback;
use pingers_gate::{FocusState, GateDecision, GateError, LocationSharing, MinuteOfDay};
use pingers_geo::{Circle, Coordinate, Fence, Fix, GeoError, Meters, Polygon};
use pingers_meetup::{LocationVisibility, TargetPosition};
use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, MeetPointLabel,
    PartyId, ProtoError, SpotId,
};
use pingers_time::UnixSeconds;

use crate::ScreenError;
use crate::clock::{LocalClock, UtcOffset};
use crate::common::{Host, Me, Name, Operator, PartyColour};
use crate::snapshot::{
    ChatEntry, ChatMessage, ChatReply, ClientSnapshot, DailyWindow, Days, EndedMeetup,
    ExclusionZone, HideTrail, Hosting, Invite, InviteLink, InviteStatus, InviteUrl, JoinPreview,
    LocationRequest, MeetPlace, MeetPointSnapshot, Meetup, MeetupPeer, MeetupSelf, Member,
    OwnPosition, Party, PartyConfig, PauseInFocus, PeerAccess, PostQuantum, PushRelay,
    PushRelayKind, QuietHoursPreference, RelayStatus, SafetyCode, Service, Settings, Spot, Toggle,
    WakePushes, ZoneId, ZoneParties, ZoneSchedule, ZoneTimes,
};

/// Why a fixture could not be built (it always can; the constructors are fallible).
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    /// A screens value.
    #[error(transparent)]
    Screen(#[from] ScreenError),
    /// A geometry value.
    #[error(transparent)]
    Geo(#[from] GeoError),
    /// A wire value.
    #[error(transparent)]
    Proto(#[from] ProtoError),
    /// A gate value.
    #[error(transparent)]
    Gate(#[from] GateError),
    /// Something a test expected was not there.
    #[error("missing")]
    Missing,
}

/// 2026-11-13 00:00 UTC.
const MIDNIGHT: u64 = 1_794_528_000;

/// A time on the fixture's day, in UTC.
#[must_use]
pub const fn at(hour: u64, minute: u64) -> UnixSeconds {
    UnixSeconds::new(MIDNIGHT + hour * 3600 + minute * 60)
}

/// The fixture's clock: 19:30 UTC, phone on UTC.
#[must_use]
pub const fn clock() -> LocalClock {
    LocalClock {
        now: at(19, 30),
        offset: UtcOffset::UTC,
    }
}

/// The Climbing crew.
#[must_use]
pub const fn climbing_crew() -> PartyId {
    PartyId::from_bytes([1; 16])
}

/// Uni flat.
#[must_use]
pub const fn uni_flat() -> PartyId {
    PartyId::from_bytes([2; 16])
}

/// Saturday 5-a-side.
#[must_use]
pub const fn five_a_side() -> PartyId {
    PartyId::from_bytes([3; 16])
}

/// Riverside Park, the Climbing crew's Spot with the live Meetup.
#[must_use]
pub const fn riverside_park() -> SpotId {
    SpotId::from_bytes([10; 16])
}

/// The live Meetup.
#[must_use]
pub const fn meetup_id() -> GroupId {
    GroupId::from_bytes([20; 32])
}

/// Ben's leaf in the Meetup.
#[must_use]
pub const fn ben() -> LeafIndex {
    LeafIndex::new(3)
}

/// Chloé's leaf in the Meetup.
#[must_use]
pub const fn chloe() -> LeafIndex {
    LeafIndex::new(1)
}

/// Sam's (this device's) leaf in the Meetup.
#[must_use]
pub const fn sam() -> LeafIndex {
    LeafIndex::new(0)
}

/// Ben's request to see Sam's exact location.
#[must_use]
pub const fn location_request_id() -> LocationRequestId {
    LocationRequestId::from_bytes([30; 16])
}

/// Ben's first chat message.
#[must_use]
pub const fn coffee_message() -> ChatMessageId {
    ChatMessageId::from_bytes([40; 16])
}

/// A point `north` and `east` metres from `origin`, near enough for a flat approximation.
///
/// # Errors
///
/// [`GeoError`] if it falls off the globe.
pub fn offset(origin: Coordinate, north: f64, east: f64) -> Result<Coordinate, GeoError> {
    let metres_per_degree = 111_320.0;
    let latitude = origin.latitude() + north / metres_per_degree;
    let longitude =
        origin.longitude() + east / (metres_per_degree * origin.latitude().to_radians().cos());
    Coordinate::new(latitude, longitude)
}

/// Where Sam is: in Riverside Park.
///
/// # Errors
///
/// Never in practice.
pub fn sam_position() -> Result<Coordinate, GeoError> {
    Coordinate::new(51.4850, -0.1250)
}

/// Where Ben is: 120 m north-east of Sam.
///
/// # Errors
///
/// Never in practice.
pub fn ben_position() -> Result<Coordinate, GeoError> {
    let degrees = 40.0_f64.to_radians();
    offset(
        sam_position()?,
        120.0 * degrees.cos(),
        120.0 * degrees.sin(),
    )
}

/// The bandstand meet point: 85 m north-east of Sam.
///
/// # Errors
///
/// Never in practice.
pub fn bandstand() -> Result<Coordinate, GeoError> {
    let degrees = 42.0_f64.to_radians();
    offset(sam_position()?, 85.0 * degrees.cos(), 85.0 * degrees.sin())
}

/// A square of `side` metres with its south-west corner at `origin`.
///
/// # Errors
///
/// Never in practice.
pub fn square(origin: Coordinate, side: f64) -> Result<Polygon, GeoError> {
    Polygon::new(vec![
        origin,
        offset(origin, 0.0, side)?,
        offset(origin, side, side)?,
        offset(origin, side, 0.0)?,
    ])
}

fn name(text: &str) -> Result<Name, ScreenError> {
    Name::new(text)
}

fn members(names: &[&str], me_at: u32) -> Result<Vec<Member>, ScreenError> {
    let mut members = Vec::new();
    for (leaf, text) in (0..).zip(names) {
        members.push(Member {
            leaf: LeafIndex::new(leaf),
            name: name(text)?,
            me: if leaf == me_at { Me::Me } else { Me::Other },
        });
    }
    Ok(members)
}

fn circle(center: Coordinate, radius: u32) -> Result<Fence, GeoError> {
    Ok(Fence::Circle(Circle::new(
        center,
        Meters::from_whole(radius),
    )?))
}

/// The configuration the canvas shows: three operators, official push relay.
///
/// # Errors
///
/// Never in practice.
pub fn full_privacy_config() -> Result<PartyConfig, ScreenError> {
    Ok(PartyConfig {
        server: Service {
            host: Host::new("pingers.flat7.org")?,
            operator: Operator::new("Flat 7")?,
        },
        hosting: Hosting::SelfHosted,
        relay: Service {
            host: Host::new("relay.quietpath.net")?,
            operator: Operator::new("Quietpath")?,
        },
        push_relay: PushRelay {
            kind: PushRelayKind::Official,
            operator: Operator::new("Pingers")?,
        },
        post_quantum: PostQuantum::Off,
    })
}

fn climbing_crew_party() -> Result<Party, FixtureError> {
    let park = Coordinate::new(51.4855, -0.1240)?;
    Ok(Party {
        id: climbing_crew(),
        name: name("Climbing crew")?,
        colour: PartyColour::Blue,
        members: members(&["Ana", "Chloé", "Sam", "Ben", "Dev", "Eli"], 2)?,
        spots: vec![
            Spot {
                id: riverside_park(),
                name: name("Riverside Park")?,
                fence: circle(park, 300)?,
            },
            Spot {
                id: SpotId::from_bytes([11; 16]),
                name: name("The Wall climbing gym")?,
                fence: Fence::Polygon(square(offset(park, 900.0, -400.0)?, 63.25)?),
            },
            Spot {
                id: SpotId::from_bytes([12; 16]),
                name: name("Ana's place")?,
                fence: circle(offset(park, -1500.0, 700.0)?, 100)?,
            },
        ],
        config: full_privacy_config()?,
        invites: vec![
            Invite {
                id: MailboxId::from_bytes([50; 16]),
                status: InviteStatus::Opened {
                    name: name("Ben")?,
                    code: SafetyCode::new("4821")?,
                },
            },
            Invite {
                id: MailboxId::from_bytes([51; 16]),
                status: InviteStatus::Pending {
                    shared_at: at(17, 30),
                    expires_at: at(17, 30)
                        .checked_add(core::time::Duration::from_hours(24))
                        .ok_or(FixtureError::Missing)?,
                },
            },
            Invite {
                id: MailboxId::from_bytes([52; 16]),
                status: InviteStatus::Joined {
                    leaf: LeafIndex::new(1),
                },
            },
        ],
        invite_link: Some(InviteLink {
            url: InviteUrl::new("https://pingers.flat7.org/join#k7Qx")?,
            expires_at: at(19, 30)
                .checked_add(core::time::Duration::from_hours(24))
                .ok_or(FixtureError::Missing)?,
        }),
    })
}

fn other_parties() -> Result<Vec<Party>, FixtureError> {
    let flat = Coordinate::new(51.5400, -0.1000)?;
    Ok(vec![
        Party {
            id: uni_flat(),
            name: name("Uni flat")?,
            colour: PartyColour::Orange,
            members: members(&["Sam", "Dev", "Eli", "Ana"], 0)?,
            spots: vec![
                Spot {
                    id: SpotId::from_bytes([13; 16]),
                    name: name("Flat 7")?,
                    fence: circle(flat, 100)?,
                },
                Spot {
                    id: SpotId::from_bytes([14; 16]),
                    name: name("Main library")?,
                    fence: circle(offset(flat, 800.0, 0.0)?, 120)?,
                },
            ],
            config: full_privacy_config()?,
            invites: Vec::new(),
            invite_link: None,
        },
        Party {
            id: five_a_side(),
            name: name("Saturday 5-a-side")?,
            colour: PartyColour::Ink,
            members: members(
                &[
                    "Sam", "Ben", "Dev", "Eli", "Ana", "Chloé", "Fin", "Gus", "Hal", "Ivy", "Jo",
                ],
                0,
            )?,
            spots: vec![Spot {
                id: SpotId::from_bytes([15; 16]),
                name: name("Hackney Marshes")?,
                fence: circle(Coordinate::new(51.5550, -0.0280)?, 400)?,
            }],
            config: full_privacy_config()?,
            invites: Vec::new(),
            invite_link: None,
        },
    ])
}

fn body(text: &str) -> Result<ChatBody, ProtoError> {
    ChatBody::new(text.to_owned())
}

fn meetup() -> Result<Meetup, FixtureError> {
    Ok(Meetup {
        id: meetup_id(),
        party: climbing_crew(),
        spot: riverside_park(),
        started_at: at(18, 40),
        me: MeetupSelf {
            leaf: sam(),
            name: name("Sam")?,
        },
        others: vec![
            MeetupPeer {
                leaf: ben(),
                name: name("Ben")?,
                access: PeerAccess::Shared(TargetPosition {
                    position: ben_position()?,
                    taken_at: at(19, 29),
                }),
            },
            MeetupPeer {
                leaf: chloe(),
                name: name("Chloé")?,
                access: PeerAccess::Private,
            },
        ],
        meet_point: Some(MeetPointSnapshot {
            place: MeetPlace::Fixed {
                position: bandstand()?,
                label: Some(MeetPointLabel::new("the bandstand".to_owned())?),
            },
            set_by: ben(),
        }),
        chat: vec![
            ChatEntry::Joined {
                leaf: ben(),
                at: at(18, 40),
            },
            ChatEntry::Joined {
                leaf: chloe(),
                at: at(18, 42),
            },
            ChatEntry::Message(ChatMessage {
                id: coffee_message(),
                author: ben(),
                body: body("Coffee after?")?,
                at: at(18, 45),
                replies: vec![ChatReply {
                    id: ChatMessageId::from_bytes([41; 16]),
                    author: chloe(),
                    body: body("Yes please")?,
                    at: at(18, 46),
                }],
            }),
            ChatEntry::MeetPointSet {
                author: ben(),
                label: Some(MeetPointLabel::new("the bandstand".to_owned())?),
            },
            ChatEntry::Message(ChatMessage {
                id: ChatMessageId::from_bytes([42; 16]),
                author: sam(),
                body: body("On my way")?,
                at: at(18, 50),
                replies: Vec::new(),
            }),
        ],
        sharing_with: vec![ben()],
    })
}

/// The settings the canvas shows.
///
/// # Errors
///
/// Never in practice.
pub fn settings() -> Result<Settings, FixtureError> {
    Ok(Settings {
        location_sharing: LocationSharing::On,
        pause_in_focus: PauseInFocus::On,
        focus: FocusState::Off,
        quiet_hours: QuietHoursPreference {
            state: Toggle::Off,
            window: DailyWindow::new(MinuteOfDay::new(23 * 60)?, MinuteOfDay::new(7 * 60)?)?,
        },
        visibility: LocationVisibility::AskEachTime,
        wake_pushes: WakePushes::On,
        hide_trail: HideTrail::On,
    })
}

fn zones() -> Result<Vec<ExclusionZone>, FixtureError> {
    let home = Coordinate::new(51.4700, -0.1400)?;
    Ok(vec![
        ExclusionZone {
            id: ZoneId::new(1),
            name: name("Home")?,
            fence: circle(home, 150)?,
            schedule: ZoneSchedule::ALWAYS,
        },
        ExclusionZone {
            id: ZoneId::new(2),
            name: name("Work")?,
            fence: Fence::Polygon(square(offset(home, 2000.0, 1000.0)?, 120.0)?),
            schedule: ZoneSchedule {
                times: ZoneTimes::Window {
                    days: Days::Weekdays,
                    window: DailyWindow::new(
                        MinuteOfDay::new(8 * 60)?,
                        MinuteOfDay::new(18 * 60)?,
                    )?,
                },
                parties: ZoneParties::All,
            },
        },
    ])
}

/// The whole example.
///
/// # Errors
///
/// Never in practice.
pub fn snapshot() -> Result<ClientSnapshot, FixtureError> {
    let mut parties = vec![climbing_crew_party()?];
    parties.extend(other_parties()?);
    Ok(ClientSnapshot {
        settings: settings()?,
        gate: GateDecision::Share,
        parties,
        zones: zones()?,
        meetups: vec![meetup()?],
        alert: None,
        location_requests: vec![LocationRequest {
            id: location_request_id(),
            meetup: meetup_id(),
            from: ben(),
            expires_at: at(19, 35),
        }],
        ended_meetup: Some(EndedMeetup {
            spot_name: name("Riverside Park")?,
            started_at: at(18, 40),
            ended_at: at(20, 5),
            people: 3,
            messages: 24,
        }),
        join_preview: Some(JoinPreview {
            inviter: name("Ana")?,
            party_name: name("Climbing crew")?,
            colour: PartyColour::Blue,
            members: ["Ana", "Chloé", "Dev", "Ben", "Eli"]
                .iter()
                .map(|text| name(text))
                .collect::<Result<_, _>>()?,
            spot_count: 3,
            safety_code: SafetyCode::new("4821 0937 5512")?,
        }),
        own_position: Some(OwnPosition {
            fix: Fix {
                coordinate: sam_position()?,
                accuracy: Meters::from_whole(6),
                taken_at: at(19, 30),
            },
            heading: None,
        }),
        fallbacks: Vec::<Fallback>::new(),
        relay: RelayStatus::Reachable,
    })
}
