// SPDX-License-Identifier: AGPL-3.0-only
//! Exclusion zones: where this device never shares, stored only on it.

use pingers_geo::{Coordinate, Fence, Meters};

use crate::ScreenError;
use crate::clock::LocalTime;
use crate::common::Name;
use crate::snapshot::{
    ClientSnapshot, Days, ExclusionZone, HideTrail, ZoneId, ZoneParties, ZoneTimes, daily_window,
};

/// A zone's shape as its row describes it.
#[derive(Clone, Debug, PartialEq)]
pub enum ZoneShape {
    /// A circle of this radius.
    Circle(Meters),
    /// A drawn area, its vertices in order (not repeating the first), so the map draws it
    /// exactly.
    Area(Vec<Coordinate>),
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
                Fence::Polygon(polygon) => ZoneShape::Area(polygon.vertices().to_vec()),
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

/// When a zone applies, as the user picked it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneTimesDraft {
    /// All the time.
    Always,
    /// On these days, from one time to another (past midnight if `to` is earlier).
    Window {
        /// Which days.
        days: Days,
        /// Where the window starts.
        from: LocalTime,
        /// Where it ends.
        to: LocalTime,
    },
}

/// Every exclusion zone, with `zone`'s times replaced by `times`: the whole list, as
/// pingers-client stores it.
///
/// # Errors
///
/// [`ScreenError::UnknownZone`]; [`ScreenError::EmptyWindow`] for a window that starts where it
/// ends; [`ScreenError::InvalidTime`] for a time outside the day.
pub fn with_times(
    snapshot: &ClientSnapshot,
    zone: ZoneId,
    times: ZoneTimesDraft,
) -> Result<Vec<ExclusionZone>, ScreenError> {
    let times = match times {
        ZoneTimesDraft::Always => ZoneTimes::Always,
        ZoneTimesDraft::Window { days, from, to } => ZoneTimes::Window {
            days,
            window: daily_window(from.minute_of_day()?, to.minute_of_day()?)?,
        },
    };
    let mut zones = snapshot.zones.clone();
    zones
        .iter_mut()
        .find(|candidate| candidate.id == zone)
        .ok_or(ScreenError::UnknownZone)?
        .schedule
        .times = times;
    Ok(zones)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};
    use crate::snapshot::ZoneSchedule;

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
        // A drawn area carries its own vertices, so the map draws the zone the user drew.
        let drawn = match snapshot.zones.get(1).map(|zone| &zone.fence) {
            Some(Fence::Polygon(polygon)) => polygon.vertices().to_vec(),
            _ => return Err(FixtureError::Missing),
        };
        assert!(drawn.len() >= 3);
        assert_eq!(work.shape, ZoneShape::Area(drawn));
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

    #[test]
    fn new_times_replace_only_that_zones_times() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let draft = ZoneTimesDraft::Window {
            days: Days::Weekends,
            from: LocalTime::new(9, 0)?,
            to: LocalTime::new(17, 0)?,
        };
        let zones = with_times(&snapshot, ZoneId::new(1), draft)?;
        let [home, work] = zones.as_slice() else {
            return Err(FixtureError::Missing);
        };
        let ZoneTimes::Window { days, window } = home.schedule.times else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(days, Days::Weekends);
        assert_eq!((window.start().get(), window.end().get()), (540, 1_020));
        assert_eq!(home.schedule.parties, ZoneParties::All);
        let before = snapshot.zones.first().ok_or(FixtureError::Missing)?;
        assert_eq!((&home.name, &home.fence), (&before.name, &before.fence));
        assert_eq!(Some(work), snapshot.zones.get(1));

        let always = with_times(&snapshot, ZoneId::new(2), ZoneTimesDraft::Always)?;
        assert_eq!(
            always.get(1).map(|zone| zone.schedule.times),
            Some(ZoneTimes::Always)
        );
        assert_eq!(always.first(), snapshot.zones.first());
        Ok(())
    }

    #[test]
    fn times_for_a_missing_zone_or_an_empty_window_are_refused() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        assert_eq!(
            with_times(&snapshot, ZoneId::new(9), ZoneTimesDraft::Always),
            Err(ScreenError::UnknownZone)
        );
        let nine = LocalTime::new(9, 0)?;
        let empty = ZoneTimesDraft::Window {
            days: Days::EveryDay,
            from: nine,
            to: nine,
        };
        assert_eq!(
            with_times(&snapshot, ZoneId::new(1), empty),
            Err(ScreenError::EmptyWindow)
        );
        Ok(())
    }
}
