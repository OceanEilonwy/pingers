// SPDX-License-Identifier: AGPL-3.0-only
//! A live Meetup's screen, and the Meetup ended summary.

use pingers_geo::{Coordinate, Meters, bearing, distance};
use pingers_proto::{GroupId, LeafIndex, MeetPointLabel};

use crate::ScreenError;
use crate::clock::{LocalClock, LocalTime, Minutes};
use crate::common::{Direction, Name, Person};
use crate::home::meetup_names;
use crate::snapshot::{ChatEntry, ClientSnapshot, MeetPlace, Meetup, PeerAccess};

/// What a meet point is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeetPointName {
    /// The label it was given ("the bandstand").
    Label(MeetPointLabel),
    /// An unlabelled pin ("the pin").
    Pin,
    /// A member's live position.
    AtMember(Person),
}

/// The meet point card.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetPointCard {
    /// What it is called.
    pub name: MeetPointName,
    /// Who set it.
    pub set_by: Person,
    /// How far it is from this device, when both positions are known.
    pub distance: Option<Meters>,
    /// Where it is on the map, when known.
    pub at: Option<Coordinate>,
}

/// What a member's exact location looks like to this device.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PeerLocation {
    /// Shared: where they are, and how far and which way from here when this device has a fix.
    Shared {
        /// Their position.
        at: Coordinate,
        /// How far.
        distance: Option<Meters>,
        /// Which way.
        direction: Option<Direction>,
    },
    /// Private: "Here · exact spot private".
    Private,
    /// Private, and asked: "Here · asked to see exact spot".
    Asked,
}

/// A member row.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetupMember {
    /// Who.
    pub person: Person,
    /// Their exact location, as far as this device may see it.
    pub location: PeerLocation,
}

/// The Meetup screen.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetupView {
    /// The Meetup.
    pub id: GroupId,
    /// Its Party's name.
    pub party_name: Name,
    /// Its Spot's name.
    pub spot_name: Name,
    /// How many people are here, this device included.
    pub here_count: usize,
    /// The meet point, if set.
    pub meet_point: Option<MeetPointCard>,
    /// The other members.
    pub members: Vec<MeetupMember>,
    /// This device on the map.
    pub me: Option<Coordinate>,
    /// How many chat messages, replies included.
    pub chat_count: usize,
    /// Who holds this device's location key ("Your exact spot: shared with Ben").
    pub shared_with: Vec<Name>,
}

/// Where a member is, as far as this device may know: its own fix, or a shared position.
pub(crate) fn locate(
    snapshot: &ClientSnapshot,
    meetup: &Meetup,
    leaf: LeafIndex,
) -> Option<Coordinate> {
    if leaf == meetup.me.leaf {
        return snapshot.own_coordinate();
    }
    match meetup.peer(leaf).ok()?.access {
        PeerAccess::Shared(target) => Some(target.position),
        PeerAccess::Private | PeerAccess::Asked => None,
    }
}

/// Where the meet point is, when known.
pub(crate) fn meet_point_position(
    snapshot: &ClientSnapshot,
    meetup: &Meetup,
) -> Option<Coordinate> {
    match &meetup.meet_point.as_ref()?.place {
        MeetPlace::Fixed { position, .. } => Some(*position),
        MeetPlace::AtMember(leaf) => locate(snapshot, meetup, *leaf),
    }
}

/// How many messages a chat holds, replies included.
pub(crate) fn message_count(chat: &[ChatEntry]) -> usize {
    chat.iter()
        .map(|entry| match entry {
            ChatEntry::Message(message) => message.replies.len().saturating_add(1),
            ChatEntry::Joined { .. } | ChatEntry::MeetPointSet { .. } => 0,
        })
        .fold(0, usize::saturating_add)
}

