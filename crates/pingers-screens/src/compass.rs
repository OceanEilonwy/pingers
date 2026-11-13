// SPDX-License-Identifier: AGPL-3.0-only
//! The compass: points at the meet point or at a member whose location is shared.

use core::time::Duration;

use pingers_geo::{Bearing, Meters};
use pingers_meetup::{Observer, TargetPosition, compass as point};
use pingers_proto::{GroupId, LeafIndex};

use crate::ScreenError;
use crate::clock::Minutes;
use crate::common::{Direction, Name, Person, person};
use crate::meetup::{MeetPointName, meet_point_position};
use crate::snapshot::{ClientSnapshot, MeetPlace, Meetup, PeerAccess};

/// What the compass is asked to point at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompassSelection {
    /// The meet point.
    MeetPoint,
    /// A member.
    Member(LeafIndex),
}

/// A chip in the target row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompassTarget {
    /// The meet point.
    MeetPoint(MeetPointName),
    /// A member; locked while their location is private (tapping asks to see it).
    Member {
        /// Who.
        person: Person,
        /// Whether their location is private.
        locked: bool,
    },
}

/// Where the target is from here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompassReading {
    /// From this device to the target, from true north.
    pub bearing: Bearing,
    /// Relative to where the phone faces, when the heading is known (0° is straight ahead).
    pub relative_bearing: Option<Bearing>,
    /// The nearest compass point.
    pub direction: Direction,
    /// How far.
    pub distance: Meters,
    /// About how long a walk.
    pub walk: Minutes,
    /// How old a member's shared position is, measured against this device's own fix
    /// ("Updated 40 s ago"); `None` for the meet point, which does not move on its own.
    pub age: Option<Duration>,
}

/// The compass screen.
#[derive(Clone, Debug, PartialEq)]
pub struct CompassView {
    /// The target row.
    pub targets: Vec<CompassTarget>,
    /// What is selected.
    pub selected: CompassSelection,
    /// The selected target.
    pub target: CompassTarget,
    /// Where it is from here; `None` without a fix, or while the target's location is unknown.
    pub reading: Option<CompassReading>,
    /// This device's GPS accuracy ("GPS ±6 m").
    pub gps_accuracy: Option<Meters>,
    /// Who sees that this device is heading over: the members holding its location key.
    pub watchers: Vec<Name>,
}

fn meet_point_name(meetup: &Meetup) -> Result<Option<MeetPointName>, ScreenError> {
    let Some(point) = &meetup.meet_point else {
        return Ok(None);
    };
    Ok(Some(match &point.place {
        MeetPlace::Fixed {
            label: Some(label), ..
        } => MeetPointName::Label(label.clone()),
        MeetPlace::Fixed { label: None, .. } => MeetPointName::Pin,
        MeetPlace::AtMember(leaf) => MeetPointName::AtMember(person(meetup, *leaf)?),
    }))
}

