// SPDX-License-Identifier: AGPL-3.0-only
//! The local projection polygons are computed in, and planar geometry on it.
//!
//! The projection is azimuthal equidistant about an origin: a point maps to the vector of
//! length `distance(origin, point)` in the direction `bearing(origin, point)`. Distances from
//! the origin are exact, and directions are true, so the projection has no seam at the
//! antimeridian and stays well defined at the poles (where "bearing" degenerates into
//! longitude, consistently in both directions). Away from the origin, lengths across the
//! direction of view stretch by `(d/R) / sin(d/R)`: under 0.005 % at 100 km, which is why
//! polygons are limited to that size (see [`crate::MAXIMUM_POLYGON_EXTENT`]).
//!
//! Polygon edges are straight lines in this projection, not great circles. At Spot scale the
//! two differ by millimetres.

use core::fmt;

use crate::measure::EARTH_RADIUS_METERS;
use crate::{Coordinate, GeoError, bearing, distance};

/// Two points closer than this are treated as touching when checking self-intersection, and a
/// point this close to an edge counts as on the boundary (inside).
pub(crate) const TOUCH_TOLERANCE_METERS: f64 = 1e-6;

/// A position in metres east and north of a projection origin. `Debug` is redacted: with the
/// origin, it is a location.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Planar {
    east: f64,
    north: f64,
}

impl fmt::Debug for Planar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Planar(redacted)")
    }
}

impl Planar {
    pub(crate) const fn new(east: f64, north: f64) -> Self {
        Self { east, north }
    }

    #[cfg(test)]
    pub(crate) const fn east(self) -> f64 {
        self.east
    }

    #[cfg(test)]
    pub(crate) const fn north(self) -> f64 {
        self.north
    }

    fn minus(self, other: Self) -> Self {
        Self::new(self.east - other.east, self.north - other.north)
    }

    fn plus_scaled(self, direction: Self, factor: f64) -> Self {
        Self::new(
            direction.east.mul_add(factor, self.east),
            direction.north.mul_add(factor, self.north),
        )
    }

    fn cross(self, other: Self) -> f64 {
        self.east.mul_add(other.north, -(self.north * other.east))
    }

    fn dot(self, other: Self) -> f64 {
        self.east.mul_add(other.east, self.north * other.north)
    }

    fn length(self) -> f64 {
        self.east.hypot(self.north)
    }

    fn distance_to(self, other: Self) -> f64 {
        self.minus(other).length()
    }
}

/// Projects `point` into the plane about `origin`.
pub(crate) fn project(origin: Coordinate, point: Coordinate) -> Planar {
    let length = distance(origin, point).get();
    let direction = bearing(origin, point).degrees().to_radians();
    Planar::new(length * direction.sin(), length * direction.cos())
}

/// The inverse of [`project`]: the coordinate at `point` in the plane about `origin`.
///
/// # Errors
///
/// Only if the trigonometry produces a non-finite value, which valid inputs do not.
pub(crate) fn unproject(origin: Coordinate, point: Planar) -> Result<Coordinate, GeoError> {
    let central_angle = point.length() / EARTH_RADIUS_METERS;
    let direction = point.east.atan2(point.north);
    let from_latitude = origin.latitude().to_radians();
    let sin_latitude = from_latitude
        .sin()
        .mul_add(
            central_angle.cos(),
            from_latitude.cos() * central_angle.sin() * direction.cos(),
        )
        .clamp(-1.0, 1.0);
    let to_latitude = sin_latitude.asin();
    let longitude_change = (direction.sin() * central_angle.sin() * from_latitude.cos()).atan2(
        from_latitude
            .sin()
            .mul_add(-sin_latitude, central_angle.cos()),
    );
    let longitude =
        (origin.longitude() + longitude_change.to_degrees() + 180.0).rem_euclid(360.0) - 180.0;
    Coordinate::new(to_latitude.to_degrees().clamp(-90.0, 90.0), longitude)
}

/// A straight segment in the plane.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Segment {
    pub(crate) start: Planar,
    pub(crate) end: Planar,
}

