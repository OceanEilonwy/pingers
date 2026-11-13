// SPDX-License-Identifier: AGPL-3.0-only
//! Circle and polygon fences.

use core::fmt;

use crate::plane::{self, Planar, TOUCH_TOLERANCE_METERS};
use crate::{Coordinate, GeoError, Meters, SquareMeters, distance};

/// The smallest Spot radius the app accepts (design E.2).
pub const MINIMUM_SPOT_RADIUS: Meters = Meters::constant(100.0);

/// Below this radius the UI warns that OS geofences are unreliable (design E.2).
pub const RECOMMENDED_SPOT_RADIUS: Meters = Meters::constant(150.0);

/// How far a polygon may span: every vertex must be within this distance of the first.
///
/// Larger shapes are not Spots or exclusion zones, and the local projection a polygon is
/// computed in is exact to the millimetre only within this range.
pub const MAXIMUM_POLYGON_EXTENT: Meters = Meters::constant(100_000.0);

/// The most vertices a polygon may have.
///
/// Polygons arrive from other Party members and are rebuilt on every load, and the validity
/// checks are quadratic (self-intersection) and cubic (enclosing circle) in the vertex count,
/// so an unbounded polygon could stall every member's core. A drawn Spot needs far fewer.
pub const MAXIMUM_POLYGON_VERTICES: usize = 100;

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

/// A simple polygon fence: at least three vertices, edges that neither cross nor touch,
/// non-zero area, and no larger than [`MAXIMUM_POLYGON_EXTENT`].
///
/// Vertices are WGS84. Containment, edge distance and area are computed in a local azimuthal
/// equidistant projection centred on the polygon's enclosing circle, in which the edges are
/// straight lines. That projection has no seam, so a polygon may straddle the antimeridian or
/// surround a pole; within the size limit its error is well under a millimetre.
#[derive(Clone, PartialEq)]
pub struct Polygon {
    vertices: Vec<Coordinate>,
    /// The vertices projected about `enclosing.center()`, in the same order.
    ring: Vec<Planar>,
    enclosing: Circle,
    area: SquareMeters,
}

impl fmt::Debug for Polygon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Polygon")
            .field("vertex_count", &self.vertices.len())
            .finish_non_exhaustive()
    }
}

impl Polygon {
    /// Validates and builds a polygon. The ring is implicitly closed; do not repeat the first
    /// vertex at the end. Either winding order is accepted.
    ///
    /// # Errors
    ///
    /// - [`GeoError::TooFewVertices`] for fewer than three vertices;
    /// - [`GeoError::TooManyVertices`] for more than [`MAXIMUM_POLYGON_VERTICES`];
    /// - [`GeoError::PolygonTooLarge`] if a vertex is farther than [`MAXIMUM_POLYGON_EXTENT`]
    ///   from the first;
    /// - [`GeoError::Degenerate`] if every vertex lies on one line;
    /// - [`GeoError::SelfIntersecting`] if two edges cross or touch, an edge retraces the one
    ///   before it, or a vertex repeats.
    pub fn new(vertices: Vec<Coordinate>) -> Result<Self, GeoError> {
        let Some(first) = vertices.first().copied() else {
            return Err(GeoError::TooFewVertices);
        };
        if vertices.len() < 3 {
            return Err(GeoError::TooFewVertices);
        }
        if vertices.len() > MAXIMUM_POLYGON_VERTICES {
            return Err(GeoError::TooManyVertices);
        }
        if vertices
            .iter()
            .any(|vertex| distance(first, *vertex) > MAXIMUM_POLYGON_EXTENT)
        {
            return Err(GeoError::PolygonTooLarge);
        }
        let provisional: Vec<Planar> = vertices
            .iter()
            .map(|vertex| plane::project(first, *vertex))
            .collect();
        if plane::is_collinear(&provisional) {
            return Err(GeoError::Degenerate);
        }
        let minimal =
            plane::minimal_enclosing_circle(&provisional).ok_or(GeoError::Internal)?;
        let center = plane::unproject(first, minimal.center)?;
        let ring: Vec<Planar> = vertices
            .iter()
            .map(|vertex| plane::project(center, *vertex))
            .collect();
        if plane::is_self_intersecting(&ring) {
            return Err(GeoError::SelfIntersecting);
        }
        // Measured on the sphere, so the circle covers every vertex whatever the projection's
        // rounding.
        let radius = vertices
            .iter()
            .map(|vertex| distance(center, *vertex))
            .fold(Meters::ZERO, Meters::max);
        let enclosing = Circle::new(center, radius)?;
        let area = SquareMeters::new(plane::area(&ring))?;
        Ok(Self {
            vertices,
            ring,
            enclosing,
            area,
        })
    }

