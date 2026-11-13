// SPDX-License-Identifier: AGPL-3.0-only
//! Values shared by several screens: names, people, colours, directions and operators.

use core::fmt;

use pingers_geo::{Bearing, Coordinate};
use pingers_proto::LeafIndex;

pub use pingers_client::snapshot::{Host, Me, Name, Operator, PartyColour};

use crate::ScreenError;
use crate::snapshot::Meetup;

/// Text the user is still typing, before it is a [`Name`]. May be empty. `Debug` does not print
/// it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct DraftText(String);

impl DraftText {
    /// Wraps what is in the text field.
    #[must_use]
    pub const fn new(text: String) -> Self {
        Self(text)
    }

    /// The text as typed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The text as a name, if it makes one.
    #[must_use]
    pub fn name(&self) -> Option<Name> {
        Name::new(&self.0).ok()
    }
}

impl fmt::Debug for DraftText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DraftText(redacted)")
    }
}

/// An avatar's letters, from [`Name::initials`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Initials(String);

impl Initials {
    /// Two letters for a name's avatar: the first letters of the first two words, or the first
    /// two letters of a one-word name ("Climbing crew" is "CC", "Ben" is "BE").
    #[must_use]
    pub fn of(name: &Name) -> Self {
        let mut words = name.as_str().split_whitespace();
        let first = words.next().unwrap_or_default();
        let letters: String = match words.next() {
            Some(second) => first
                .chars()
                .take(1)
                .chain(second.chars().take(1))
                .collect(),
            None => first.chars().take(2).collect(),
        };
        Self(letters.to_uppercase())
    }

    /// The letters.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Avatar background tints from the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tint {
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
    /// Stone: always this device's own avatar.
    Stone,
}

impl Tint {
    /// A member's tint, stable for their leaf in the group, so a person keeps their colour as
    /// others join and leave. This device is always [`Tint::Stone`].
    #[must_use]
    pub const fn for_member(leaf: LeafIndex, me: Me) -> Self {
        if matches!(me, Me::Me) {
            return Self::Stone;
        }
        match leaf.get() % 6 {
            0 => Self::Peach,
            1 => Self::Sky,
            2 => Self::Sage,
            3 => Self::Butter,
            4 => Self::Lilac,
            _ => Self::Mint,
        }
    }
}

/// A person as an avatar row shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// Their leaf in the group the screen is about (the Party's, or the Meetup's).
    pub leaf: LeafIndex,
    /// Their name.
    pub name: Name,
    /// Avatar letters.
    pub initials: Initials,
    /// Avatar tint.
    pub tint: Tint,
    /// Whether it is this device's user.
    pub me: Me,
}

impl Person {
    /// The person for a member of a group.
    #[must_use]
    pub fn new(leaf: LeafIndex, name: &Name, me: Me) -> Self {
        Self {
            leaf,
            initials: Initials::of(name),
            name: name.clone(),
            tint: Tint::for_member(leaf, me),
            me,
        }
    }
}

/// The member of `meetup` with this leaf, this device included, as an avatar shows them.
///
/// # Errors
///
/// [`ScreenError::UnknownMember`].
pub fn person(meetup: &Meetup, leaf: LeafIndex) -> Result<Person, ScreenError> {
    if leaf == meetup.me.leaf {
        return Ok(Person::new(leaf, &meetup.me.name, Me::Me));
    }
    let peer = meetup.peer(leaf)?;
    Ok(Person::new(leaf, &peer.name, Me::Other))
}

/// Everyone in `meetup`, this device first.
#[must_use]
pub fn people(meetup: &Meetup) -> Vec<Person> {
    core::iter::once(Person::new(meetup.me.leaf, &meetup.me.name, Me::Me))
        .chain(
            meetup
                .others
                .iter()
                .map(|peer| Person::new(peer.leaf, &peer.name, Me::Other)),
        )
        .collect()
}

/// How private a Party's setup is, from who runs the server and the two relays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrivacyLevel {
    /// Server, privacy relay and (when used) push relay are three different operators.
    Full,
    /// At least two of them are the same operator.
    Reduced,
}

/// A compass point, for "120 m north-east".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
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

impl Direction {
    /// The nearest of the eight points to a bearing; each covers 45°, centred on its point.
    #[must_use]
    pub fn from_bearing(bearing: Bearing) -> Self {
        const POINTS: [Direction; 8] = [
            Direction::N,
            Direction::NE,
            Direction::E,
            Direction::SE,
            Direction::S,
            Direction::SW,
            Direction::W,
            Direction::NW,
        ];
        let degrees = bearing.degrees();
        let mut upper = 22.5;
        for point in POINTS {
            if degrees < upper {
                return point;
            }
            upper += 45.0;
        }
        Self::N
    }
}

/// The midpoint of two nearby coordinates, by averaging (accurate enough for the points of one
/// Spot or the members of one Meetup), the short way round across the antimeridian.
pub(crate) fn midpoint(a: Coordinate, b: Coordinate) -> Coordinate {
    mean(&[a, b]).unwrap_or(a)
}

