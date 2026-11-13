// SPDX-License-Identifier: AGPL-3.0-only
//! Builders for the fixtures most tests need: a Party with members and Spots, and fixes
//! inside or outside a Spot.
//!
//! These are plain data in L0 vocabulary (proto ids, geo fences and fixes). Each crate turns
//! them into its own domain types; the testkit does not depend on the crates that own Parties.

use std::collections::BTreeSet;

use pingers_geo::{Circle, Coordinate, Fence, Fix, GeoError, Meters, exit_buffer};
use pingers_proto::{AccountId, LeafIndex, PartyId, SpotId};
use pingers_time::UnixSeconds;

use crate::rng::random_array;

/// Names given by [`PartyBuilder::members`], in order; later members are `member-<n>`.
const DEFAULT_NAMES: [&str; 8] = [
    "alice", "bob", "carol", "dave", "erin", "frank", "grace", "heidi",
];

/// The mean Earth radius pingers-geo measures distances with.
const EARTH_RADIUS_METERS: f64 = 6_371_008.8;

/// Why a fixture could not be built.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum FixtureError {
    /// A coordinate or fence was invalid.
    #[error(transparent)]
    Geo(#[from] GeoError),
    /// A Party needs at least one member.
    #[error("a Party needs at least one member")]
    NoMembers,
    /// Two members have the same name.
    #[error("two members are named `{0}`")]
    DuplicateMember(String),
    /// Two Spots have the same name.
    #[error("two Spots are named `{0}`")]
    DuplicateSpot(String),
    /// More members than MLS leaf indices.
    #[error("too many members for a leaf index")]
    TooManyMembers,
    /// No member has this name.
    #[error("no member is named `{0}`")]
    UnknownMember(String),
    /// No Spot has this name.
    #[error("no Spot is named `{0}`")]
    UnknownSpot(String),
}

/// A Party member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberFixture {
    /// A readable name for test output, for example `alice`.
    pub name: String,
    /// The member's account id (random).
    pub account: AccountId,
    /// The member's leaf in the Party group: members are numbered in the order added.
    pub leaf: LeafIndex,
}

/// A Spot of a Party.
#[derive(Clone, Debug, PartialEq)]
pub struct SpotFixture {
    /// The Spot id (random).
    pub id: SpotId,
    /// A readable name, for example `anchor`.
    pub name: String,
    /// The fence.
    pub fence: Fence,
}

impl SpotFixture {
    /// The accuracy of the fixes these helpers build, 10 m: well inside every entry limit
    /// (`max(50 m, r/2)`, design F.2).
    #[must_use]
    pub fn fix_accuracy() -> Meters {
        Meters::from_whole(10)
    }

    /// How far beyond the exit buffer [`fix_outside`](Self::fix_outside) places its fix, 10 m,
    /// so that rounding cannot put it back on the edge.
    #[must_use]
    pub fn outside_margin() -> Meters {
        Meters::from_whole(10)
    }

    /// A fix at the centre of the Spot's enclosing circle, taken at `at`.
    ///
    /// For a circle and for any convex polygon this is inside the Spot. For a concave polygon
    /// the centre may fall outside; build that fix with [`fix_at_distance`](Self::fix_at_distance)
    /// or by hand.
    #[must_use]
    pub fn fix_inside(&self, at: UnixSeconds) -> Fix {
        Fix {
            coordinate: self.fence.enclosing_circle().center(),
            accuracy: Self::fix_accuracy(),
            taken_at: at,
        }
    }

    /// A fix due north of the centre, beyond the enclosing circle by the exit buffer
    /// (`max(50 m, r/4)`, design F.2) plus [`OUTSIDE_MARGIN`](Self::outside_margin()): far enough
    /// to confirm an exit, for any fence shape.
    ///
    /// # Errors
    ///
    /// [`FixtureError::Geo`] if the point would lie beyond the North Pole.
    pub fn fix_outside(&self, at: UnixSeconds) -> Result<Fix, FixtureError> {
        let radius = self.fence.enclosing_circle().radius();
        let distance = radius
            .saturating_add(exit_buffer(radius))
            .saturating_add(Self::outside_margin());
        self.fix_at_distance(distance, at)
    }

