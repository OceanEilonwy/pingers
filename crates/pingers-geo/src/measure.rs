// SPDX-License-Identifier: AGPL-3.0-only
//! Coordinates, lengths, fixes, distance and bearing.

use core::fmt;

use pingers_time::UnixSeconds;

use crate::GeoError;

/// Mean Earth radius (IUGG), in metres. Spherical distances are accurate to about 0.5 %,
/// far inside GPS accuracy at Spot scale.
pub(crate) const EARTH_RADIUS_METERS: f64 = 6_371_008.8;

/// A length in metres: finite and not negative.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Meters(f64);

impl Meters {
    /// Zero metres.
    pub const ZERO: Self = Self(0.0);

    /// Checks that `meters` is finite and not negative.
    ///
    /// # Errors
    ///
    /// [`GeoError::InvalidLength`] otherwise.
    pub fn new(meters: f64) -> Result<Self, GeoError> {
        if meters.is_finite() && meters >= 0.0 {
            Ok(Self(meters))
        } else {
            Err(GeoError::InvalidLength)
        }
    }

    /// A length for a `const` item, such as a slider's limit: `Meters::constant(1000.0)`.
    ///
    /// Meant for literals. A value that is not a valid length cannot be refused in a `const`
    /// without panicking, so it saturates instead: negative or NaN gives [`Meters::ZERO`] and
    /// infinity the largest finite length. Use [`Meters::new`] for values from outside.
    #[must_use]
    pub const fn constant(meters: f64) -> Self {
        if meters.is_nan() || meters < 0.0 {
            Self::ZERO
        } else if meters.is_infinite() {
            Self(f64::MAX)
        } else {
            Self(meters)
        }
    }

    /// A whole number of metres; always valid.
    #[must_use]
    pub fn from_whole(meters: u32) -> Self {
        Self(f64::from(meters))
    }

    /// The length in metres.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    /// The larger of two lengths.
    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        if self.0 >= other.0 { self } else { other }
    }

    /// The sum, saturating at the largest finite length.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self((self.0 + other.0).min(f64::MAX))
    }

    /// The difference, or zero if `other` is longer.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self((self.0 - other.0).max(0.0))
    }

    /// This length scaled by a non-negative factor (used for `r/4`, `0.8 × d` and the like).
    #[must_use]
    pub fn scaled(self, factor: f64) -> Self {
        let scaled = self.0 * factor.max(0.0);
        if scaled.is_finite() {
            Self(scaled)
        } else {
            Self(f64::MAX)
        }
    }
}

/// An area in square metres: finite and not negative.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct SquareMeters(f64);

impl SquareMeters {
    /// Checks that `area` is finite and not negative.
    ///
    /// # Errors
    ///
    /// [`GeoError::InvalidLength`] otherwise.
    pub fn new(area: f64) -> Result<Self, GeoError> {
        if area.is_finite() && area >= 0.0 {
            Ok(Self(area))
        } else {
            Err(GeoError::InvalidLength)
        }
    }

    /// The area in square metres.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

/// A compass bearing in degrees clockwise from true north, in `[0, 360)`.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Bearing(f64);

impl Bearing {
    /// Checks the range.
    ///
    /// # Errors
    ///
    /// [`GeoError::BearingOutOfRange`] unless `degrees` is in `[0, 360)`.
    pub fn new(degrees: f64) -> Result<Self, GeoError> {
        if (0.0..360.0).contains(&degrees) {
            Ok(Self(degrees))
        } else {
            Err(GeoError::BearingOutOfRange)
        }
    }

    /// Normalises any finite angle into `[0, 360)`.
    ///
    /// # Errors
    ///
    /// [`GeoError::BearingOutOfRange`] if `degrees` is not finite.
    pub fn normalised(degrees: f64) -> Result<Self, GeoError> {
        if !degrees.is_finite() {
            return Err(GeoError::BearingOutOfRange);
        }
        let wrapped = degrees.rem_euclid(360.0);
        // rem_euclid can return exactly 360.0 for tiny negative inputs.
        Self::new(if wrapped >= 360.0 { 0.0 } else { wrapped })
    }

    /// The bearing in degrees.
    #[must_use]
    pub const fn degrees(self) -> f64 {
        self.0
    }
}

/// A WGS84 position. `Debug` prints `Coordinate(redacted)` (style guide §9).
#[derive(Clone, Copy, PartialEq)]
pub struct Coordinate {
    latitude: f64,
    longitude: f64,
}

impl Coordinate {
    /// Checks the ranges: latitude within ±90°, longitude within ±180°, both finite.
    ///
    /// # Errors
    ///
    /// [`GeoError::LatitudeOutOfRange`] or [`GeoError::LongitudeOutOfRange`].
    pub fn new(latitude: f64, longitude: f64) -> Result<Self, GeoError> {
        if !(-90.0..=90.0).contains(&latitude) {
            return Err(GeoError::LatitudeOutOfRange);
        }
        if !(-180.0..=180.0).contains(&longitude) {
            return Err(GeoError::LongitudeOutOfRange);
        }
        Ok(Self {
            latitude,
            longitude,
        })
    }

    /// Latitude in degrees.
    #[must_use]
    pub const fn latitude(self) -> f64 {
        self.latitude
    }

    /// Longitude in degrees.
    #[must_use]
    pub const fn longitude(self) -> f64 {
        self.longitude
    }
}

impl fmt::Debug for Coordinate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Coordinate(redacted)")
    }
}

