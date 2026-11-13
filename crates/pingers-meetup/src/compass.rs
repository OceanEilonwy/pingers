// SPDX-License-Identifier: AGPL-3.0-only
//! The compass, and conversion between geo coordinates and their wire form.

use core::time::Duration;

use pingers_geo::{Bearing, Coordinate, Fix, Meters, bearing, distance};
use pingers_proto::{E7Coordinate, LeafIndex};
use pingers_time::UnixSeconds;

use crate::MeetupError;
use crate::stub::not_implemented;

/// What the compass points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompassTarget {
    /// A member, if their location key is held.
    Member(LeafIndex),
    /// The meet point (a fixed place, or the member it is set to).
    MeetPoint,
}

/// What the compass screen shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompassReading {
    /// Distance from the device to the target.
    pub distance: Meters,
    /// Bearing from the device to the target, relative to true north.
    pub bearing: Bearing,
    /// Bearing relative to the device's heading, when the heading is known (0° = straight
    /// ahead).
    pub relative_bearing: Option<Bearing>,
    /// How old the target's position is.
    pub age: Duration,
}

/// Where the device is and which way it faces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observer {
    /// The device's own latest fix.
    pub fix: Fix,
    /// Its heading from the compass sensor, if available.
    pub heading: Option<Bearing>,
}

/// A target's last known position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetPosition {
    /// Where.
    pub position: Coordinate,
    /// When that position was taken.
    pub taken_at: UnixSeconds,
}

/// Points from the observer to the target.
#[must_use]
pub fn compass(observer: &Observer, target: &TargetPosition, now: UnixSeconds) -> CompassReading {
    let Observer { fix: own, heading } = *observer;
    let TargetPosition {
        position: target,
        taken_at: target_taken_at,
    } = *target;
    let absolute = bearing(own.coordinate, target);
    let relative_bearing =
        heading.and_then(|facing| Bearing::normalised(absolute.degrees() - facing.degrees()).ok());
    CompassReading {
        distance: distance(own.coordinate, target),
        bearing: absolute,
        relative_bearing,
        age: now.saturating_duration_since(target_taken_at),
    }
}

/// A wire coordinate as a geo coordinate.
///
/// # Errors
///
/// [`MeetupError::Geo`] cannot happen for a validated `E7Coordinate`, but is reported rather
/// than assumed.
pub fn coordinate_from_wire(wire: E7Coordinate) -> Result<Coordinate, MeetupError> {
    let latitude = f64::from(wire.latitude_e7()) / 10_000_000.0;
    let longitude = f64::from(wire.longitude_e7()) / 10_000_000.0;
    Ok(Coordinate::new(latitude, longitude)?)
}

/// A geo coordinate rounded to the wire's 10⁻⁷ degrees (about 1 cm).
///
/// # Errors
///
/// [`MeetupError::Proto`] if rounding leaves the wire range.
pub fn coordinate_to_wire(coordinate: Coordinate) -> Result<E7Coordinate, MeetupError> {
    not_implemented(coordinate)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn heading_turns_the_bearing_into_a_relative_one() -> Result<(), MeetupError> {
        let own = Fix {
            coordinate: Coordinate::new(0.0, 0.0)?,
            accuracy: Meters::from_whole(5),
            taken_at: UnixSeconds::new(100),
        };
        let east = Coordinate::new(0.0, 0.001)?;
        let facing_north_east = Bearing::new(45.0)?;
        let observer = Observer {
            fix: own,
            heading: Some(facing_north_east),
        };
        let target = TargetPosition {
            position: east,
            taken_at: UnixSeconds::new(90),
        };
        let reading = compass(&observer, &target, UnixSeconds::new(100));
        let relative = reading
            .relative_bearing
            .ok_or(MeetupError::NotImplemented)?;
        assert!((relative.degrees() - 45.0).abs() < 1e-6);
        assert_eq!(reading.age, Duration::from_secs(10));
        Ok(())
    }
}