    /// The vertices, in the order given.
    #[must_use]
    pub fn vertices(&self) -> &[Coordinate] {
        &self.vertices
    }

    /// Whether `point` is inside or on the polygon. A point within a centimetre of an edge
    /// counts as on it.
    #[must_use]
    pub fn contains(&self, point: Coordinate) -> bool {
        self.distance_outside(point) == Meters::ZERO
    }

    /// How far `point` is outside the polygon (distance to the nearest edge); zero if inside
    /// or on it.
    #[must_use]
    pub fn distance_outside(&self, point: Coordinate) -> Meters {
        let projected = plane::project(self.enclosing.center(), point);
        if plane::encloses(&self.ring, projected) {
            return Meters::ZERO;
        }
        let edge_distance = plane::distance_to_boundary(&self.ring, projected);
        if edge_distance <= TOUCH_TOLERANCE_METERS {
            Meters::ZERO
        } else {
            Meters::new(edge_distance).unwrap_or(Meters::ZERO)
        }
    }

    /// The enclosed area.
    #[must_use]
    pub const fn area(&self) -> SquareMeters {
        self.area
    }

    /// The smallest circle containing every vertex, to within the projection's rounding (far
    /// inside the 10 % allowed by `CONTRACT.md`).
    #[must_use]
    pub const fn enclosing_circle(&self) -> Circle {
        self.enclosing
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
    use crate::testing::{offset, polygon_around};

    fn anchor() -> Result<Coordinate, GeoError> {
        Coordinate::new(51.5, -0.1)
    }

    /// A 100 m square with its south-west corner at the anchor.
    const SQUARE: [[f64; 2]; 4] = [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]];

    /// An L: a 200 m square with its north-east quarter missing.
    const L_SHAPE: [[f64; 2]; 6] = [
        [0.0, 0.0],
        [200.0, 0.0],
        [200.0, 100.0],
        [100.0, 100.0],
        [100.0, 200.0],
        [0.0, 200.0],
    ];

    fn close(actual: f64, expected: f64, tolerance: f64) -> bool {
        (actual - expected).abs() <= tolerance
    }

    /// `count` vertices on a 500 m circle around the anchor: a valid convex polygon.
    fn regular_polygon(count: u32) -> Result<Vec<Coordinate>, GeoError> {
        let center = anchor()?;
        let step = core::f64::consts::TAU / f64::from(count);
        (0..count)
            .map(|index| {
                let angle = step * f64::from(index);
                offset(center, 500.0 * angle.sin(), 500.0 * angle.cos())
            })
            .collect()
    }

    #[test]
    fn polygons_above_the_vertex_limit_are_refused() -> Result<(), GeoError> {
        let limit = u32::try_from(MAXIMUM_POLYGON_VERTICES).map_err(|_| GeoError::Internal)?;
        assert!(Polygon::new(regular_polygon(limit)?).is_ok());
        let over = limit.checked_add(1).ok_or(GeoError::Internal)?;
        assert_eq!(
            Polygon::new(regular_polygon(over)?).err(),
            Some(GeoError::TooManyVertices)
        );
        Ok(())
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
        assert!(close(circle.distance_outside(away).get(), 100.0, 1.0));
        Ok(())
    }

    #[test]
    fn accuracy_disc_reaching_the_fence_intersects_it() -> Result<(), GeoError> {
        let circle = Circle::new(anchor()?, Meters::from_whole(100))?;
        let away = Coordinate::new(51.5018, -0.1)?;
        assert!(circle.intersects_disc(away, Meters::from_whole(101)));
        assert!(!circle.intersects_disc(away, Meters::from_whole(99)));
        assert!(Fence::Circle(circle).intersects_disc(away, Meters::from_whole(101)));
        assert!(!Fence::Circle(circle).intersects_disc(away, Meters::from_whole(99)));
        Ok(())
    }