impl Segment {
    /// The shortest distance from `point` to any point of the segment.
    pub(crate) fn distance_to(self, point: Planar) -> f64 {
        let along = self.end.minus(self.start);
        let squared_length = along.dot(along);
        let fraction = if squared_length > 0.0 {
            (point.minus(self.start).dot(along) / squared_length).clamp(0.0, 1.0)
        } else {
            0.0
        };
        point.distance_to(self.start.plus_scaled(along, fraction))
    }

    fn touches_point(self, point: Planar) -> bool {
        self.distance_to(point) <= TOUCH_TOLERANCE_METERS
    }

    /// Whether the closed segments share any point (crossing, touching or overlapping).
    pub(crate) fn intersects(self, other: Self) -> bool {
        let sides_of_other = [orientation(other, self.start), orientation(other, self.end)];
        let sides_of_self = [orientation(self, other.start), orientation(self, other.end)];
        let properly_crosses = opposite(sides_of_other) && opposite(sides_of_self);
        properly_crosses
            || other.touches_point(self.start)
            || other.touches_point(self.end)
            || self.touches_point(other.start)
            || self.touches_point(other.end)
    }

    /// For two consecutive edges sharing `self.end == next.start`: whether the second folds
    /// back over the first, so the ring retraces itself.
    pub(crate) fn folds_back_onto(self, next: Self) -> bool {
        self.touches_point(next.end) || next.touches_point(self.start)
    }
}

fn orientation(segment: Segment, point: Planar) -> f64 {
    segment
        .end
        .minus(segment.start)
        .cross(point.minus(segment.start))
}

fn opposite(sides: [f64; 2]) -> bool {
    let [first, second] = sides;
    (first > 0.0 && second < 0.0) || (first < 0.0 && second > 0.0)
}

/// The closed ring's edges, last vertex back to the first.
pub(crate) fn edges(ring: &[Planar]) -> Vec<Segment> {
    ring.iter()
        .zip(ring.iter().cycle().skip(1))
        .map(|(start, end)| Segment {
            start: *start,
            end: *end,
        })
        .collect()
}

/// Whether every vertex lies on one line (or all coincide), so the ring encloses nothing.
pub(crate) fn is_collinear(ring: &[Planar]) -> bool {
    let Some(first) = ring.first().copied() else {
        return true;
    };
    let farthest = ring.iter().copied().fold(first, |best, candidate| {
        if candidate.distance_to(first) > best.distance_to(first) {
            candidate
        } else {
            best
        }
    });
    let baseline = farthest.minus(first);
    let length = baseline.length();
    if length <= TOUCH_TOLERANCE_METERS {
        return true;
    }
    ring.iter().all(|vertex| {
        (baseline.cross(vertex.minus(first)) / length).abs() <= TOUCH_TOLERANCE_METERS
    })
}

/// Whether the ring's edges cross, touch or retrace each other anywhere other than where
/// consecutive edges meet.
pub(crate) fn is_self_intersecting(ring: &[Planar]) -> bool {
    let edges = edges(ring);
    let last = edges.len().saturating_sub(1);
    edges.iter().enumerate().any(|(first_index, first)| {
        edges
            .iter()
            .enumerate()
            .skip(first_index.saturating_add(1))
            .any(|(second_index, second)| {
                if second_index == first_index.saturating_add(1) {
                    first.folds_back_onto(*second)
                } else if first_index == 0 && second_index == last {
                    second.folds_back_onto(*first)
                } else {
                    first.intersects(*second)
                }
            })
    })
}

/// The shoelace area of the ring, always non-negative.
pub(crate) fn area(ring: &[Planar]) -> f64 {
    let twice_signed: f64 = edges(ring)
        .iter()
        .map(|edge| edge.start.cross(edge.end))
        .sum();
    (twice_signed / 2.0).abs()
}

/// The distance from `point` to the nearest edge of the ring.
pub(crate) fn distance_to_boundary(ring: &[Planar], point: Planar) -> f64 {
    edges(ring)
        .iter()
        .map(|edge| edge.distance_to(point))
        .fold(f64::INFINITY, f64::min)
}

/// Even-odd containment of `point` in the ring's interior; the boundary is the caller's
/// concern.
pub(crate) fn encloses(ring: &[Planar], point: Planar) -> bool {
    edges(ring).iter().fold(false, |inside, edge| {
        let straddles = (edge.start.north > point.north) != (edge.end.north > point.north);
        if straddles {
            let crossing_east = edge.start.east
                + (point.north - edge.start.north) * (edge.end.east - edge.start.east)
                    / (edge.end.north - edge.start.north);
            if point.east < crossing_east {
                return !inside;
            }
        }
        inside
    })
}