    /// A fix `distance` due north of the centre of the enclosing circle.
    ///
    /// # Errors
    ///
    /// [`FixtureError::Geo`] if the point would lie beyond the North Pole.
    pub fn fix_at_distance(&self, distance: Meters, at: UnixSeconds) -> Result<Fix, FixtureError> {
        let center = self.fence.enclosing_circle().center();
        let latitude = center.latitude() + (distance.get() / EARTH_RADIUS_METERS).to_degrees();
        Ok(Fix {
            coordinate: Coordinate::new(latitude, center.longitude())?,
            accuracy: Self::fix_accuracy(),
            taken_at: at,
        })
    }
}

/// A Party with its members and Spots.
#[derive(Clone, Debug, PartialEq)]
pub struct PartyFixture {
    /// The Party id (random).
    pub id: PartyId,
    /// Members in leaf order.
    pub members: Vec<MemberFixture>,
    /// Spots in the order added.
    pub spots: Vec<SpotFixture>,
}

impl PartyFixture {
    /// The member called `name`.
    ///
    /// # Errors
    ///
    /// [`FixtureError::UnknownMember`].
    pub fn member(&self, name: &str) -> Result<&MemberFixture, FixtureError> {
        self.members
            .iter()
            .find(|member| member.name == name)
            .ok_or_else(|| FixtureError::UnknownMember(name.to_owned()))
    }

    /// The Spot called `name`.
    ///
    /// # Errors
    ///
    /// [`FixtureError::UnknownSpot`].
    pub fn spot(&self, name: &str) -> Result<&SpotFixture, FixtureError> {
        self.spots
            .iter()
            .find(|spot| spot.name == name)
            .ok_or_else(|| FixtureError::UnknownSpot(name.to_owned()))
    }
}

/// Builds a [`PartyFixture`]. Ids come from the RNG passed to [`build`](Self::build), so a
/// seeded RNG gives the same Party every run.
///
/// ```
/// # use pingers_testkit::{PartyBuilder, FixtureError, seeded_rng, TEST_START};
/// # use pingers_geo::{Coordinate, Meters};
/// let party = PartyBuilder::new()
///     .members(3)
///     .circle_spot("anchor", Coordinate::new(51.5007, -0.1246)?, Meters::from_whole(150))?
///     .build(&mut seeded_rng(1))?;
/// let alice_arrives = party.spot("anchor")?.fix_inside(TEST_START);
/// # Ok::<(), FixtureError>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct PartyBuilder {
    members: Vec<String>,
    spots: Vec<(String, Fence)>,
}

impl PartyBuilder {
    /// No members and no Spots yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a member called `name`.
    #[must_use]
    pub fn member(mut self, name: &str) -> Self {
        self.members.push(name.to_owned());
        self
    }

    /// Adds `count` members named alice, bob, carol, ... by position in the Party (so after
    /// one member, the next default name is `bob`); past the eighth, `member-<n>`.
    #[must_use]
    pub fn members(mut self, count: usize) -> Self {
        for _ in 0..count {
            let position = self.members.len();
            let name = DEFAULT_NAMES
                .get(position)
                .map_or_else(|| format!("member-{position}"), |name| (*name).to_owned());
            self.members.push(name);
        }
        self
    }

    /// Adds a Spot with any fence.
    #[must_use]
    pub fn spot(mut self, name: &str, fence: Fence) -> Self {
        self.spots.push((name.to_owned(), fence));
        self
    }

    /// Adds a circular Spot.
    ///
    /// # Errors
    ///
    /// [`FixtureError::Geo`] for a zero radius.
    pub fn circle_spot(
        self,
        name: &str,
        center: Coordinate,
        radius: Meters,
    ) -> Result<Self, FixtureError> {
        let circle = Circle::new(center, radius)?;
        Ok(self.spot(name, Fence::Circle(circle)))
    }