    #[test]
    fn inflating_a_circle_grows_only_its_radius() -> Result<(), GeoError> {
        let circle = Circle::new(anchor()?, Meters::from_whole(100))?;
        let inflated = circle.inflated(Meters::from_whole(30));
        assert_eq!(inflated.center(), circle.center());
        assert_eq!(inflated.radius(), Meters::from_whole(130));
        Ok(())
    }

    #[test]
    fn polygon_needs_three_vertices() -> Result<(), GeoError> {
        assert_eq!(Polygon::new(Vec::new()), Err(GeoError::TooFewVertices));
        assert_eq!(
            polygon_around(anchor()?, &[[0.0, 0.0], [100.0, 0.0]]),
            Err(GeoError::TooFewVertices)
        );
        Ok(())
    }

    #[test]
    fn polygon_wider_than_the_extent_limit_is_rejected() -> Result<(), GeoError> {
        let triangle = |east: f64| [[0.0, 0.0], [east, 0.0], [0.0, 1_000.0]];
        assert_eq!(
            polygon_around(anchor()?, &triangle(100_010.0)),
            Err(GeoError::PolygonTooLarge)
        );
        assert!(polygon_around(anchor()?, &triangle(99_990.0)).is_ok());
        Ok(())
    }

    #[test]
    fn vertices_on_one_line_are_degenerate() -> Result<(), GeoError> {
        assert_eq!(
            polygon_around(anchor()?, &[[0.0, 0.0], [50.0, 50.0], [100.0, 100.0]]),
            Err(GeoError::Degenerate)
        );
        assert_eq!(
            polygon_around(anchor()?, &[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]),
            Err(GeoError::Degenerate)
        );
        Ok(())
    }

    #[test]
    fn a_bow_tie_is_self_intersecting() -> Result<(), GeoError> {
        assert_eq!(
            polygon_around(
                anchor()?,
                &[[0.0, 0.0], [100.0, 100.0], [100.0, 0.0], [0.0, 100.0]]
            ),
            Err(GeoError::SelfIntersecting)
        );
        Ok(())
    }

    #[test]
    fn two_lobes_touching_at_a_vertex_are_self_intersecting() -> Result<(), GeoError> {
        let figure_eight = [
            [0.0, 0.0],
            [100.0, 0.0],
            [50.0, 50.0],
            [100.0, 100.0],
            [0.0, 100.0],
            [50.0, 50.0],
        ];
        assert_eq!(
            polygon_around(anchor()?, &figure_eight),
            Err(GeoError::SelfIntersecting)
        );
        Ok(())
    }

    #[test]
    fn a_spike_that_retraces_an_edge_is_self_intersecting() -> Result<(), GeoError> {
        // The third edge runs back along the second.
        let spike = [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [100.0, 50.0]];
        assert_eq!(
            polygon_around(anchor()?, &spike),
            Err(GeoError::SelfIntersecting)
        );
        Ok(())
    }

    #[test]
    fn square_has_its_area_and_contains_its_inside() -> Result<(), GeoError> {
        let square = polygon_around(anchor()?, &SQUARE)?;
        assert!(close(square.area().get(), 10_000.0, 0.01));
        assert!(square.contains(offset(anchor()?, 50.0, 50.0)?));
        assert!(!square.contains(offset(anchor()?, 150.0, 50.0)?));
        assert_eq!(square.vertices().len(), 4);
        Ok(())
    }

    #[test]
    fn points_on_the_boundary_are_inside() -> Result<(), GeoError> {
        let square = polygon_around(anchor()?, &SQUARE)?;
        for vertex in square.vertices() {
            assert!(square.contains(*vertex));
            assert_eq!(square.distance_outside(*vertex), Meters::ZERO);
        }
        // Edge midpoints, which the even-odd test alone may put either side.
        for [east, north] in [[50.0, 0.0], [100.0, 50.0], [50.0, 100.0], [0.0, 50.0]] {
            assert!(square.contains(offset(anchor()?, east, north)?));
        }
        Ok(())
    }