/// A circle in the plane.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlanarCircle {
    pub(crate) center: Planar,
    pub(crate) radius: f64,
}

impl PlanarCircle {
    fn covers(self, point: Planar) -> bool {
        self.center.distance_to(point) <= self.radius + TOUCH_TOLERANCE_METERS
    }

    fn on_diameter(first: Planar, second: Planar) -> Self {
        let center = first.plus_scaled(second.minus(first), 0.5);
        Self {
            center,
            radius: center.distance_to(first),
        }
    }

    fn through(first: Planar, second: Planar, third: Planar) -> Self {
        let to_second = second.minus(first);
        let to_third = third.minus(first);
        let determinant = 2.0 * to_second.cross(to_third);
        if determinant.abs() <= f64::EPSILON {
            // Collinear: the circle on the two farthest-apart points covers all three.
            let candidates = [
                Self::on_diameter(first, second),
                Self::on_diameter(first, third),
                Self::on_diameter(second, third),
            ];
            return candidates.into_iter().fold(
                Self::on_diameter(first, first),
                |best, candidate| {
                    if candidate.radius > best.radius {
                        candidate
                    } else {
                        best
                    }
                },
            );
        }
        let second_squared = to_second.dot(to_second);
        let third_squared = to_third.dot(to_third);
        let offset = Planar::new(
            (to_third.north * second_squared - to_second.north * third_squared) / determinant,
            (to_second.east * third_squared - to_third.east * second_squared) / determinant,
        );
        Self {
            center: first.plus_scaled(offset, 1.0),
            radius: offset.length(),
        }
    }
}

