// SPDX-License-Identifier: AGPL-3.0-only
//! Conversions between domain types and column values, in one place.
//!
//! SQLite integers are `i64`; the protocol's counters and times are unsigned. Values past
//! `i64::MAX` are refused with [`StoreError::OutOfRange`] rather than wrapped.

use pingers_geo::{Circle, Coordinate, Fence, Meters, Polygon};
use pingers_time::{Bucket, UnixSeconds};
use rusqlite::Row;
use rusqlite::types::ValueRef;
use zeroize::Zeroizing;

use crate::StoreError;

/// An unsigned value as a column integer.
pub(crate) fn to_int(value: u64, field: &'static str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::OutOfRange { field })
}

/// A column integer back to an unsigned value.
pub(crate) fn from_int(value: i64, field: &'static str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt { field })
}

/// A column integer as a narrower unsigned type.
pub(crate) fn narrow<Target: TryFrom<i64>>(
    value: i64,
    field: &'static str,
) -> Result<Target, StoreError> {
    Target::try_from(value).map_err(|_| StoreError::Corrupt { field })
}

pub(crate) fn time(instant: UnixSeconds, field: &'static str) -> Result<i64, StoreError> {
    to_int(instant.get(), field)
}

pub(crate) fn read_time(
    row: &Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<UnixSeconds, StoreError> {
    Ok(UnixSeconds::new(from_int(row.get(index)?, field)?))
}

pub(crate) fn read_optional_time(
    row: &Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<Option<UnixSeconds>, StoreError> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| from_int(value, field).map(UnixSeconds::new))
        .transpose()
}

pub(crate) fn bucket(value: Bucket, field: &'static str) -> Result<i64, StoreError> {
    to_int(value.index(), field)
}