    #[test]
    fn distance_outside_is_the_distance_to_the_nearest_edge() -> Result<(), GeoError> {
        let square = polygon_around(anchor()?, &SQUARE)?;
        let beside = offset(anchor()?, 150.0, 50.0)?;
        assert!(close(square.distance_outside(beside).get(), 50.0, 0.01));
        // Diagonally off a corner, the nearest point is the corner.
        let off_corner = offset(anchor()?, 130.0, 140.0)?;
        assert!(close(square.distance_outside(off_corner).get(), 50.0, 0.01));
        assert_eq!(
            square.distance_outside(offset(anchor()?, 50.0, 50.0)?),
            Meters::ZERO
        );
        Ok(())
    }

    #[test]
    fn the_notch_of_a_concave_polygon_is_outside() -> Result<(), GeoError> {
        let shape = polygon_around(anchor()?, &L_SHAPE)?;
        assert!(close(shape.area().get(), 30_000.0, 0.1));
        assert!(!shape.contains(offset(anchor()?, 150.0, 150.0)?));
        assert!(close(
            shape
                .distance_outside(offset(anchor()?, 150.0, 150.0)?)
                .get(),
            50.0,
            0.01
        ));
        assert!(shape.contains(offset(anchor()?, 50.0, 150.0)?));
        assert!(shape.contains(offset(anchor()?, 150.0, 50.0)?));
        Ok(())
    }

    #[test]
    fn winding_order_does_not_matter() -> Result<(), GeoError> {
        let mut reversed = L_SHAPE;
        reversed.reverse();
        let forward = polygon_around(anchor()?, &L_SHAPE)?;
        let backward = polygon_around(anchor()?, &reversed)?;
        assert!(close(forward.area().get(), backward.area().get(), 1e-6));
        assert!(!backward.contains(offset(anchor()?, 150.0, 150.0)?));
        Ok(())
    }

    #[test]
    fn enclosing_circle_of_a_square_is_centred_with_half_the_diagonal() -> Result<(), GeoError> {
        let square = polygon_around(anchor()?, &SQUARE)?;
        let circle = square.enclosing_circle();
        assert!(close(circle.radius().get(), 50.0 * 2.0_f64.sqrt(), 0.01));
        assert!(close(
            distance(circle.center(), offset(anchor()?, 50.0, 50.0)?).get(),
            0.0,
            0.01
        ));
        let fence = Fence::Polygon(square);
        assert_eq!(fence.enclosing_circle(), circle);
        assert_eq!(fence.characteristic_radius(), circle.radius());
        Ok(())
    }

    #[test]
    fn enclosing_circle_of_an_obtuse_triangle_spans_its_longest_side() -> Result<(), GeoError> {
        // The circumcircle would be far larger; the minimal circle has the long side as
        // diameter.
        let triangle = polygon_around(anchor()?, &[[0.0, 0.0], [200.0, 0.0], [100.0, 10.0]])?;
        assert!(close(
            triangle.enclosing_circle().radius().get(),
            100.0,
            0.01
        ));
        Ok(())
    }

    #[test]
    fn fence_dispatches_to_the_polygon() -> Result<(), GeoError> {
        let fence = Fence::Polygon(polygon_around(anchor()?, &L_SHAPE)?);
        let notch = offset(anchor()?, 150.0, 150.0)?;
        assert!(!fence.contains(notch));
        assert!(close(fence.distance_outside(notch).get(), 50.0, 0.01));
        assert!(fence.contains(offset(anchor()?, 50.0, 50.0)?));
        Ok(())
    }

    #[test]
    fn polygon_across_the_antimeridian_contains_the_meridian() -> Result<(), GeoError> {
        let corners = [
            Coordinate::new(-0.001, 179.999)?,
            Coordinate::new(-0.001, -179.999)?,
            Coordinate::new(0.001, -179.999)?,
            Coordinate::new(0.001, 179.999)?,
        ];
        let square = Polygon::new(corners.to_vec())?;
        // 0.002° on the equator is 222.39 m each way.
        let side = distance(Coordinate::new(0.0, 0.0)?, Coordinate::new(0.0, 0.002)?).get();
        assert!(close(square.area().get(), side * side, 1.0));
        assert!(square.contains(Coordinate::new(0.0, 180.0)?));
        assert!(square.contains(Coordinate::new(0.0, -180.0)?));
        assert!(!square.contains(Coordinate::new(0.0, 179.99)?));
        assert!(square.enclosing_circle().radius().get() < 160.0);
        Ok(())
    }