/// A location fix from the platform. `Debug` prints `Fix(redacted)`.
#[derive(Clone, Copy, PartialEq)]
pub struct Fix {
    /// Where.
    pub coordinate: Coordinate,
    /// Horizontal accuracy radius.
    pub accuracy: Meters,
    /// When the fix was taken, by the device's wall clock.
    pub taken_at: UnixSeconds,
}

impl fmt::Debug for Fix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Fix(redacted)")
    }
}

/// Great-circle distance between two positions.
#[must_use]
pub fn distance(from: Coordinate, to: Coordinate) -> Meters {
    let from_latitude = from.latitude.to_radians();
    let to_latitude = to.latitude.to_radians();
    let latitude_change = (to.latitude - from.latitude).to_radians();
    let longitude_change = (to.longitude - from.longitude).to_radians();
    let haversine = (latitude_change / 2.0).sin().powi(2)
        + from_latitude.cos() * to_latitude.cos() * (longitude_change / 2.0).sin().powi(2);
    let central_angle = 2.0 * haversine.sqrt().clamp(0.0, 1.0).asin();
    Meters::new(EARTH_RADIUS_METERS * central_angle).unwrap_or(Meters::ZERO)
}

/// Initial great-circle bearing from `from` towards `to`. Due north (0°) if they coincide.
#[must_use]
pub fn bearing(from: Coordinate, to: Coordinate) -> Bearing {
    let from_latitude = from.latitude.to_radians();
    let to_latitude = to.latitude.to_radians();
    let longitude_change = (to.longitude - from.longitude).to_radians();
    let east = longitude_change.sin() * to_latitude.cos();
    let north = from_latitude.cos() * to_latitude.sin()
        - from_latitude.sin() * to_latitude.cos() * longitude_change.cos();
    Bearing::normalised(east.atan2(north).to_degrees()).unwrap_or(Bearing(0.0))
}

/// How far beyond a fence a fix must be to confirm an exit: `max(50 m, r/4)` (design F.2).
#[must_use]
pub fn exit_buffer(radius: Meters) -> Meters {
    Meters::from_whole(50).max(radius.scaled(0.25))
}

/// The worst accuracy a fix may have to confirm an entry: `max(50 m, r/2)` (design F.2).
#[must_use]
pub fn entry_accuracy_limit(radius: Meters) -> Meters {
    Meters::from_whole(50).max(radius.scaled(0.5))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn constants_keep_valid_lengths_and_saturate_the_rest() {
        assert_eq!(Meters::constant(150.0), Meters::from_whole(150));
        assert_eq!(Meters::constant(0.0), Meters::ZERO);
        assert_eq!(Meters::constant(-1.0), Meters::ZERO);
        assert_eq!(Meters::constant(f64::NAN), Meters::ZERO);
        assert_eq!(
            Meters::constant(f64::INFINITY),
            Meters::new(f64::MAX).unwrap_or(Meters::ZERO)
        );
    }

    #[test]
    fn one_degree_of_latitude_is_about_111_km() -> Result<(), GeoError> {
        let length = distance(Coordinate::new(0.0, 0.0)?, Coordinate::new(1.0, 0.0)?);
        assert!((length.get() - 111_195.0).abs() < 10.0);
        Ok(())
    }

    #[test]
    fn bearing_due_east_is_90_degrees() -> Result<(), GeoError> {
        let east = bearing(Coordinate::new(0.0, 0.0)?, Coordinate::new(0.0, 1.0)?);
        assert!((east.degrees() - 90.0).abs() < 1e-9);
        Ok(())
    }

    #[test]
    fn bearing_west_wraps_to_270() -> Result<(), GeoError> {
        let west = bearing(Coordinate::new(0.0, 0.0)?, Coordinate::new(0.0, -1.0)?);
        assert!((west.degrees() - 270.0).abs() < 1e-9);
        Ok(())
    }

    #[test]
    fn small_fence_exit_buffer_is_50_metres() {
        assert_eq!(exit_buffer(Meters::from_whole(100)), Meters::from_whole(50));
        assert_eq!(
            exit_buffer(Meters::from_whole(400)),
            Meters::from_whole(100)
        );
    }

    #[test]
    fn entry_accuracy_limit_is_half_the_radius_for_large_fences() {
        assert_eq!(
            entry_accuracy_limit(Meters::from_whole(80)),
            Meters::from_whole(50)
        );
        assert_eq!(
            entry_accuracy_limit(Meters::from_whole(300)),
            Meters::from_whole(150)
        );
    }

    #[test]
    fn out_of_range_coordinates_are_rejected() {
        assert_eq!(
            Coordinate::new(90.5, 0.0),
            Err(GeoError::LatitudeOutOfRange)
        );
        assert_eq!(
            Coordinate::new(0.0, f64::NAN),
            Err(GeoError::LongitudeOutOfRange)
        );
    }

    #[test]
    fn negative_lengths_are_rejected() {
        assert_eq!(Meters::new(-1.0), Err(GeoError::InvalidLength));
        assert_eq!(Meters::new(f64::INFINITY), Err(GeoError::InvalidLength));
    }

    #[test]
    fn locations_are_redacted_in_debug_output() -> Result<(), GeoError> {
        let coordinate = Coordinate::new(51.5, -0.1)?;
        assert_eq!(format!("{coordinate:?}"), "Coordinate(redacted)");
        let fix = Fix {
            coordinate,
            accuracy: Meters::from_whole(5),
            taken_at: UnixSeconds::new(1),
        };
        assert_eq!(format!("{fix:?}"), "Fix(redacted)");
        Ok(())
    }
}
