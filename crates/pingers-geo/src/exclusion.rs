// SPDX-License-Identifier: AGPL-3.0-only
//! Exclusion zones and their trail buffer (design B.11).

use core::time::Duration;

use pingers_time::UnixSeconds;

use crate::{Fence, Fix};

/// Whether exclusion zones suppress sharing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneStatus {
    /// No zone applies.
    Clear,
    /// The latest fix's accuracy circle intersects a zone.
    InsideZone,
    /// The device left a zone recently; sharing stays suppressed until `until` so the trail
    /// away from the zone is not revealed.
    TrailBuffer {
        /// When the buffer ends.
        until: UnixSeconds,
    },
}

impl ZoneStatus {
    /// Whether this status suppresses sharing.
    #[must_use]
    pub const fn suppresses(self) -> bool {
        !matches!(self, Self::Clear)
    }
}

/// Tracks the last time the device was in an exclusion zone, to apply the trail buffer.
///
/// Zones themselves are local-only data passed in on every call; this holds only a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExclusionTrail {
    last_in_zone: Option<UnixSeconds>,
}

impl ExclusionTrail {
    /// How long sharing stays suppressed after the last fix that touched a zone.
    pub const TRAIL_BUFFER: Duration = Duration::from_mins(5);

    /// A trail that has never been in a zone.
    #[must_use]
    pub const fn new() -> Self {
        Self { last_in_zone: None }
    }

    /// Restores a trail from the stored time of the last fix in a zone.
    #[must_use]
    pub const fn restore(last_in_zone: Option<UnixSeconds>) -> Self {
        Self { last_in_zone }
    }

    /// The time to store.
    #[must_use]
    pub const fn last_in_zone(&self) -> Option<UnixSeconds> {
        self.last_in_zone
    }

    /// Checks `fix` against `zones` and records it if it touches one.
    pub fn observe(&mut self, zones: &[Fence], fix: &Fix) -> ZoneStatus {
        let touches = zones
            .iter()
            .any(|zone| zone.intersects_disc(fix.coordinate, fix.accuracy));
        if touches {
            // A late-delivered older fix must not shorten the buffer set by a newer one.
            let latest = self
                .last_in_zone
                .map_or(fix.taken_at, |last| last.max(fix.taken_at));
            self.last_in_zone = Some(latest);
            ZoneStatus::InsideZone
        } else {
            self.status_at(fix.taken_at)
        }
    }