    #[test]
    fn polygon_around_the_north_pole_contains_the_pole() -> Result<(), GeoError> {
        let corners = [0.0, 90.0, 180.0, -90.0]
            .into_iter()
            .map(|longitude| Coordinate::new(89.999, longitude))
            .collect::<Result<Vec<_>, _>>()?;
        let square = Polygon::new(corners)?;
        let half_diagonal = distance(Coordinate::new(90.0, 0.0)?, Coordinate::new(89.999, 0.0)?);
        assert!(close(
            square.area().get(),
            2.0 * half_diagonal.get() * half_diagonal.get(),
            1.0
        ));
        assert!(square.contains(Coordinate::new(90.0, 0.0)?));
        assert!(square.contains(Coordinate::new(89.9995, 45.0)?));
        assert!(!square.contains(Coordinate::new(89.998, 0.0)?));
        Ok(())
    }

    #[test]
    fn polygon_around_the_south_pole_contains_the_pole() -> Result<(), GeoError> {
        let corners = [0.0, 120.0, -120.0]
            .into_iter()
            .map(|longitude| Coordinate::new(-89.999, longitude))
            .collect::<Result<Vec<_>, _>>()?;
        let triangle = Polygon::new(corners)?;
        assert!(triangle.contains(Coordinate::new(-90.0, 0.0)?));
        assert!(!triangle.contains(Coordinate::new(-89.99, 0.0)?));
        Ok(())
    }

    #[test]
    fn polygon_debug_hides_its_vertices() -> Result<(), GeoError> {
        let square = polygon_around(anchor()?, &SQUARE)?;
        assert_eq!(format!("{square:?}"), "Polygon { vertex_count: 4, .. }");
        Ok(())
    }

    mod properties {

        use core::f64::consts::{PI, TAU};

        use proptest::prelude::*;

        use super::*;

        /// A star-shaped polygon around `center`: vertices in strictly increasing direction,
        /// no gap of half a turn or more, so it is simple and contains `center`.
        #[derive(Clone, Debug)]
        struct Star {
            center: Coordinate,
            /// Direction (radians clockwise from north) and distance (metres) of each vertex.
            rays: Vec<[f64; 2]>,
        }

        impl Star {
            fn offsets(&self) -> Vec<[f64; 2]> {
                self.rays
                    .iter()
                    .map(|&[direction, length]| {
                        [length * direction.sin(), length * direction.cos()]
                    })
                    .collect()
            }

            fn polygon(&self) -> Result<Polygon, GeoError> {
                polygon_around(self.center, &self.offsets())
            }

            fn planar_area(&self) -> f64 {
                let offsets = self.offsets();
                let twice: f64 = offsets
                    .iter()
                    .zip(offsets.iter().cycle().skip(1))
                    .map(|(&[east, north], &[next_east, next_north])| {
                        east * next_north - north * next_east
                    })
                    .sum();
                (twice / 2.0).abs()
            }

            fn point_on_ray(&self, direction: f64, along: f64) -> Result<Coordinate, GeoError> {
                offset(
                    self.center,
                    along * direction.sin(),
                    along * direction.cos(),
                )
            }
        }

        fn coordinate() -> impl Strategy<Value = Coordinate> {
            (-89.0..89.0_f64, -180.0..=180.0_f64)
                .prop_filter_map("valid coordinate", |(latitude, longitude)| {
                    Coordinate::new(latitude, longitude).ok()
                })
        }

        fn star() -> impl Strategy<Value = Star> {
            (
                coordinate(),
                prop::collection::vec((1.0..1.9_f64, 50.0..2_000.0_f64), 3..12),
                0.0..TAU,
            )
                .prop_map(|(center, gaps, rotation)| {
                    let total: f64 = gaps.iter().map(|&(gap, _)| gap).sum();
                    let mut direction = rotation;
                    let rays = gaps
                        .iter()
                        .map(|&(gap, length)| {
                            let ray = [direction, length];
                            direction += gap / total * TAU;
                            ray
                        })
                        .collect();
                    Star { center, rays }
                })
        }

