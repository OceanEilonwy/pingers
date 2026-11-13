// SPDX-License-Identifier: AGPL-3.0-only
//! The meet point editor: a pin, "at me", or the midpoint of everyone whose position is known.

use pingers_geo::{Coordinate, Meters, distance};
use pingers_proto::GroupId;

use crate::ScreenError;
use crate::common::{DraftText, Me, Person, mean, people};
use crate::meetup::locate;
use crate::snapshot::ClientSnapshot;

/// How the meet point is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeetPointMode {
    /// A pin the user drags the map under.
    Pin,
    /// This device's live position ("Meet at me"). Only a member can make themself the meet
    /// point (design B.6), so "At a person" is always this device.
    AtMe,
    /// The midpoint of everyone whose position this device knows, itself included.
    Midpoint,
}

/// The editor's state, as the user has left it.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetPointDraft {
    /// The mode chosen.
    pub mode: MeetPointMode,
    /// The label typed.
    pub label: DraftText,
    /// Where the pin is (the map's centre).
    pub pin: Coordinate,
}

/// How far a member is from the meet point.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetPointDistance {
    /// Who.
    pub person: Person,
    /// How far; `None` while their location is private (or, for this device, without a fix).
    pub distance: Option<Meters>,
}

/// A member on the editor's map.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberOnMap {
    /// Who.
    pub person: Person,
    /// Where.
    pub at: Coordinate,
}

/// The meet point editor screen.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetPointView {
    /// The mode chosen.
    pub mode: MeetPointMode,
    /// The label typed.
    pub label: DraftText,
    /// Where the meet point would be; `None` when the mode has no position yet (at me without
    /// a fix).
    pub point: Option<Coordinate>,
    /// How far each other member is from it.
    pub distances: Vec<MeetPointDistance>,
    /// Everyone whose position is known, for the map.
    pub members: Vec<MemberOnMap>,
    /// Whether "Share meet point" is enabled.
    pub can_share: bool,
}

/// The meet point editor.
///
/// # Errors
///
/// [`ScreenError::UnknownMeetup`].
pub fn meet_point(
    snapshot: &ClientSnapshot,
    id: GroupId,
    draft: &MeetPointDraft,
) -> Result<MeetPointView, ScreenError> {
    let meetup = snapshot.meetup(id)?;
    let members: Vec<MemberOnMap> = people(meetup)
        .into_iter()
        .filter_map(|person| {
            let at = locate(snapshot, meetup, person.leaf)?;
            Some(MemberOnMap { person, at })
        })
        .collect();
    let point = match draft.mode {
        MeetPointMode::Pin => Some(draft.pin),
        MeetPointMode::AtMe => snapshot.own_coordinate(),
        MeetPointMode::Midpoint => {
            let known: Vec<Coordinate> = members.iter().map(|member| member.at).collect();
            mean(&known)
        }
    };
    let distances = people(meetup)
        .into_iter()
        .filter(|person| person.me == Me::Other)
        .map(|person| MeetPointDistance {
            distance: point
                .zip(locate(snapshot, meetup, person.leaf))
                .map(|(point, at)| distance(at, point)),
            person,
        })
        .collect();
    Ok(MeetPointView {
        mode: draft.mode,
        label: draft.label.clone(),
        point,
        distances,
        members,
        can_share: point.is_some(),
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    fn draft(mode: MeetPointMode) -> Result<MeetPointDraft, FixtureError> {
        Ok(MeetPointDraft {
            mode,
            label: DraftText::new("Bandstand".into()),
            pin: fixtures::bandstand()?,
        })
    }

    #[test]
    fn a_pin_measures_from_each_member() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = meet_point(
            &snapshot,
            fixtures::meetup_id(),
            &draft(MeetPointMode::Pin)?,
        )?;
        assert_eq!(view.point, Some(fixtures::bandstand()?));
        assert!(view.can_share);
        assert_eq!(view.label.as_str(), "Bandstand");
        let [ben, chloe] = view.distances.as_slice() else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(ben.person.name.as_str(), "Ben");
        let expected = distance(fixtures::ben_position()?, fixtures::bandstand()?);
        assert_eq!(ben.distance, Some(expected));
        assert_eq!(
            (chloe.person.name.as_str(), chloe.distance),
            ("Chloé", None)
        );
        let on_map: Vec<_> = view
            .members
            .iter()
            .map(|member| member.person.name.as_str())
            .collect();
        assert_eq!(on_map, ["Sam", "Ben"]);
        Ok(())
    }

    #[test]
    fn at_me_needs_a_fix() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let view = meet_point(
            &snapshot,
            fixtures::meetup_id(),
            &draft(MeetPointMode::AtMe)?,
        )?;
        assert_eq!(view.point, Some(fixtures::sam_position()?));
        snapshot.own_position = None;
        let view = meet_point(
            &snapshot,
            fixtures::meetup_id(),
            &draft(MeetPointMode::AtMe)?,
        )?;
        assert_eq!(view.point, None);
        assert!(!view.can_share);
        assert!(view.distances.iter().all(|row| row.distance.is_none()));
        Ok(())
    }

    #[test]
    fn the_midpoint_is_between_everyone_known() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = meet_point(
            &snapshot,
            fixtures::meetup_id(),
            &draft(MeetPointMode::Midpoint)?,
        )?;
        let point = view.point.ok_or(FixtureError::Missing)?;
        let sam = fixtures::sam_position()?;
        let ben = fixtures::ben_position()?;
        assert!((point.latitude() - f64::midpoint(sam.latitude(), ben.latitude())).abs() < 1e-9);
        assert!((point.longitude() - f64::midpoint(sam.longitude(), ben.longitude())).abs() < 1e-9);
        assert_eq!(view.mode, MeetPointMode::Midpoint);
        assert_eq!(
            meet_point(
                &snapshot,
                GroupId::from_bytes([0; 32]),
                &draft(MeetPointMode::Pin)?
            ),
            Err(ScreenError::UnknownMeetup)
        );
        Ok(())
    }
}
