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
            self.last_in_zone = Some(fix.taken_at);
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
        Ok(())
    }
}