    /// The Party, with random ids from `rng`.
    ///
    /// # Errors
    ///
    /// [`FixtureError::NoMembers`], [`FixtureError::DuplicateMember`],
    /// [`FixtureError::DuplicateSpot`] or [`FixtureError::TooManyMembers`].
    pub fn build(self, rng: &mut impl rand_core::CryptoRng) -> Result<PartyFixture, FixtureError> {
        if self.members.is_empty() {
            return Err(FixtureError::NoMembers);
        }
        if let Some(name) = first_duplicate(self.members.iter()) {
            return Err(FixtureError::DuplicateMember(name));
        }
        if let Some(name) = first_duplicate(self.spots.iter().map(|(name, _fence)| name)) {
            return Err(FixtureError::DuplicateSpot(name));
        }
        let id = PartyId::from_bytes(random_array(rng));
        let members = self
            .members
            .into_iter()
            .enumerate()
            .map(|(position, name)| {
                let leaf = u32::try_from(position).map_err(|_| FixtureError::TooManyMembers)?;
                Ok(MemberFixture {
                    name,
                    account: AccountId::from_bytes(random_array(rng)),
                    leaf: LeafIndex::new(leaf),
                })
            })
            .collect::<Result<Vec<_>, FixtureError>>()?;
        let spots = self
            .spots
            .into_iter()
            .map(|(name, fence)| SpotFixture {
                id: SpotId::from_bytes(random_array(rng)),
                name,
                fence,
            })
            .collect();
        Ok(PartyFixture { id, members, spots })
    }
}

