// SPDX-License-Identifier: AGPL-3.0-only
//! Coordinates, fences and fixes as plain numbers.

use core::fmt;

use crate::CoreError;

/// A WGS84 position in degrees. `Debug` does not print it (style guide §9).
#[derive(Clone, Copy, PartialEq, uniffi::Record)]
pub struct Coordinate {
    /// Degrees north, -90 to 90.
    pub latitude: f64,
    /// Degrees east, -180 to 180.
    pub longitude: f64,
}

impl fmt::Debug for Coordinate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Coordinate(redacted)")
    }
}

impl From<pingers_geo::Coordinate> for Coordinate {
    fn from(coordinate: pingers_geo::Coordinate) -> Self {
        Self {
            latitude: coordinate.latitude(),
            longitude: coordinate.longitude(),
        }
    }
}

impl TryFrom<Coordinate> for pingers_geo::Coordinate {
    type Error = CoreError;

    fn try_from(coordinate: Coordinate) -> Result<Self, CoreError> {
        Ok(Self::new(coordinate.latitude, coordinate.longitude)?)
    }
}

/// Converts a list of coordinates from the app.
pub(crate) fn coordinates(
    points: &[Coordinate],
) -> Result<Vec<pingers_geo::Coordinate>, CoreError> {
    points.iter().map(|point| (*point).try_into()).collect()
}

/// A Spot's or zone's shape.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum Fence {
    /// A circle.
    Circle {
        /// Its centre.
        center: Coordinate,
        /// Its radius in metres.
        radius_m: f64,
    },
    /// A polygon, its vertices in order (not repeating the first).
    Polygon {
        /// The vertices.
        vertices: Vec<Coordinate>,
    },
}

impl From<&pingers_geo::Fence> for Fence {
    fn from(fence: &pingers_geo::Fence) -> Self {
        match fence {
            pingers_geo::Fence::Circle(circle) => Self::Circle {
                center: circle.center().into(),
                radius_m: circle.radius().get(),
            },
            pingers_geo::Fence::Polygon(polygon) => Self::Polygon {
                vertices: polygon.vertices().iter().copied().map(Into::into).collect(),
            },
        }
    }
}

impl TryFrom<&Fence> for pingers_geo::Fence {
    type Error = CoreError;

    fn try_from(fence: &Fence) -> Result<Self, CoreError> {
        Ok(match fence {
            Fence::Circle { center, radius_m } => Self::Circle(pingers_geo::Circle::new(
                (*center).try_into()?,
                pingers_geo::Meters::new(*radius_m)?,
            )?),
            Fence::Polygon { vertices } => {
                Self::Polygon(pingers_geo::Polygon::new(coordinates(vertices)?)?)
            }
        })
    }
}

/// A location fix from the platform.
#[derive(Clone, Copy, PartialEq, uniffi::Record)]
pub struct Fix {
    /// Where.
    pub coordinate: Coordinate,
    /// The accuracy radius in metres.
    pub accuracy_m: f64,
    /// When it was taken, in Unix seconds.
    pub taken_at: u64,
}

impl fmt::Debug for Fix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Fix(redacted)")
    }
}

impl TryFrom<Fix> for pingers_geo::Fix {
    type Error = CoreError;

    fn try_from(fix: Fix) -> Result<Self, CoreError> {
        Ok(Self {
            coordinate: fix.coordinate.try_into()?,
            accuracy: pingers_geo::Meters::new(fix.accuracy_m)?,
            taken_at: pingers_time::UnixSeconds::new(fix.taken_at),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Coordinate, Fence};

    #[test]
    fn fences_round_trip() -> Result<(), crate::CoreError> {
        let circle = Fence::Circle {
            center: Coordinate {
                latitude: 51.5,
                longitude: -0.1,
            },
            radius_m: 150.0,
        };
        let core = pingers_geo::Fence::try_from(&circle)?;
        assert_eq!(Fence::from(&core), circle);
        let bad = Coordinate {
            latitude: 91.0,
            longitude: 0.0,
        };
        assert!(pingers_geo::Coordinate::try_from(bad).is_err());
        Ok(())
    }
}
