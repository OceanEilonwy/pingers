// SPDX-License-Identifier: AGPL-3.0-only
//! Exclusion zones: where this device never shares, stored only on it.

use pingers_geo::{Coordinate, Fence, MINIMUM_SPOT_RADIUS, Meters, SquareMeters};

use crate::ScreenError;
use crate::clock::LocalTime;
use crate::common::{DraftText, Name};
use crate::snapshot::{
    ClientSnapshot, Days, ExclusionZone, HideTrail, ZoneId, ZoneParties, ZoneSchedule, ZoneTimes,
    daily_window,
};
use crate::spot::{ShapeFields, ShapeKind, SpotDraft, maximum_spot_radius};

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

/// The Zone times editor (canvas `ZoneTimes`), opened from a zone row.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneTimesEditor {
    /// The zone, its shape and who it hides this device from.
    pub zone: ZoneRow,
    /// The times picked so far.
    pub draft: ZoneTimesDraft,
    /// Why "Save" is off: a window that starts where it ends.
    pub problem: Option<ScreenError>,
}

impl ZoneTimesDraft {
    /// The draft a zone's stored times start the editor with.
    #[must_use]
    pub fn of(times: ZoneTimes) -> Self {
        match times {
            ZoneTimes::Always => Self::Always,
            ZoneTimes::Window { days, window } => Self::Window {
                days,
                from: LocalTime::from_minute_of_day(window.start()),
                to: LocalTime::from_minute_of_day(window.end()),
            },
        }
    }
}

/// The Zone times editor for `zone`, with `draft` picked so far, or the zone's stored times.
///
/// # Errors
///
/// [`ScreenError::UnknownZone`].
pub fn zone_times_editor(
    snapshot: &ClientSnapshot,
    zone: ZoneId,
    draft: Option<ZoneTimesDraft>,
) -> Result<ZoneTimesEditor, ScreenError> {
    let row = exclusion_zones(snapshot)
        .zones
        .into_iter()
        .find(|row| row.id == zone)
        .ok_or(ScreenError::UnknownZone)?;
    let draft = draft.unwrap_or_else(|| ZoneTimesDraft::of(row.times));
    let problem = match draft {
        ZoneTimesDraft::Window { from, to, .. } if from == to => Some(ScreenError::EmptyWindow),
        ZoneTimesDraft::Window { .. } | ZoneTimesDraft::Always => None,
    };
    Ok(ZoneTimesEditor {
        zone: row,
        draft,
        problem,
    })
}

/// Every exclusion zone but `zone` ("Delete zone"): the whole list, as pingers-client stores
/// it.
///
/// # Errors
///
/// [`ScreenError::UnknownZone`].
pub fn without(snapshot: &ClientSnapshot, zone: ZoneId) -> Result<Vec<ExclusionZone>, ScreenError> {
    if !snapshot.zones.iter().any(|candidate| candidate.id == zone) {
        return Err(ScreenError::UnknownZone);
    }
    Ok(snapshot
        .zones
        .iter()
        .filter(|candidate| candidate.id != zone)
        .cloned()
        .collect())
}

/// The editor for a new exclusion zone ("Add exclusion zone"): the Spot editor's name and shape,
/// for no Party, since a zone never leaves this device.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneEditor {
    /// The name typed; empty shows the "New zone" placeholder.
    pub name: DraftText,
    /// Which shape is chosen.
    pub shape: ShapeKind,
    /// The radius, within the slider's range.
    pub radius: Meters,
    /// The slider's lower end.
    pub min_radius: Meters,
    /// The slider's upper end.
    pub max_radius: Meters,
    /// The drawn area's size, once drawn.
    pub area: Option<SquareMeters>,
    /// Whether "Save zone" is enabled: a name, and an area if Area is chosen.
    pub can_save: bool,
}

/// The zone editor, for what has been typed and drawn so far.
#[must_use]
pub fn zone_editor(draft: &SpotDraft) -> ZoneEditor {
    let shape = ShapeFields::of(draft);
    ZoneEditor {
        name: draft.name.clone(),
        shape: draft.shape,
        radius: shape.radius,
        min_radius: MINIMUM_SPOT_RADIUS,
        max_radius: maximum_spot_radius(),
        area: shape.area,
        can_save: shape.ready && draft.name.name().is_some(),
    }
}