    /// The status at `now` without a new fix: the trail buffer, or clear.
    #[must_use]
    pub fn status_at(&self, now: UnixSeconds) -> ZoneStatus {
        let until = self
            .last_in_zone
            .and_then(|last| last.checked_add(Self::TRAIL_BUFFER));
        match until {
            Some(until) if now < until => ZoneStatus::TrailBuffer { until },
            Some(_) | None => ZoneStatus::Clear,
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::{Circle, Coordinate, GeoError, Meters};

    fn home() -> Result<Fence, GeoError> {
        Ok(Fence::Circle(Circle::new(
            Coordinate::new(51.5, -0.1)?,
            Meters::from_whole(100),
        )?))
    }

    fn fix_at(latitude: f64, seconds: u64) -> Result<Fix, GeoError> {
        Ok(Fix {
            coordinate: Coordinate::new(latitude, -0.1)?,
            accuracy: Meters::from_whole(20),
            taken_at: UnixSeconds::new(seconds),
        })
    }

    #[test]
    fn leaving_a_zone_keeps_sharing_suppressed_for_five_minutes() -> Result<(), GeoError> {
        let zones = [home()?];
        let mut trail = ExclusionTrail::new();
        assert_eq!(
            trail.observe(&zones, &fix_at(51.5, 1_000)?),
            ZoneStatus::InsideZone
        );
        let away = fix_at(51.51, 1_100)?;
        assert_eq!(
            trail.observe(&zones, &away),
            ZoneStatus::TrailBuffer {
                until: UnixSeconds::new(1_300)
            }
        );
        assert_eq!(trail.status_at(UnixSeconds::new(1_300)), ZoneStatus::Clear);
        Ok(())
    }

    #[test]
    fn a_trail_that_never_touched_a_zone_is_clear() -> Result<(), GeoError> {
        let mut trail = ExclusionTrail::new();
        assert_eq!(
            trail.observe(&[home()?], &fix_at(51.51, 10)?),
            ZoneStatus::Clear
        );
        assert_eq!(trail.last_in_zone(), None);
        Ok(())
    }

    #[test]
    fn only_clear_lets_sharing_through() {
        assert!(!ZoneStatus::Clear.suppresses());
        assert!(ZoneStatus::InsideZone.suppresses());
        assert!(
            ZoneStatus::TrailBuffer {
                until: UnixSeconds::new(1)
            }
            .suppresses()
        );
    }

    #[test]
    fn an_accuracy_circle_reaching_into_a_zone_counts_as_inside() -> Result<(), GeoError> {
        // 0.0011° north is about 122 m from the centre: 22 m outside the 100 m zone, so the
        // 20 m accuracy circle misses it and a 30 m one reaches it.
        let mut trail = ExclusionTrail::new();
        let mut fix = fix_at(51.5011, 10)?;
        assert_eq!(trail.observe(&[home()?], &fix), ZoneStatus::Clear);
        fix.accuracy = Meters::from_whole(30);
        assert_eq!(trail.observe(&[home()?], &fix), ZoneStatus::InsideZone);
        Ok(())
    }

    #[test]
    fn any_one_of_several_zones_suppresses() -> Result<(), GeoError> {
        let elsewhere = Fence::Circle(Circle::new(
            Coordinate::new(48.85, 2.35)?,
            Meters::from_whole(100),
        )?);
        let mut trail = ExclusionTrail::new();
        assert_eq!(
            trail.observe(&[elsewhere, home()?], &fix_at(51.5, 10)?),
            ZoneStatus::InsideZone
        );
        assert_eq!(trail.last_in_zone(), Some(UnixSeconds::new(10)));
        Ok(())
    }

    #[test]
    fn a_polygon_zone_is_checked_against_its_edges() -> Result<(), GeoError> {
        let square = crate::testing::polygon_around(
            Coordinate::new(51.5, -0.1)?,
            &[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]],
        )?;
        let zones = [Fence::Polygon(square)];
        let mut trail = ExclusionTrail::new();
        // About 11 m south of the square's southern edge.
        let south = fix_at(51.4999, 10)?;
        assert_eq!(trail.observe(&zones, &south), ZoneStatus::InsideZone);
        let farther = fix_at(51.4995, 20)?;
        assert_eq!(
            trail.observe(&zones, &farther),
            ZoneStatus::TrailBuffer {
                until: UnixSeconds::new(310)
            }
        );
        Ok(())
    }

    #[test]
    fn the_buffer_runs_from_the_latest_fix_in_a_zone() -> Result<(), GeoError> {
        let zones = [home()?];
        let mut trail = ExclusionTrail::new();
        let _ = trail.observe(&zones, &fix_at(51.5, 1_000)?);
        let _ = trail.observe(&zones, &fix_at(51.5, 1_200)?);
        // A fix from before, delivered late, does not pull the buffer back.
        let _ = trail.observe(&zones, &fix_at(51.5, 900)?);
        assert_eq!(trail.last_in_zone(), Some(UnixSeconds::new(1_200)));
        assert_eq!(
            trail.status_at(UnixSeconds::new(1_499)),
            ZoneStatus::TrailBuffer {
                until: UnixSeconds::new(1_500)
            }
        );
        Ok(())
    }

    #[test]
    fn a_restored_trail_keeps_its_buffer() {
        let trail = ExclusionTrail::restore(Some(UnixSeconds::new(1_000)));
        assert_eq!(trail.last_in_zone(), Some(UnixSeconds::new(1_000)));
        assert_eq!(
            trail.status_at(UnixSeconds::new(1_299)),
            ZoneStatus::TrailBuffer {
                until: UnixSeconds::new(1_300)
            }
        );
        assert_eq!(ExclusionTrail::restore(None), ExclusionTrail::new());
    }
}
