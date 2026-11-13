// SPDX-License-Identifier: AGPL-3.0-only
//! Values shared by several screens: names, people, colours, directions and operators.

use core::fmt;

use pingers_geo::{Bearing, Coordinate};
use pingers_proto::LeafIndex;

use crate::ScreenError;

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
    /// [`ScreenError::InvalidName`] if it is empty after trimming or too long.
    pub fn new(text: &str) -> Result<Self, ScreenError> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.chars().count() > Self::MAX_CHARACTERS {
            return Err(ScreenError::InvalidName);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Two letters for an avatar: the first letters of the first two words, or the first two
    /// letters of a one-word name ("Climbing crew" is "CC", "Ben" is "BE").
    #[must_use]
    pub fn initials(&self) -> Initials {
        let mut words = self.0.split_whitespace();
        let first = words.next().unwrap_or_default();
        let letters: String = match words.next() {
            Some(second) => first
                .chars()
                .take(1)
                .chain(second.chars().take(1))
                .collect(),
            None => first.chars().take(2).collect(),
        };
        Initials(letters.to_uppercase())
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Name(redacted)")
    }
}

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
            initials: name.initials(),
            name: name.clone(),
            tint: Tint::for_member(leaf, me),
            me,
        }
    }
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

/// A server or relay host as the user typed or the server advertised it ("pingers.flat7.org").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Host(String);

impl Host {
    /// Checks a host: not empty, no spaces.
    ///
    /// # Errors
    ///
    /// [`ScreenError::InvalidHost`].
    pub fn new(text: &str) -> Result<Self, ScreenError> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
            return Err(ScreenError::InvalidHost);
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    /// The host.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The operator a server or relay declares (design B.4.9). Compared without regard to case or
/// surrounding space, so "Flat 7" and "flat 7 " are one operator.
#[derive(Clone, Debug)]
pub struct Operator(String);

impl Operator {
    /// Checks an operator name: not empty.
    ///
    /// # Errors
    ///
    /// [`ScreenError::InvalidOperator`].
    pub fn new(text: &str) -> Result<Self, ScreenError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ScreenError::InvalidOperator);
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
        assert_eq!(Name::new("Climbing crew")?.initials().as_str(), "CC");
        assert_eq!(Name::new("Ben")?.initials().as_str(), "BE");
        assert_eq!(Name::new("chloé martin")?.initials().as_str(), "CM");
        assert_eq!(Name::new("Q")?.initials().as_str(), "Q");
        Ok(())
    }

    #[test]
    fn names_are_trimmed_and_never_empty_or_too_long() -> Result<(), ScreenError> {
        assert_eq!(Name::new("  Uni flat ")?.as_str(), "Uni flat");
        assert_eq!(Name::new("   "), Err(ScreenError::InvalidName));
        assert!(Name::new(&"a".repeat(Name::MAX_CHARACTERS)).is_ok());
        assert_eq!(
            Name::new(&"a".repeat(Name::MAX_CHARACTERS + 1)),
            Err(ScreenError::InvalidName)
        );
        assert_eq!(format!("{:?}", Name::new("Home")?), "Name(redacted)");
        assert_eq!(
            format!("{:?}", DraftText::new("Home".into())),
            "DraftText(redacted)"
        );
        Ok(())
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
    fn hosts_and_operators_are_normalised() -> Result<(), ScreenError> {
        assert_eq!(
            Host::new(" Pingers.Flat7.org ")?.as_str(),
            "pingers.flat7.org"
        );
        assert_eq!(Host::new("a b"), Err(ScreenError::InvalidHost));
        assert_eq!(Host::new(" "), Err(ScreenError::InvalidHost));
        assert_eq!(Operator::new("Flat 7")?, Operator::new(" flat 7 ")?);
        assert_ne!(Operator::new("Flat 7")?, Operator::new("Flat 8")?);
        assert_eq!(Operator::new(" Flat 7 ")?.as_str(), "Flat 7");
        assert_eq!(Operator::new(""), Err(ScreenError::InvalidOperator));
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