/// Every exclusion zone and a new one, `name` shaped as `fence`, applying always and for every
/// Party until its times are changed: the whole list, as pingers-client stores it, and the new
/// zone's id, one past the largest in use.
///
/// # Errors
///
/// [`ScreenError::TooManyZones`] once every id is taken.
pub fn with_new(
    snapshot: &ClientSnapshot,
    name: Name,
    fence: Fence,
) -> Result<(Vec<ExclusionZone>, ZoneId), ScreenError> {
    let id = match snapshot.zones.iter().map(|zone| zone.id.get()).max() {
        None => 1,
        Some(largest) => largest.checked_add(1).ok_or(ScreenError::TooManyZones)?,
    };
    let id = ZoneId::new(id);
    let mut zones = snapshot.zones.clone();
    zones.push(ExclusionZone {
        id,
        name,
        fence,
        schedule: ZoneSchedule::ALWAYS,
    });
    Ok((zones, id))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};
    use pingers_geo::{Circle, Polygon};

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

    #[test]
    fn the_editor_starts_from_the_stored_times_and_flags_equal_ones() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let work = zone_times_editor(&snapshot, ZoneId::new(2), None)?;
        assert_eq!(work.zone.name.as_str(), "Work");
        assert_eq!(
            work.draft,
            ZoneTimesDraft::Window {
                days: Days::Weekdays,
                from: LocalTime::new(8, 0)?,
                to: LocalTime::new(18, 0)?
            }
        );
        assert_eq!(work.problem, None);
        let eight = LocalTime::new(8, 0)?;
        let equal = ZoneTimesDraft::Window {
            days: Days::Weekends,
            from: eight,
            to: eight,
        };
        let flagged = zone_times_editor(&snapshot, ZoneId::new(2), Some(equal))?;
        assert_eq!(flagged.problem, Some(ScreenError::EmptyWindow));
        let home = zone_times_editor(&snapshot, ZoneId::new(1), None)?;
        assert_eq!(home.draft, ZoneTimesDraft::Always);
        assert_eq!(
            zone_times_editor(&snapshot, ZoneId::new(9), None).map(|editor| editor.draft),
            Err(ScreenError::UnknownZone)
        );
        Ok(())
    }

    #[test]
    fn deleting_a_zone_keeps_the_others() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let left = without(&snapshot, ZoneId::new(1))?;
        let names: Vec<_> = left.iter().map(|zone| zone.name.as_str()).collect();
        assert_eq!(names, ["Work"]);
        assert_eq!(
            without(&snapshot, ZoneId::new(9)),
            Err(ScreenError::UnknownZone)
        );
        Ok(())
    }
    #[test]
    fn a_new_zone_joins_the_list_after_the_others() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = fixtures::snapshot()?;
        let centre = Coordinate::new(51.5, -0.12)?;
        let fence = Fence::Circle(Circle::new(centre, Meters::from_whole(200))?);
        let (zones, id) = with_new(&snapshot, Name::new("Gym")?, fence.clone())?;
        assert_eq!(id, ZoneId::new(3));
        let names: Vec<_> = zones.iter().map(|zone| zone.name.as_str()).collect();
        assert_eq!(names, ["Home", "Work", "Gym"]);
        let gym = zones.last().ok_or(FixtureError::Missing)?;
        assert_eq!(
            (gym.id, &gym.fence, &gym.schedule),
            (id, &fence, &ZoneSchedule::ALWAYS)
        );
        let mut empty = snapshot.clone();
        empty.zones.clear();
        assert_eq!(
            with_new(&empty, Name::new("Gym")?, fence.clone())?.1,
            ZoneId::new(1)
        );
        let mut full = snapshot;
        if let Some(last) = full.zones.last_mut() {
            last.id = ZoneId::new(u32::MAX);
        }
        assert_eq!(
            with_new(&full, Name::new("Gym")?, fence).map(|(_, id)| id),
            Err(ScreenError::TooManyZones)
        );
        Ok(())
    }

    #[test]
    fn the_zone_editor_wants_a_name_and_a_finished_shape() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut draft = SpotDraft {
            name: DraftText::new("Gym".into()),
            shape: ShapeKind::Circle,
            radius: Meters::from_whole(5000),
            area: None,
        };
        let view = zone_editor(&draft);
        assert!(view.can_save);
        assert_eq!(view.radius, maximum_spot_radius());
        assert_eq!(view.min_radius, MINIMUM_SPOT_RADIUS);
        draft.shape = ShapeKind::Area;
        assert!(!zone_editor(&draft).can_save);
        draft.area = Some(Polygon::new(vec![
            Coordinate::new(51.5, -0.12)?,
            Coordinate::new(51.501, -0.12)?,
            Coordinate::new(51.501, -0.119)?,
        ])?);
        let view = zone_editor(&draft);
        assert!(view.can_save && view.area.is_some());
        draft.name = DraftText::default();
        assert!(!zone_editor(&draft).can_save);
        Ok(())
    }
}