/// The Meetup screen.
///
/// # Errors
///
/// [`ScreenError::UnknownMeetup`] if the snapshot has no such Meetup, or no longer has its
/// Party or Spot; [`ScreenError::UnknownMember`] if the meet point names someone not in it.
pub fn meetup(snapshot: &ClientSnapshot, id: GroupId) -> Result<MeetupView, ScreenError> {
    let meetup = snapshot.meetup(id)?;
    let names = meetup_names(snapshot, meetup).ok_or(ScreenError::UnknownMeetup)?;
    let me = snapshot.own_coordinate();
    let from_me = |to: Coordinate| me.map(|here| distance(here, to));
    let meet_point = match &meetup.meet_point {
        None => None,
        Some(point) => {
            let name = match &point.place {
                MeetPlace::Fixed {
                    label: Some(label), ..
                } => MeetPointName::Label(label.clone()),
                MeetPlace::Fixed { label: None, .. } => MeetPointName::Pin,
                MeetPlace::AtMember(leaf) => MeetPointName::AtMember(meetup.person(*leaf)?),
            };
            let at = meet_point_position(snapshot, meetup);
            Some(MeetPointCard {
                name,
                set_by: meetup.person(point.set_by)?,
                distance: at.and_then(from_me),
                at,
            })
        }
    };
    let members = meetup
        .others
        .iter()
        .map(|peer| MeetupMember {
            person: Person::new(peer.leaf, &peer.name, crate::common::Me::Other),
            location: match peer.access {
                PeerAccess::Shared(target) => PeerLocation::Shared {
                    at: target.position,
                    distance: from_me(target.position),
                    direction: me
                        .map(|here| Direction::from_bearing(bearing(here, target.position))),
                },
                PeerAccess::Private => PeerLocation::Private,
                PeerAccess::Asked => PeerLocation::Asked,
            },
        })
        .collect();
    Ok(MeetupView {
        id,
        party_name: names.party,
        spot_name: names.spot,
        here_count: meetup.head_count(),
        meet_point,
        members,
        me,
        chat_count: message_count(&meetup.chat),
        shared_with: meetup
            .others
            .iter()
            .filter(|peer| meetup.sharing_with.contains(&peer.leaf))
            .map(|peer| peer.name.clone())
            .collect(),
    })
}

/// The Meetup ended screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetupEnded {
    /// The Spot's name ("Everyone left Riverside Park").
    pub spot_name: Name,
    /// How long it lasted.
    pub duration: Minutes,
    /// When it started, local time.
    pub from: LocalTime,
    /// When it ended, local time.
    pub to: LocalTime,
    /// How many people met.
    pub people: usize,
    /// How many messages were sent.
    pub messages: usize,
}