pub(crate) fn read_bucket(
    row: &Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<Bucket, StoreError> {
    Bucket::from_index(from_int(row.get(index)?, field)?).map_err(|_| StoreError::Corrupt { field })
}

/// A fixed-length BLOB column.
pub(crate) fn read_array<const N: usize>(
    row: &Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<[u8; N], StoreError> {
    match row.get_ref(index)? {
        ValueRef::Blob(bytes) => {
            <[u8; N]>::try_from(bytes).map_err(|_| StoreError::Corrupt { field })
        }
        ValueRef::Null | ValueRef::Integer(_) | ValueRef::Real(_) | ValueRef::Text(_) => {
            Err(StoreError::Corrupt { field })
        }
    }
}

/// A nullable fixed-length BLOB column.
pub(crate) fn read_optional_array<const N: usize>(
    row: &Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<Option<[u8; N]>, StoreError> {
    match row.get_ref(index)? {
        ValueRef::Null => Ok(None),
        ValueRef::Blob(_) | ValueRef::Integer(_) | ValueRef::Real(_) | ValueRef::Text(_) => {
            read_array(row, index, field).map(Some)
        }
    }
}

/// A fixed-length secret BLOB column, copied straight from SQLite's buffer into memory that is
/// zeroed on drop.
pub(crate) fn read_secret<const N: usize>(
    row: &Row<'_>,
    index: usize,
    field: &'static str,
) -> Result<Zeroizing<[u8; N]>, StoreError> {
    let mut secret = Zeroizing::new([0_u8; N]);
    match row.get_ref(index)? {
        ValueRef::Blob(bytes) if bytes.len() == N => {
            secret.copy_from_slice(bytes);
            Ok(secret)
        }
        ValueRef::Blob(_)
        | ValueRef::Null
        | ValueRef::Integer(_)
        | ValueRef::Real(_)
        | ValueRef::Text(_) => Err(StoreError::Corrupt { field }),
    }
}

/// Fence encoding version. Bump and keep decoding the old one if the layout ever changes.
const FENCE_VERSION: u8 = 1;
const FENCE_CIRCLE: u8 = 0;
const FENCE_POLYGON: u8 = 1;

/// Encodes a fence for `spot.geometry` and `exclusion_zone.geometry`:
/// `version || kind || payload`, where a circle is `be_f64(lat) || be_f64(lon) ||
/// be_f64(radius)` and a polygon is `be16(count) || (be_f64(lat) || be_f64(lon))*`.
pub(crate) fn encode_fence(fence: &Fence) -> Result<Vec<u8>, StoreError> {
    let mut bytes = vec![FENCE_VERSION];
    match fence {
        Fence::Circle(circle) => {
            bytes.push(FENCE_CIRCLE);
            push_coordinate(&mut bytes, circle.center());
            bytes.extend_from_slice(&circle.radius().get().to_be_bytes());
        }
        Fence::Polygon(polygon) => {
            bytes.push(FENCE_POLYGON);
            let count =
                u16::try_from(polygon.vertices().len()).map_err(|_| StoreError::OutOfRange {
                    field: "polygon vertex count",
                })?;
            bytes.extend_from_slice(&count.to_be_bytes());
            for vertex in polygon.vertices() {
                push_coordinate(&mut bytes, *vertex);
            }
        }
    }
    Ok(bytes)
}

fn push_coordinate(bytes: &mut Vec<u8>, coordinate: Coordinate) {
    bytes.extend_from_slice(&coordinate.latitude().to_be_bytes());
    bytes.extend_from_slice(&coordinate.longitude().to_be_bytes());
}

/// Decodes [`encode_fence`]'s output, re-validating through the geo constructors.
pub(crate) fn decode_fence(bytes: &[u8], field: &'static str) -> Result<Fence, StoreError> {
    let corrupt = || StoreError::Corrupt { field };
    let mut reader = Reader { rest: bytes };
    if reader.byte().ok_or_else(corrupt)? != FENCE_VERSION {
        return Err(corrupt());
    }
    let fence = match reader.byte().ok_or_else(corrupt)? {
        FENCE_CIRCLE => {
            let center = reader.coordinate().ok_or_else(corrupt)?;
            let radius = Meters::new(reader.float().ok_or_else(corrupt)?).map_err(|_| corrupt())?;
            Fence::Circle(Circle::new(center, radius).map_err(|_| corrupt())?)
        }
        FENCE_POLYGON => {
            let count = reader.u16().ok_or_else(corrupt)?;
            let vertices = (0..count)
                .map(|_| reader.coordinate().ok_or_else(corrupt))
                .collect::<Result<Vec<_>, _>>()?;
            Fence::Polygon(Polygon::new(vertices).map_err(|_| corrupt())?)
        }
        _ => return Err(corrupt()),
    };
    if reader.rest.is_empty() {
        Ok(fence)
    } else {
        Err(corrupt())
    }
}

struct Reader<'bytes> {
    rest: &'bytes [u8],
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, tail) = self.rest.split_at_checked(N)?;
        self.rest = tail;
        head.try_into().ok()
    }

    fn byte(&mut self) -> Option<u8> {
        self.take::<1>().map(u8::from_be_bytes)
    }

    fn u16(&mut self) -> Option<u16> {
        self.take::<2>().map(u16::from_be_bytes)
    }

    fn float(&mut self) -> Option<f64> {
        self.take::<8>().map(f64::from_be_bytes)
    }

    fn coordinate(&mut self) -> Option<Coordinate> {
        let latitude = self.float()?;
        let longitude = self.float()?;
        Coordinate::new(latitude, longitude).ok()
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::{TestError, circle};

    #[test]
    fn circle_round_trips_exactly() -> Result<(), TestError> {
        let fence = circle(48.8566, 2.3522, 150.0)?;
        let bytes = encode_fence(&fence)?;
        assert_eq!(bytes.len(), 26);
        assert_eq!(decode_fence(&bytes, "test")?, fence);
        Ok(())
    }

    #[test]
    fn fence_with_trailing_bytes_is_corrupt() -> Result<(), TestError> {
        let mut bytes = encode_fence(&circle(1.0, 2.0, 100.0)?)?;
        bytes.push(0);
        assert!(matches!(
            decode_fence(&bytes, "test"),
            Err(StoreError::Corrupt { field: "test" })
        ));
        Ok(())
    }

    #[test]
    fn fence_with_unknown_version_or_kind_is_corrupt() -> Result<(), TestError> {
        let good = encode_fence(&circle(1.0, 2.0, 100.0)?)?;
        let mut wrong_version = good.clone();
        wrong_version.splice(0..1, [2]);
        let mut wrong_kind = good;
        wrong_kind.splice(1..2, [7]);
        assert!(decode_fence(&wrong_version, "test").is_err());
        assert!(decode_fence(&wrong_kind, "test").is_err());
        Ok(())
    }

    #[test]
    fn truncated_fence_is_corrupt() -> Result<(), TestError> {
        let bytes = encode_fence(&circle(1.0, 2.0, 100.0)?)?;
        for length in 0..bytes.len() {
            assert!(decode_fence(bytes.get(..length).unwrap_or_default(), "test").is_err());
        }
        Ok(())
    }

    #[test]
    fn invalid_geometry_is_corrupt_not_accepted() -> Result<(), TestError> {
        let mut bytes = encode_fence(&circle(1.0, 2.0, 100.0)?)?;
        // Latitude 91 degrees.
        bytes.splice(2..10, 91.0_f64.to_be_bytes());
        assert!(decode_fence(&bytes, "test").is_err());
        let mut zero_radius = encode_fence(&circle(1.0, 2.0, 100.0)?)?;
        zero_radius.splice(18..26, 0.0_f64.to_be_bytes());
        assert!(decode_fence(&zero_radius, "test").is_err());
        Ok(())
    }

    #[test]
    fn polygon_with_too_few_vertices_is_corrupt() {
        // version, polygon, count 0: rejected by pingers-geo's constructor whatever it does.
        assert!(decode_fence(&[1, 1, 0, 0], "test").is_err());
    }

    #[test]
    fn integers_past_i64_are_out_of_range() {
        assert!(matches!(
            to_int(u64::MAX, "field"),
            Err(StoreError::OutOfRange { field: "field" })
        ));
        assert!(matches!(to_int(7, "field"), Ok(7)));
        assert!(matches!(
            from_int(-1, "field"),
            Err(StoreError::Corrupt { field: "field" })
        ));
    }
}
