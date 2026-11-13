// SPDX-License-Identifier: AGPL-3.0-only
//! Exclusion zones: where this device never shares, stored only on it.

use pingers_geo::{Coordinate, Fence, Meters};

use crate::common::Name;
use crate::snapshot::{ClientSnapshot, HideTrail, ZoneId, ZoneParties, ZoneTimes};

/// A zone's shape as its row describes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZoneShape {
    /// A circle of this radius.
    Circle(Meters),
    /// A drawn area.
    Area,
}

/// Which Parties a zone hides this device from, by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZoneAppliesTo {
    /// Every Party.
    AllParties,
    /// Only these (Parties this device has since left are not named).
    Parties(Vec<Name>),
}

/// A zone in the list and on the map.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneRow {
    /// The zone.
    pub id: ZoneId,
    /// Its name.
    pub name: Name,
    /// Its shape, for the row.
    pub shape: ZoneShape,
    /// Its fence, for the map.
    pub fence: Fence,
    /// When it applies.
    pub times: ZoneTimes,
    /// For whom.
    pub applies_to: ZoneAppliesTo,
    /// Where its label sits on the map: the centre of the circle around it.
    pub label_at: Coordinate,
}

/// The Exclusion zones screen.
#[derive(Clone, Debug, PartialEq)]
pub struct ExclusionZones {
    /// The zones.
    pub zones: Vec<ZoneRow>,
    /// "Hide trail near zones".
    pub hide_trail: HideTrail,
    /// Where this device is, for the map's dot.
    pub me: Option<Coordinate>,
}

/// The Exclusion zones screen.
#[must_use]
pub fn exclusion_zones(snapshot: &ClientSnapshot) -> ExclusionZones {
    let zones = snapshot
        .zones
        .iter()
        .map(|zone| ZoneRow {
            id: zone.id,
            name: zone.name.clone(),
            shape: match &zone.fence {
                Fence::Circle(circle) => ZoneShape::Circle(circle.radius()),
                Fence::Polygon(_) => ZoneShape::Area,
            },
            fence: zone.fence.clone(),
            times: zone.schedule.times,
            applies_to: match &zone.schedule.parties {
                ZoneParties::All => ZoneAppliesTo::AllParties,
                ZoneParties::Only(ids) => ZoneAppliesTo::Parties(
                    snapshot
                        .parties
                        .iter()
                        .filter(|party| ids.contains(&party.id))
                        .map(|party| party.name.clone())
                        .collect(),
                ),
            },
            label_at: zone.fence.enclosing_circle().center(),
        })
        .collect();
    ExclusionZones {
        zones,
        hide_trail: snapshot.settings.hide_trail,
        me: snapshot.own_coordinate(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};
    use crate::snapshot::{Days, ZoneSchedule};

    #[test]
    fn zones_read_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = exclusion_zones(&snapshot);
        let [home, work] = view.zones.as_slice() else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(home.name.as_str(), "Home");
        assert_eq!((home.id.get(), work.id.get()), (1, 2));
        assert_eq!(home.shape, ZoneShape::Circle(Meters::from_whole(150)));
        assert_eq!(home.times, ZoneTimes::Always);
        assert_eq!(home.applies_to, ZoneAppliesTo::AllParties);
        assert_eq!(
            Some(home.label_at),
            snapshot
                .zones
                .first()
                .map(|zone| zone.fence.enclosing_circle().center())
        );
        assert_eq!(work.shape, ZoneShape::Area);
        assert!(matches!(
            work.times,
            ZoneTimes::Window {
                days: Days::Weekdays,
                ..
            }
        ));
        assert_eq!(view.hide_trail, HideTrail::On);
        assert_eq!(view.me, Some(fixtures::sam_position()?));
        Ok(())
    }

    #[test]
    fn a_zone_for_some_parties_names_those_still_joined() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        if let Some(zone) = snapshot.zones.first_mut() {
            zone.schedule = ZoneSchedule {
                times: ZoneTimes::Always,
                parties: ZoneParties::Only(vec![
                    fixtures::uni_flat(),
                    pingers_proto::PartyId::from_bytes([99; 16]),
                ]),
            };
        }
        let view = exclusion_zones(&snapshot);
        let applies = view.zones.first().map(|zone| zone.applies_to.clone());
        let ZoneAppliesTo::Parties(names) = applies.ok_or(FixtureError::Missing)? else {
            return Err(FixtureError::Missing);
        };
        let names: Vec<_> = names.iter().map(Name::as_str).collect();
        assert_eq!(names, ["Uni flat"]);
        Ok(())
    }
}