        proptest! {
            #[test]
            fn a_star_contains_its_centre_and_its_vertices(star in star()) {
                let polygon = star.polygon()?;
                prop_assert!(polygon.contains(star.center));
                for vertex in polygon.vertices() {
                    prop_assert!(polygon.contains(*vertex));
                }
            }

            #[test]
            fn a_star_contains_every_point_on_its_rays(
                star in star(),
                fraction in 0.0..0.999_f64,
            ) {
                let polygon = star.polygon()?;
                for &[direction, length] in &star.rays {
                    prop_assert!(polygon.contains(star.point_on_ray(direction, length * fraction)?));
                }
            }

            #[test]
            fn beyond_the_farthest_vertex_is_outside(star in star(), extra in 1.0..500.0_f64) {
                // Every vertex is within the farthest one's distance of the centre, so the
                // whole star is too.
                let polygon = star.polygon()?;
                let [direction, length] = star.rays.iter().copied()
                    .fold([0.0, 0.0], |best, ray| if ray[1] > best[1] { ray } else { best });
                let point = star.point_on_ray(direction, length + extra)?;
                prop_assert!(!polygon.contains(point));
            }

            #[test]
            fn area_matches_the_shoelace_of_the_offsets(star in star()) {
                let polygon = star.polygon()?;
                let expected = star.planar_area();
                prop_assert!((polygon.area().get() - expected).abs() <= expected * 1e-6);
            }

            #[test]
            fn area_ignores_the_starting_vertex_and_winding(star in star(), shift in 0_usize..12) {
                let offsets = star.offsets();
                let mut rotated = offsets.clone();
                rotated.rotate_left(shift.checked_rem(offsets.len()).unwrap_or(0));
                let mut reversed = offsets.clone();
                reversed.reverse();
                let area = star.polygon()?.area().get();
                let rotated_area = polygon_around(star.center, &rotated)?.area().get();
                let reversed_area = polygon_around(star.center, &reversed)?.area().get();
                prop_assert!((area - rotated_area).abs() <= area * 1e-9);
                prop_assert!((area - reversed_area).abs() <= area * 1e-9);
            }

            #[test]
            fn area_fits_inside_the_enclosing_circle(star in star()) {
                let polygon = star.polygon()?;
                let radius = polygon.enclosing_circle().radius().get();
                prop_assert!(polygon.area().get() > 0.0);
                prop_assert!(polygon.area().get() <= PI * radius * radius);
            }

            #[test]
            fn enclosing_circle_covers_every_vertex_and_is_near_minimal(star in star()) {
                let polygon = star.polygon()?;
                let circle = polygon.enclosing_circle();
                for vertex in polygon.vertices() {
                    prop_assert!(circle.contains(*vertex));
                }
                // A circle round the star's centre through its farthest vertex also covers
                // every vertex, so the minimal one is no larger.
                let farthest = star.rays.iter().map(|&[_, length]| length).fold(0.0, f64::max);
                prop_assert!(circle.radius().get() <= farthest + 0.02);
                // No circle covering two vertices is smaller than half their distance.
                let widest = polygon.vertices().iter().flat_map(|first| {
                    polygon.vertices().iter().map(move |second| distance(*first, *second).get())
                }).fold(0.0, f64::max);
                prop_assert!(circle.radius().get() >= widest / 2.0 - 1e-3);
            }

            #[test]
            fn points_beyond_the_enclosing_circle_are_outside(
                star in star(),
                direction in 0.0..TAU,
                margin in 0.01..5_000.0_f64,
            ) {
                let polygon = star.polygon()?;
                let circle = polygon.enclosing_circle();
                let reach = circle.radius().get() + margin;
                let point = offset(circle.center(), reach * direction.sin(), reach * direction.cos())?;
                prop_assert!(!polygon.contains(point));
                prop_assert!(polygon.distance_outside(point).get() >= margin * 0.999);
            }

        }
    }
}
