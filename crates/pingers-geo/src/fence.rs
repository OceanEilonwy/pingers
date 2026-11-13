// SPDX-License-Identifier: AGPL-3.0-only
//! Circle and polygon fences.

use crate::stub::{Unimplemented, not_implemented};
use crate::{Coordinate, GeoError, Meters, SquareMeters, distance};

/// The smallest Spot radius the app accepts (design E.2).
pub const MINIMUM_SPOT_RADIUS: Meters = Meters::constant(100.0);

/// Below this radius the UI warns that OS geofences are unreliable (design E.2).
pub const RECOMMENDED_SPOT_RADIUS: Meters = Meters::constant(150.0);

/// A circular fence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Circle {
    center: Coordinate,
    radius: Meters,
}

impl Circle {
    /// A circle around `center`.
    ///
    /// # Errors
    ///
    /// [`GeoError::ZeroRadius`] if `radius` is zero.
    pub fn new(center: Coordinate, radius: Meters) -> Result<Self, GeoError> {
        if radius.get() > 0.0 {
            Ok(Self { center, radius })
        } else {
            Err(GeoError::ZeroRadius)
        }
    }

    /// The centre.
    #[must_use]
    pub const fn center(&self) -> Coordinate {
        self.center
    }

    /// The radius.
    #[must_use]
    pub const fn radius(&self) -> Meters {
        self.radius
    }

    /// Whether `point` is inside or on the circle.
    #[must_use]
    pub fn contains(&self, point: Coordinate) -> bool {
        distance(self.center, point) <= self.radius
    }

    /// How far `point` is outside the circle; zero if inside.
    #[must_use]
    pub fn distance_outside(&self, point: Coordinate) -> Meters {
        distance(self.center, point).saturating_sub(self.radius)
    }

    /// Whether the disc of `radius` around `point` overlaps this circle (for a fix's accuracy
    /// circle against an exclusion zone, design B.11).
    #[must_use]
    pub fn intersects_disc(&self, point: Coordinate, radius: Meters) -> bool {
        distance(self.center, point) <= self.radius.saturating_add(radius)
    }

    /// This circle with its radius grown by `margin`.
    #[must_use]
    pub fn inflated(&self, margin: Meters) -> Self {
        Self {
            center: self.center,
            radius: self.radius.saturating_add(margin),
        }
    }
}

/// A simple polygon fence: at least three vertices, edges that do not cross, non-zero area.
///
/// Vertices are WGS84; containment and area are computed in a local east-north-up projection
/// centred on the polygon, which is exact enough at Spot scale.
#[derive(Clone, Debug, PartialEq)]
pub struct Polygon {
    vertices: Vec<Coordinate>,
    unimplemented: Unimplemented,
}

impl Polygon {
    /// Validates and builds a polygon. The ring is implicitly closed; do not repeat the first
    /// vertex at the end.
    ///
    /// # Errors
    ///
    /// [`GeoError::TooFewVertices`], [`GeoError::SelfIntersecting`] or [`GeoError::Degenerate`].
    pub fn new(vertices: Vec<Coordinate>) -> Result<Self, GeoError> {
        not_implemented(vertices)
    }

    /// The vertices, in the order given.
    #[must_use]
    pub fn vertices(&self) -> &[Coordinate] {
        &self.vertices
    }

    /// Whether `point` is inside or on the polygon.
    #[must_use]
    pub fn contains(&self, point: Coordinate) -> bool {
        let _ = point;
        match self.unimplemented {}
    }

    /// How far `point` is outside the polygon (distance to the nearest edge); zero if inside.
    #[must_use]
    pub fn distance_outside(&self, point: Coordinate) -> Meters {
        let _ = point;
        match self.unimplemented {}
    }

    /// The enclosed area.
    #[must_use]
    pub fn area(&self) -> SquareMeters {
        match self.unimplemented {}
    }

    /// The smallest circle containing every vertex (or a close bound; see `CONTRACT.md`).
    #[must_use]
    pub fn enclosing_circle(&self) -> Circle {
        match self.unimplemented {}
    }
}

/// A Spot's or exclusion zone's shape.
#[derive(Clone, Debug, PartialEq)]
pub enum Fence {
    /// A circle.
    Circle(Circle),
    /// A polygon.
    Polygon(Polygon),
}

impl Fence {
    /// Whether `point` is inside or on the fence.
    #[must_use]
    pub fn contains(&self, point: Coordinate) -> bool {
        match self {
            Self::Circle(circle) => circle.contains(point),
            Self::Polygon(polygon) => polygon.contains(point),
        }
    }

    /// How far `point` is outside the fence; zero if inside.
    #[must_use]
    pub fn distance_outside(&self, point: Coordinate) -> Meters {
        match self {
            Self::Circle(circle) => circle.distance_outside(point),
            Self::Polygon(polygon) => polygon.distance_outside(point),
        }
    }

    /// The fence's `r` in the F.2 rules: a circle's radius, or a polygon's enclosing-circle
    /// radius.
    #[must_use]
    pub fn characteristic_radius(&self) -> Meters {
        self.enclosing_circle().radius()
    }

    /// The circle an OS geofence uses for this fence: the circle itself, or a polygon's
    /// enclosing circle (inflation by the exit margin is the caller's choice, design E.2).
    #[must_use]
    pub fn enclosing_circle(&self) -> Circle {
        match self {
            Self::Circle(circle) => *circle,
            Self::Polygon(polygon) => polygon.enclosing_circle(),
        }
    }

    /// Whether the disc of `radius` around `point` overlaps the fence.
    #[must_use]
    pub fn intersects_disc(&self, point: Coordinate, radius: Meters) -> bool {
        self.distance_outside(point) <= radius
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn anchor() -> Result<Coordinate, GeoError> {
        Coordinate::new(51.5, -0.1)
    }

    #[test]
    fn zero_radius_circle_is_rejected() -> Result<(), GeoError> {
        assert_eq!(
            Circle::new(anchor()?, Meters::ZERO),
            Err(GeoError::ZeroRadius)
        );
        Ok(())
    }

    #[test]
    fn circle_contains_its_centre_but_not_a_point_200_m_away() -> Result<(), GeoError> {
        let circle = Circle::new(anchor()?, Meters::from_whole(100))?;
        assert!(circle.contains(anchor()?));
        // 0.0018° of latitude is about 200 m.
        let away = Coordinate::new(51.5018, -0.1)?;
        assert!(!circle.contains(away));
        assert!((circle.distance_outside(away).get() - 100.0).abs() < 1.0);
        Ok(())
    }

    #[test]
    fn accuracy_disc_reaching_the_fence_intersects_it() -> Result<(), GeoError> {
        let circle = Circle::new(anchor()?, Meters::from_whole(100))?;
        let away = Coordinate::new(51.5018, -0.1)?;
        assert!(Fence::Circle(circle).intersects_disc(away, Meters::from_whole(101)));
        assert!(!Fence::Circle(circle).intersects_disc(away, Meters::from_whole(99)));
        Ok(())
    }
}