fn first_duplicate<'name>(names: impl Iterator<Item = &'name String>) -> Option<String> {
    let mut seen = BTreeSet::new();
    names
        .into_iter()
        .find(|name| !seen.insert(name.as_str()))
        .cloned()
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_geo::{distance, entry_accuracy_limit};

    use super::*;
    use crate::{TEST_START, seeded_rng};

    fn anchor_party(seed: u64) -> Result<PartyFixture, FixtureError> {
        PartyBuilder::new()
            .members(3)
            .circle_spot(
                "anchor",
                Coordinate::new(51.5007, -0.1246)?,
                Meters::from_whole(150),
            )?
            .build(&mut seeded_rng(seed))
    }

    #[test]
    fn members_get_default_names_and_leaves_in_order() -> Result<(), FixtureError> {
        let party = anchor_party(1)?;
        let names: Vec<&str> = party
            .members
            .iter()
            .map(|member| member.name.as_str())
            .collect();
        assert_eq!(names, ["alice", "bob", "carol"]);
        let leaves: Vec<u32> = party
            .members
            .iter()
            .map(|member| member.leaf.get())
            .collect();
        assert_eq!(leaves, [0, 1, 2]);
        Ok(())
    }

    #[test]
    fn default_names_continue_after_named_members_and_run_out_gracefully()
    -> Result<(), FixtureError> {
        let party = PartyBuilder::new()
            .member("zed")
            .members(9)
            .build(&mut seeded_rng(1))?;
        assert_eq!(party.member("zed")?.leaf, LeafIndex::new(0));
        assert_eq!(party.member("bob")?.leaf, LeafIndex::new(1));
        assert_eq!(party.member("heidi")?.leaf, LeafIndex::new(7));
        assert_eq!(party.member("member-8")?.leaf, LeafIndex::new(8));
        assert_eq!(party.member("member-9")?.leaf, LeafIndex::new(9));
        Ok(())
    }

    #[test]
    fn same_seed_builds_the_same_party_and_ids_are_distinct() -> Result<(), FixtureError> {
        let first = anchor_party(5)?;
        assert_eq!(first, anchor_party(5)?);
        assert_ne!(first.id, anchor_party(6)?.id);
        let accounts: BTreeSet<_> = first.members.iter().map(|member| member.account).collect();
        assert_eq!(accounts.len(), 3);
        Ok(())
    }

    #[test]
    fn a_party_needs_members() {
        assert_eq!(
            PartyBuilder::new().build(&mut seeded_rng(1)),
            Err(FixtureError::NoMembers)
        );
    }

    #[test]
    fn duplicate_names_are_rejected() -> Result<(), FixtureError> {
        assert_eq!(
            PartyBuilder::new()
                .member("alice")
                .members(1)
                .member("bob")
                .build(&mut seeded_rng(1)),
            Err(FixtureError::DuplicateMember(String::from("bob")))
        );
        assert_eq!(
            PartyBuilder::new()
                .members(1)
                .circle_spot("home", Coordinate::new(0.0, 0.0)?, Meters::from_whole(100))?
                .circle_spot("home", Coordinate::new(1.0, 1.0)?, Meters::from_whole(100))?
                .build(&mut seeded_rng(1)),
            Err(FixtureError::DuplicateSpot(String::from("home")))
        );
        Ok(())
    }

    #[test]
    fn unknown_names_are_reported() -> Result<(), FixtureError> {
        let party = anchor_party(1)?;
        assert_eq!(
            party.member("mallory"),
            Err(FixtureError::UnknownMember(String::from("mallory")))
        );
        assert_eq!(
            party.spot("pub"),
            Err(FixtureError::UnknownSpot(String::from("pub")))
        );
        Ok(())
    }

    #[test]
    fn a_zero_radius_spot_is_a_geo_error() -> Result<(), FixtureError> {
        assert_eq!(
            PartyBuilder::new()
                .circle_spot("nowhere", Coordinate::new(0.0, 0.0)?, Meters::ZERO)
                .map(|_builder| ()),
            Err(FixtureError::Geo(GeoError::ZeroRadius))
        );
        Ok(())
    }

    #[test]
    fn fix_inside_is_inside_and_accurate_enough_to_enter() -> Result<(), FixtureError> {
        let party = anchor_party(1)?;
        let spot = party.spot("anchor")?;
        let fix = spot.fix_inside(TEST_START);
        assert!(spot.fence.contains(fix.coordinate));
        assert!(fix.accuracy <= entry_accuracy_limit(Meters::from_whole(150)));
        assert_eq!(fix.taken_at, TEST_START);
        Ok(())
    }

    #[test]
    fn fix_outside_is_beyond_the_exit_buffer() -> Result<(), FixtureError> {
        let party = anchor_party(1)?;
        let spot = party.spot("anchor")?;
        let fix = spot.fix_outside(TEST_START)?;
        let circle = spot.fence.enclosing_circle();
        let beyond = distance(circle.center(), fix.coordinate).get() - circle.radius().get();
        // 150 m radius: exit buffer max(50, 37.5) = 50 m, plus the 10 m margin.
        assert!((beyond - 60.0).abs() < 0.01, "beyond = {beyond}");
        assert_eq!(fix.taken_at, TEST_START);
        Ok(())
    }

    #[test]
    fn fix_at_distance_is_that_far_from_the_centre() -> Result<(), FixtureError> {
        let party = anchor_party(1)?;
        let spot = party.spot("anchor")?;
        let fix = spot.fix_at_distance(Meters::from_whole(1_000), TEST_START)?;
        let measured = distance(spot.fence.enclosing_circle().center(), fix.coordinate).get();
        assert!((measured - 1_000.0).abs() < 0.01, "measured = {measured}");
        Ok(())
    }

    #[test]
    fn fix_beyond_the_pole_is_an_error() -> Result<(), FixtureError> {
        let party = PartyBuilder::new()
            .members(1)
            .circle_spot(
                "pole",
                Coordinate::new(89.9999, 0.0)?,
                Meters::from_whole(100),
            )?
            .build(&mut seeded_rng(1))?;
        assert_eq!(
            party.spot("pole")?.fix_outside(TEST_START),
            Err(FixtureError::Geo(GeoError::LatitudeOutOfRange))
        );
        Ok(())
    }
}