/// The smallest circle covering every point (the incremental algorithm of Welzl, without the
/// shuffle: polygons have few vertices, so its cubic worst case does not matter).
pub(crate) fn minimal_enclosing_circle(points: &[Planar]) -> Option<PlanarCircle> {
    let first = points.first().copied()?;
    let mut circle = PlanarCircle::on_diameter(first, first);
    for (outer_index, outer) in points.iter().copied().enumerate() {
        if circle.covers(outer) {
            continue;
        }
        circle = PlanarCircle::on_diameter(outer, outer);
        for (middle_index, middle) in points.iter().copied().enumerate().take(outer_index) {
            if circle.covers(middle) {
                continue;
            }
            circle = PlanarCircle::on_diameter(outer, middle);
            for inner in points.iter().copied().take(middle_index) {
                if !circle.covers(inner) {
                    circle = PlanarCircle::through(outer, middle, inner);
                }
            }
        }
    }
    Some(circle)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use proptest::prelude::*;

    use super::*;

    fn close(actual: f64, expected: f64, tolerance: f64) -> bool {
        (actual - expected).abs() <= tolerance
    }

    fn point(east: f64, north: f64) -> Planar {
        Planar::new(east, north)
    }

    fn ring(points: &[[f64; 2]]) -> Vec<Planar> {
        points
            .iter()
            .map(|&[east, north]| point(east, north))
            .collect()
    }

    #[test]
    fn projection_points_north_and_east() -> Result<(), GeoError> {
        let origin = Coordinate::new(0.0, 0.0)?;
        let north = project(origin, Coordinate::new(0.001, 0.0)?);
        assert!(close(north.east(), 0.0, 1e-9));
        assert!(close(north.north(), 111.195, 0.001));
        let east = project(origin, Coordinate::new(0.0, 0.001)?);
        assert!(close(east.east(), 111.195, 0.001));
        assert!(close(east.north(), 0.0, 1e-9));
        Ok(())
    }

    #[test]
    fn projection_has_no_seam_at_the_antimeridian() -> Result<(), GeoError> {
        let origin = Coordinate::new(10.0, 179.9995)?;
        let across = project(origin, Coordinate::new(10.0, -179.9995)?);
        // 0.001° of longitude at 10° N, eastwards rather than 360° the other way.
        assert!(close(across.east(), 109.5, 0.1));
        let back = unproject(origin, across)?;
        assert!(close(back.longitude(), -179.9995, 1e-9));
        Ok(())
    }

    #[test]
    fn unprojection_wraps_longitude_into_range() -> Result<(), GeoError> {
        let origin = Coordinate::new(0.0, 180.0)?;
        let east = unproject(origin, point(1_000.0, 0.0))?;
        assert!(east.longitude() < 0.0);
        assert!(close(distance(origin, east).get(), 1_000.0, 1e-6));
        Ok(())
    }

    #[test]
    fn projection_from_the_pole_turns_with_longitude() -> Result<(), GeoError> {
        let pole = Coordinate::new(90.0, 0.0)?;
        let towards_greenwich = project(pole, Coordinate::new(89.999, 0.0)?);
        let towards_ninety_east = project(pole, Coordinate::new(89.999, 90.0)?);
        // Both 111 m from the pole, a quarter turn apart.
        assert!(close(towards_greenwich.length(), 111.195, 0.001));
        assert!(close(towards_ninety_east.length(), 111.195, 0.001));
        assert!(close(towards_greenwich.dot(towards_ninety_east), 0.0, 1e-6));
        Ok(())
    }

    #[test]
    fn the_origin_projects_to_zero_and_back() -> Result<(), GeoError> {
        let origin = Coordinate::new(-33.9, 151.2)?;
        assert_eq!(project(origin, origin), point(0.0, 0.0));
        assert_eq!(unproject(origin, point(0.0, 0.0))?, origin);
        Ok(())
    }

    #[test]
    fn planar_debug_is_redacted() {
        assert_eq!(format!("{:?}", point(1.0, 2.0)), "Planar(redacted)");
    }

    #[test]
    fn segment_distance_clamps_to_the_ends() {
        let segment = Segment {
            start: point(0.0, 0.0),
            end: point(10.0, 0.0),
        };
        assert!(close(segment.distance_to(point(5.0, 3.0)), 3.0, 1e-12));
        assert!(close(segment.distance_to(point(-4.0, 3.0)), 5.0, 1e-12));
        assert!(close(segment.distance_to(point(13.0, 4.0)), 5.0, 1e-12));
        let dot = Segment {
            start: point(1.0, 1.0),
            end: point(1.0, 1.0),
        };
        assert!(close(dot.distance_to(point(4.0, 5.0)), 5.0, 1e-12));
    }

    #[test]
    fn crossing_and_touching_segments_intersect_but_parallel_ones_do_not() {
        let horizontal = Segment {
            start: point(0.0, 0.0),
            end: point(10.0, 0.0),
        };
        let crossing = Segment {
            start: point(5.0, -5.0),
            end: point(5.0, 5.0),
        };
        let touching = Segment {
            start: point(5.0, 0.0),
            end: point(5.0, 5.0),
        };
        let parallel = Segment {
            start: point(0.0, 1.0),
            end: point(10.0, 1.0),
        };
        let short = Segment {
            start: point(5.0, 1.0),
            end: point(5.0, 5.0),
        };
        assert!(horizontal.intersects(crossing));
        assert!(crossing.intersects(horizontal));
        assert!(horizontal.intersects(touching));
        assert!(touching.intersects(horizontal));
        assert!(!horizontal.intersects(parallel));
        assert!(!horizontal.intersects(short));
        assert!(!short.intersects(horizontal));
    }

    #[test]
    fn segments_that_would_cross_if_extended_do_not_intersect() {
        let first = Segment {
            start: point(0.0, 0.0),
            end: point(4.0, 4.0),
        };
        let second = Segment {
            start: point(10.0, 0.0),
            end: point(6.0, 4.0),
        };
        assert!(!first.intersects(second));
        assert!(!second.intersects(first));
    }

    #[test]
    fn a_closing_edge_that_retraces_the_first_edge_is_caught() {
        // The closing edge (60, 0) → (0, 0) runs back over the first edge, and the edge
        // before it ends exactly on it, so the polygon is rejected either way; the fold-back
        // check is what catches it when the two edges are the only overlap.
        assert!(is_self_intersecting(&ring(&[
            [0.0, 0.0],
            [100.0, 0.0],
            [100.0, 100.0],
            [60.0, 0.0],
        ])));
        let first = Segment {
            start: point(0.0, 0.0),
            end: point(100.0, 0.0),
        };
        let closing = Segment {
            start: point(60.0, 0.0),
            end: point(0.0, 0.0),
        };
        assert!(closing.folds_back_onto(first));
        let straight_on = Segment {
            start: point(100.0, 0.0),
            end: point(150.0, 0.0),
        };
        assert!(!first.folds_back_onto(straight_on));
    }

    #[test]
    fn a_redundant_vertex_on_a_straight_edge_is_allowed() {
        let square_with_midpoint = ring(&[
            [0.0, 0.0],
            [50.0, 0.0],
            [100.0, 0.0],
            [100.0, 100.0],
            [0.0, 100.0],
        ]);
        assert!(!is_self_intersecting(&square_with_midpoint));
        assert!(close(area(&square_with_midpoint), 10_000.0, 1e-9));
    }

    #[test]
    fn a_slightly_bent_line_is_not_collinear() {
        assert!(is_collinear(&ring(&[
            [0.0, 0.0],
            [50.0, 0.0],
            [100.0, 0.0]
        ])));
        assert!(!is_collinear(&ring(&[
            [0.0, 0.0],
            [50.0, 0.001],
            [100.0, 0.0]
        ])));
        assert!(is_collinear(&[]));
    }

    #[test]
    fn enclosing_circle_of_collinear_points_spans_the_extremes() {
        let circle = PlanarCircle::through(point(0.0, 0.0), point(10.0, 0.0), point(4.0, 0.0));
        assert!(close(circle.radius, 5.0, 1e-12));
        assert!(close(circle.center.east(), 5.0, 1e-12));
        let flipped = PlanarCircle::through(point(4.0, 0.0), point(0.0, 0.0), point(10.0, 0.0));
        assert!(close(flipped.radius, 5.0, 1e-12));
        let other = PlanarCircle::through(point(0.0, 0.0), point(4.0, 0.0), point(10.0, 0.0));
        assert!(close(other.radius, 5.0, 1e-12));
    }

    #[test]
    fn enclosing_circle_through_three_points_is_their_circumcircle() {
        let circle = PlanarCircle::through(point(-1.0, 0.0), point(1.0, 0.0), point(0.0, 1.0));
        assert!(close(circle.radius, 1.0, 1e-12));
        assert!(close(circle.center.east(), 0.0, 1e-12));
        assert!(close(circle.center.north(), 0.0, 1e-12));
    }

    #[test]
    fn minimal_circle_of_nothing_is_none() {
        assert!(minimal_enclosing_circle(&[]).is_none());
    }

    proptest! {
        #[test]
        fn unprojecting_a_projection_returns_the_point(
            latitude in -90.0..=90.0_f64,
            longitude in -180.0..=180.0_f64,
            east in -100_000.0..100_000.0_f64,
            north in -100_000.0..100_000.0_f64,
        ) {
            let origin = Coordinate::new(latitude, longitude)?;
            let target = unproject(origin, point(east, north))?;
            prop_assert!(close(distance(origin, target).get(), east.hypot(north), 1e-6));
            let back = project(origin, target);
            prop_assert!(back.distance_to(point(east, north)) < 1e-3);
        }

        #[test]
        fn minimal_circle_covers_every_point_and_touches_one(
            points in prop::collection::vec((-1_000.0..1_000.0_f64, -1_000.0..1_000.0_f64), 1..20),
        ) {
            let planar: Vec<Planar> = points.iter().map(|&(east, north)| point(east, north)).collect();
            let circle = minimal_enclosing_circle(&planar).ok_or(GeoError::TooFewVertices)?;
            let farthest = planar
                .iter()
                .map(|vertex| circle.center.distance_to(*vertex))
                .fold(0.0, f64::max);
            prop_assert!(farthest <= circle.radius + 1e-6);
            prop_assert!(farthest >= circle.radius - 1e-6);
            let widest = planar
                .iter()
                .flat_map(|first| planar.iter().map(move |second| first.distance_to(*second)))
                .fold(0.0, f64::max);
            prop_assert!(circle.radius >= widest / 2.0 - 1e-6);
            // Never worse than the circle about the first point through the farthest one.
            let naive = planar
                .iter()
                .map(|vertex| planar.first().map_or(0.0, |first| first.distance_to(*vertex)))
                .fold(0.0, f64::max);
            prop_assert!(circle.radius <= naive + 1e-6);
        }
    }
}