/// The mean of nearby coordinates, the short way round across the antimeridian. `None` for an
/// empty list.
pub(crate) fn mean(points: &[Coordinate]) -> Option<Coordinate> {
    let first = points.first()?;
    let mut latitude = 0.0;
    let mut longitude = 0.0;
    let mut count = 0.0;
    for point in points {
        latitude += point.latitude();
        let mut lon = point.longitude();
        if lon - first.longitude() > 180.0 {
            lon -= 360.0;
        } else if first.longitude() - lon > 180.0 {
            lon += 360.0;
        }
        longitude += lon;
        count += 1.0;
    }
    let mut longitude = longitude / count;
    if longitude > 180.0 {
        longitude -= 360.0;
    } else if longitude < -180.0 {
        longitude += 360.0;
    }
    Coordinate::new(latitude / count, longitude).ok()
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn initials_take_two_words_or_two_letters() -> Result<(), ScreenError> {
        assert_eq!(Initials::of(&Name::new("Climbing crew")?).as_str(), "CC");
        assert_eq!(Initials::of(&Name::new("Ben")?).as_str(), "BE");
        assert_eq!(Initials::of(&Name::new("chloé martin")?).as_str(), "CM");
        assert_eq!(Initials::of(&Name::new("Q")?).as_str(), "Q");
        Ok(())
    }

    #[test]
    fn draft_text_is_redacted() {
        assert_eq!(
            format!("{:?}", DraftText::new("Home".into())),
            "DraftText(redacted)"
        );
    }

    #[test]
    fn draft_text_makes_a_name_only_when_it_is_one() {
        assert!(DraftText::new("  ".into()).name().is_none());
        assert_eq!(
            DraftText::new(" Uni ".into())
                .name()
                .map(|name| name.as_str().to_owned()),
            Some("Uni".to_owned())
        );
    }

    #[test]
    fn tints_are_stable_per_leaf_and_stone_for_me() {
        let tint = |leaf| Tint::for_member(LeafIndex::new(leaf), Me::Other);
        assert_eq!(
            [
                tint(0),
                tint(1),
                tint(2),
                tint(3),
                tint(4),
                tint(5),
                tint(6)
            ],
            [
                Tint::Peach,
                Tint::Sky,
                Tint::Sage,
                Tint::Butter,
                Tint::Lilac,
                Tint::Mint,
                Tint::Peach
            ]
        );
        assert_eq!(Tint::for_member(LeafIndex::new(3), Me::Me), Tint::Stone);
    }

    #[test]
    fn directions_cover_45_degrees_centred_on_each_point() -> Result<(), pingers_geo::GeoError> {
        let direction = |degrees| Bearing::new(degrees).map(Direction::from_bearing);
        assert_eq!(direction(0.0)?, Direction::N);
        assert_eq!(direction(22.4)?, Direction::N);
        assert_eq!(direction(22.5)?, Direction::NE);
        assert_eq!(direction(40.0)?, Direction::NE);
        assert_eq!(direction(90.0)?, Direction::E);
        assert_eq!(direction(135.0)?, Direction::SE);
        assert_eq!(direction(180.0)?, Direction::S);
        assert_eq!(direction(225.0)?, Direction::SW);
        assert_eq!(direction(270.0)?, Direction::W);
        assert_eq!(direction(315.0)?, Direction::NW);
        assert_eq!(direction(337.4)?, Direction::NW);
        assert_eq!(direction(337.5)?, Direction::N);
        assert_eq!(direction(359.9)?, Direction::N);
        Ok(())
    }

    #[test]
    fn means_go_the_short_way_round() -> Result<(), pingers_geo::GeoError> {
        let mid = midpoint(
            Coordinate::new(10.0, 179.0)?,
            Coordinate::new(12.0, -179.0)?,
        );
        assert!((mid.latitude() - 11.0).abs() < 1e-9);
        assert!((mid.longitude().abs() - 180.0).abs() < 1e-9);
        let mid = midpoint(Coordinate::new(0.0, -179.0)?, Coordinate::new(0.0, 177.0)?);
        assert!(
            (mid.longitude() - 179.0).abs() < 1e-9,
            "{}",
            mid.longitude()
        );
        let longitude = |points: &[(f64, f64)]| -> Result<f64, pingers_geo::GeoError> {
            let points = points
                .iter()
                .map(|(lat, lon)| Coordinate::new(*lat, *lon))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(mean(&points).map_or(f64::NAN, Coordinate::longitude))
        };
        // Three points, so wrapping the wrong way does not happen to land on the same answer.
        let west_first = longitude(&[(0.0, -179.0), (0.0, 177.0), (0.0, 178.0)])?;
        assert!(
            (west_first - (178.0 + 2.0 / 3.0)).abs() < 1e-9,
            "{west_first}"
        );
        let east_first = longitude(&[(0.0, 179.0), (0.0, -177.0), (0.0, -178.0)])?;
        assert!(
            (east_first + (178.0 + 2.0 / 3.0)).abs() < 1e-9,
            "{east_first}"
        );
        // Exactly half way round is not wrapped.
        assert!((longitude(&[(0.0, 0.0), (0.0, 180.0)])? - 90.0).abs() < 1e-9);
        assert!((longitude(&[(0.0, 0.0), (0.0, -180.0)])? + 90.0).abs() < 1e-9);
        // A mean of exactly ±180 keeps its sign.
        assert!((longitude(&[(0.0, 170.0), (0.0, -170.0)])? - 180.0).abs() < 1e-9);
        assert!((longitude(&[(0.0, -170.0), (0.0, 170.0)])? + 180.0).abs() < 1e-9);
        let mid = midpoint(Coordinate::new(0.0, 10.0)?, Coordinate::new(2.0, 20.0)?);
        assert!((mid.longitude() - 15.0).abs() < 1e-9 && (mid.latitude() - 1.0).abs() < 1e-9);
        assert_eq!(mean(&[]), None);
        Ok(())
    }
}