/// The compass screen.
///
/// # Errors
///
/// [`ScreenError::UnknownMeetup`]; [`ScreenError::UnknownMember`] if the selection names a
/// member who is not in the Meetup; [`ScreenError::NoMeetPoint`] if it names the meet point
/// while none is set.
pub fn compass(
    snapshot: &ClientSnapshot,
    id: GroupId,
    selected: CompassSelection,
) -> Result<CompassView, ScreenError> {
    let meetup = snapshot.meetup(id)?;
    let meet_point = meet_point_name(meetup)?;
    let mut targets: Vec<CompassTarget> = meet_point
        .clone()
        .map(CompassTarget::MeetPoint)
        .into_iter()
        .collect();
    targets.extend(meetup.others.iter().map(|peer| CompassTarget::Member {
        person: Person::new(peer.leaf, &peer.name, crate::common::Me::Other),
        locked: !matches!(peer.access, PeerAccess::Shared(_)),
    }));
    let now = snapshot
        .own_position
        .map_or(pingers_time::UnixSeconds::EPOCH, |own| own.fix.taken_at);
    let (target, position) = match selected {
        CompassSelection::MeetPoint => {
            let name = meet_point.ok_or(ScreenError::NoMeetPoint)?;
            let position = meet_point_position(snapshot, meetup).map(|position| TargetPosition {
                position,
                taken_at: now,
            });
            (CompassTarget::MeetPoint(name), position)
        }
        CompassSelection::Member(leaf) => {
            let peer = meetup.peer(leaf)?;
            let position = match peer.access {
                PeerAccess::Shared(target) => Some(target),
                PeerAccess::Private | PeerAccess::Asked => None,
            };
            let target = CompassTarget::Member {
                person: Person::new(peer.leaf, &peer.name, crate::common::Me::Other),
                locked: position.is_none(),
            };
            (target, position)
        }
    };
    let reading = snapshot.own_position.zip(position).map(|(own, target)| {
        let observer = Observer {
            fix: own.fix,
            heading: own.heading,
        };
        let reading = point(&observer, &target, now);
        CompassReading {
            bearing: reading.bearing,
            relative_bearing: reading.relative_bearing,
            direction: Direction::from_bearing(reading.bearing),
            distance: reading.distance,
            walk: Minutes::walking(reading.distance),
            age: match selected {
                CompassSelection::Member(_) => Some(reading.age),
                CompassSelection::MeetPoint => None,
            },
        }
    });
    Ok(CompassView {
        targets,
        selected,
        target,
        reading,
        gps_accuracy: snapshot.own_position.map(|own| own.fix.accuracy),
        watchers: meetup
            .others
            .iter()
            .filter(|peer| meetup.sharing_with.contains(&peer.leaf))
            .map(|peer| peer.name.clone())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn the_compass_points_at_the_meet_point() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = compass(
            &snapshot,
            fixtures::meetup_id(),
            CompassSelection::MeetPoint,
        )?;
        assert_eq!(view.targets.len(), 3);
        assert!(matches!(
            view.targets.first(),
            Some(CompassTarget::MeetPoint(MeetPointName::Label(_)))
        ));
        assert!(matches!(
            view.targets.get(1),
            Some(CompassTarget::Member { locked: false, .. })
        ));
        assert!(matches!(
            view.targets.get(2),
            Some(CompassTarget::Member { locked: true, .. })
        ));
        let reading = view.reading.ok_or(FixtureError::Missing)?;
        assert!((reading.distance.get() - 85.0).abs() < 1.0);
        assert!(
            (reading.bearing.degrees() - 42.0).abs() < 1.0,
            "{}",
            reading.bearing.degrees()
        );
        assert_eq!(reading.direction, Direction::NE);
        assert_eq!(reading.walk.get(), 1);
        assert_eq!(reading.relative_bearing, None);
        assert_eq!(reading.age, None, "the meet point does not age");
        assert_eq!(view.gps_accuracy, Some(Meters::from_whole(6)));
        let watchers: Vec<_> = view.watchers.iter().map(Name::as_str).collect();
        assert_eq!(watchers, ["Ben"]);
        assert_eq!(view.selected, CompassSelection::MeetPoint);
        Ok(())
    }

    #[test]
    fn the_compass_points_at_a_sharing_member() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        if let Some(own) = &mut snapshot.own_position {
            own.heading = Some(Bearing::new(10.0)?);
        }
        let view = compass(
            &snapshot,
            fixtures::meetup_id(),
            CompassSelection::Member(fixtures::ben()),
        )?;
        assert!(
            matches!(&view.target, CompassTarget::Member { person, locked: false } if person.name.as_str() == "Ben")
        );
        let reading = view.reading.ok_or(FixtureError::Missing)?;
        assert!((reading.distance.get() - 120.0).abs() < 1.0);
        assert_eq!(reading.walk, Minutes::walking(reading.distance));
        // Ben shared at 19:29; this device's fix is from 19:30.
        assert_eq!(reading.age, Some(Duration::from_secs(60)));
        let relative = reading.relative_bearing.ok_or(FixtureError::Missing)?;
        assert!(
            (relative.degrees() - 30.0).abs() < 1.0,
            "{}",
            relative.degrees()
        );
        Ok(())
    }

    #[test]
    fn a_private_member_is_locked_and_unmeasured() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = compass(
            &snapshot,
            fixtures::meetup_id(),
            CompassSelection::Member(fixtures::chloe()),
        )?;
        assert!(matches!(
            view.target,
            CompassTarget::Member { locked: true, .. }
        ));
        assert_eq!(view.reading, None);
        Ok(())
    }

    #[test]
    fn without_a_fix_there_is_no_reading() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.own_position = None;
        let view = compass(
            &snapshot,
            fixtures::meetup_id(),
            CompassSelection::MeetPoint,
        )?;
        assert_eq!((view.reading, view.gps_accuracy), (None, None));
        Ok(())
    }

    #[test]
    fn selecting_what_is_not_there_is_an_error() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        assert_eq!(
            compass(
                &snapshot,
                fixtures::meetup_id(),
                CompassSelection::Member(LeafIndex::new(70))
            ),
            Err(ScreenError::UnknownMember)
        );
        for meetup in &mut snapshot.meetups {
            meetup.meet_point = None;
        }
        assert_eq!(
            compass(
                &snapshot,
                fixtures::meetup_id(),
                CompassSelection::MeetPoint
            ),
            Err(ScreenError::NoMeetPoint)
        );
        let view = compass(
            &snapshot,
            fixtures::meetup_id(),
            CompassSelection::Member(fixtures::ben()),
        )?;
        assert_eq!(view.targets.len(), 2);
        Ok(())
    }
}
