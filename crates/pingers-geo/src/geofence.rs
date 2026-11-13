// SPDX-License-Identifier: AGPL-3.0-only
//! Choosing which Spots to register as OS geofences (design E.2).

use crate::{Circle, Coordinate, Fence, distance, exit_buffer};

/// How many Spot regions the platform lets the app register, leaving room for the horizon
/// region and a spare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeofenceCapacity(usize);

impl GeofenceCapacity {
    /// Android allows 100 geofences per app: 97 Spots, the horizon region, one spare.
    pub const ANDROID: Self = Self(97);

    /// A custom capacity (for tests and future platforms).
    #[must_use]
    pub const fn new(spots: usize) -> Self {
        Self(spots)
    }

    /// The number of Spot regions.
    #[must_use]
    pub const fn spots(self) -> usize {
        self.0
    }
}

/// A Spot that could be registered, identified by a caller-chosen key (typically the Spot id).
#[derive(Clone, Debug)]
pub struct GeofenceCandidate<'fence, Key> {
    /// What the caller calls this Spot.
    pub key: Key,
    /// Its fence.
    pub fence: &'fence Fence,
}

/// One region to register with the OS.
#[derive(Clone, Debug, PartialEq)]
pub struct OsGeofence<Key> {
    /// The caller's key for the Spot.
    pub key: Key,
    /// The circle to register: the Spot's circle, or a polygon's enclosing circle inflated by
    /// the exit margin.
    pub region: Circle,
}

/// The regions to register, replacing all previous ones.
#[derive(Clone, Debug, PartialEq)]
pub struct GeofenceSelection<Key> {
    /// The nearest Spots, nearest first.
    pub regions: Vec<OsGeofence<Key>>,
    /// Centred on the device; leaving it means "reselect". `None` when every Spot fits.
    pub horizon: Option<Circle>,
}

/// Picks the Spots nearest to `position` up to `capacity`, plus a horizon region of radius
/// `0.8 ×` the distance to the farthest chosen Spot (design E.2).
///
/// Uses only local data, so the choice has no privacy cost.
#[must_use]
pub fn select_os_geofences<Key: Clone>(
    candidates: &[GeofenceCandidate<'_, Key>],
    position: Coordinate,
    capacity: GeofenceCapacity,
) -> GeofenceSelection<Key> {
    let mut regions: Vec<OsGeofence<Key>> = candidates
        .iter()
        .map(|candidate| OsGeofence {
            key: candidate.key.clone(),
            region: os_region(candidate.fence),
        })
        .collect();
    regions.sort_by(|left, right| {
        distance(position, left.region.center())
            .get()
            .total_cmp(&distance(position, right.region.center()).get())
    });
    let all_fit = regions.len() <= capacity.spots();
    regions.truncate(capacity.spots());
    let horizon = if all_fit {
        None
    } else {
        regions
            .last()
            .map(|farthest| distance(position, farthest.region.center()).scaled(0.8))
            .and_then(|radius| Circle::new(position, radius).ok())
    };
    GeofenceSelection { regions, horizon }
}

fn os_region(fence: &Fence) -> Circle {
    match fence {
        Fence::Circle(circle) => *circle,
        Fence::Polygon(_) => {
            let enclosing = fence.enclosing_circle();
            enclosing.inflated(exit_buffer(enclosing.radius()))
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::{GeoError, Meters};

    fn spot_at(latitude: f64) -> Result<Fence, GeoError> {
        Ok(Fence::Circle(Circle::new(
            Coordinate::new(latitude, 0.0)?,
            Meters::from_whole(100),
        )?))
    }

    #[test]
    fn nearest_spots_are_chosen_with_a_horizon() -> Result<(), GeoError> {
        let fences = [spot_at(0.03)?, spot_at(0.01)?, spot_at(0.02)?];
        let candidates: Vec<_> = fences
            .iter()
            .enumerate()
            .map(|(key, fence)| GeofenceCandidate { key, fence })
            .collect();
        let selection = select_os_geofences(
            &candidates,
            Coordinate::new(0.0, 0.0)?,
            GeofenceCapacity::new(2),
        );
        let keys: Vec<usize> = selection.regions.iter().map(|region| region.key).collect();
        assert_eq!(keys, vec![1, 2]);
        let horizon = selection.horizon.ok_or(GeoError::ZeroRadius)?;
        // 0.8 × the distance to the second-nearest Spot (0.02° ≈ 2,224 m).
        assert!((horizon.radius().get() - 1_779.0).abs() < 2.0);
        Ok(())
    }

    #[test]
    fn no_horizon_when_every_spot_fits() -> Result<(), GeoError> {
        let fence = spot_at(0.01)?;
        let candidates = [GeofenceCandidate {
            key: (),
            fence: &fence,
        }];
        let selection = select_os_geofences(
            &candidates,
            Coordinate::new(0.0, 0.0)?,
            GeofenceCapacity::ANDROID,
        );
        assert_eq!(selection.horizon, None);
        Ok(())
    }
}