/// The Meetup ended screen, if a Meetup ended and its summary has not been dismissed.
#[must_use]
pub fn meetup_ended(snapshot: &ClientSnapshot, clock: LocalClock) -> Option<MeetupEnded> {
    let ended = snapshot.ended_meetup.as_ref()?;
    Some(MeetupEnded {
        spot_name: ended.spot_name.clone(),
        duration: Minutes::from_duration(
            ended.ended_at.saturating_duration_since(ended.started_at),
        ),
        from: clock.time_of(ended.started_at),
        to: clock.time_of(ended.ended_at),
        people: ended.people,
        messages: ended.messages,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};
    use crate::snapshot::MeetPointSnapshot;

    fn close(distance: Option<Meters>, metres: f64) -> bool {
        distance.is_some_and(|distance| (distance.get() - metres).abs() < 1.0)
    }

    #[test]
    fn the_meetup_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = meetup(&snapshot, fixtures::meetup_id())?;
        assert_eq!(view.party_name.as_str(), "Climbing crew");
        assert_eq!(view.spot_name.as_str(), "Riverside Park");
        assert_eq!(view.here_count, 3);
        assert_eq!(view.chat_count, 3);
        let shared: Vec<_> = view.shared_with.iter().map(Name::as_str).collect();
        assert_eq!(shared, ["Ben"]);
        assert_eq!(view.me, Some(fixtures::sam_position()?));

        let point = view.meet_point.ok_or(FixtureError::Missing)?;
        assert!(
            matches!(&point.name, MeetPointName::Label(label) if label.as_str() == "the bandstand")
        );
        assert_eq!(point.set_by.name.as_str(), "Ben");
        assert!(close(point.distance, 85.0), "{:?}", point.distance);
        assert_eq!(point.at, Some(fixtures::bandstand()?));

        let [ben, chloe] = view.members.as_slice() else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(ben.person.name.as_str(), "Ben");
        let PeerLocation::Shared {
            at,
            distance,
            direction,
        } = ben.location
        else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(at, fixtures::ben_position()?);
        assert!(close(distance, 120.0), "{distance:?}");
        assert_eq!(direction, Some(Direction::NE));
        assert_eq!(chloe.location, PeerLocation::Private);
        Ok(())
    }

    #[test]
    fn without_a_fix_nothing_is_measured() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.own_position = None;
        let view = meetup(&snapshot, fixtures::meetup_id())?;
        assert_eq!(view.me, None);
        assert_eq!(view.meet_point.and_then(|point| point.distance), None);
        let ben = view.members.first().ok_or(FixtureError::Missing)?;
        assert!(matches!(
            ben.location,
            PeerLocation::Shared {
                distance: None,
                direction: None,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn meet_points_are_named_by_label_pin_or_person() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let name = |snapshot: &ClientSnapshot| -> Result<MeetPointCard, FixtureError> {
            meetup(snapshot, fixtures::meetup_id())?
                .meet_point
                .ok_or(FixtureError::Missing)
        };
        let set = |snapshot: &mut ClientSnapshot, place: MeetPlace| {
            for meetup in &mut snapshot.meetups {
                meetup.meet_point = Some(MeetPointSnapshot {
                    place: place.clone(),
                    set_by: fixtures::chloe(),
                });
            }
        };
        set(
            &mut snapshot,
            MeetPlace::Fixed {
                position: fixtures::bandstand()?,
                label: None,
            },
        );
        assert_eq!(name(&snapshot)?.name, MeetPointName::Pin);
        set(&mut snapshot, MeetPlace::AtMember(fixtures::ben()));
        let card = name(&snapshot)?;
        assert!(
            matches!(&card.name, MeetPointName::AtMember(person) if person.name.as_str() == "Ben")
        );
        assert_eq!(card.at, Some(fixtures::ben_position()?));
        assert_eq!(card.set_by.name.as_str(), "Chloé");
        set(&mut snapshot, MeetPlace::AtMember(fixtures::sam()));
        let card = name(&snapshot)?;
        assert_eq!(card.at, Some(fixtures::sam_position()?));
        assert!(close(card.distance, 0.0));
        // Chloé's location is private, so a meet point at her has no position here.
        set(&mut snapshot, MeetPlace::AtMember(fixtures::chloe()));
        let card = name(&snapshot)?;
        assert_eq!((card.at, card.distance), (None, None));
        set(&mut snapshot, MeetPlace::AtMember(LeafIndex::new(77)));
        assert_eq!(
            meetup(&snapshot, fixtures::meetup_id()),
            Err(ScreenError::UnknownMember)
        );
        for meetup in &mut snapshot.meetups {
            meetup.meet_point = None;
        }
        assert_eq!(meetup(&snapshot, fixtures::meetup_id())?.meet_point, None);
        Ok(())
    }

    #[test]
    fn asked_members_say_so() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for meetup in &mut snapshot.meetups {
            for peer in &mut meetup.others {
                peer.access = PeerAccess::Asked;
            }
        }
        let view = meetup(&snapshot, fixtures::meetup_id())?;
        assert!(
            view.members
                .iter()
                .all(|member| member.location == PeerLocation::Asked)
        );
        Ok(())
    }

    #[test]
    fn a_missing_meetup_or_party_is_an_error() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        assert_eq!(
            meetup(&snapshot, GroupId::from_bytes([0; 32])),
            Err(ScreenError::UnknownMeetup)
        );
        snapshot.parties.clear();
        assert_eq!(
            meetup(&snapshot, fixtures::meetup_id()),
            Err(ScreenError::UnknownMeetup)
        );
        Ok(())
    }

    #[test]
    fn meetup_ended_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = meetup_ended(&snapshot, fixtures::clock()).ok_or(FixtureError::Missing)?;
        assert_eq!(view.spot_name.as_str(), "Riverside Park");
        assert_eq!(
            (view.duration.hours(), view.duration.minutes_past_hour()),
            (1, 25)
        );
        assert_eq!((view.from.hour(), view.from.minute()), (18, 40));
        assert_eq!((view.to.hour(), view.to.minute()), (20, 5));
        assert_eq!((view.people, view.messages), (3, 24));
        let mut dismissed = snapshot;
        dismissed.ended_meetup = None;
        assert_eq!(meetup_ended(&dismissed, fixtures::clock()), None);
        Ok(())
    }
}
