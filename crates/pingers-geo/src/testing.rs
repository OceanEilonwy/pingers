// SPDX-License-Identifier: AGPL-3.0-only
//! Builders shared by this crate's unit tests.

use crate::plane::{self, Planar};
use crate::{Coordinate, GeoError, Polygon};

/// The point `east` and `north` metres from `origin`.
pub(crate) fn offset(origin: Coordinate, east: f64, north: f64) -> Result<Coordinate, GeoError> {
    plane::unproject(origin, Planar::new(east, north))
}

/// A polygon whose vertices are given as metre offsets from `origin`.
pub(crate) fn polygon_around(
    origin: Coordinate,
    offsets: &[[f64; 2]],
) -> Result<Polygon, GeoError> {
    Polygon::new(
        offsets
            .iter()
            .map(|&[east, north]| offset(origin, east, north))
            .collect::<Result<Vec<_>, _>>()?,
    )
}
