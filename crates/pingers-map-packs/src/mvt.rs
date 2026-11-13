// SPDX-License-Identifier: AGPL-3.0-only
//! Just enough of the Mapbox Vector Tile format (protobuf) to read named features and one
//! representative point each: layer names, string properties and the first geometry.
//! Specification: <https://github.com/mapbox/vector-tile-spec/tree/master/2.1>.

use crate::pmtiles::varint;

/// A feature with the properties the index reads.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Feature {
    pub(crate) layer: String,
    pub(crate) name: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) kind_detail: Option<String>,
    /// A point on the feature in tile coordinates, `0..extent`.
    pub(crate) point: TilePoint,
    pub(crate) extent: u32,
}

/// A position inside a tile.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TilePoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

/// A malformed tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("malformed vector tile")]
pub(crate) struct TileError;

/// One protobuf field: its number and either a varint or a length-delimited slice.
enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn next(&mut self) -> Result<Option<(u64, Field<'a>)>, TileError> {
        if self.at >= self.bytes.len() {
            return Ok(None);
        }
        let key = varint(self.bytes, &mut self.at).ok_or(TileError)?;
        let number = key >> 3;
        let field = match key & 7 {
            0 => Field::Varint(varint(self.bytes, &mut self.at).ok_or(TileError)?),
            1 => self.skip(8)?,
            2 => {
                let length = usize::try_from(varint(self.bytes, &mut self.at).ok_or(TileError)?)
                    .map_err(|_| TileError)?;
                let end = self.at.checked_add(length).ok_or(TileError)?;
                let slice = self.bytes.get(self.at..end).ok_or(TileError)?;
                self.at = end;
                Field::Bytes(slice)
            }
            5 => self.skip(4)?,
            _ => return Err(TileError),
        };
        Ok(Some((number, field)))
    }

    fn skip(&mut self, length: usize) -> Result<Field<'a>, TileError> {
        self.at = self.at.checked_add(length).ok_or(TileError)?;
        if self.at > self.bytes.len() {
            return Err(TileError);
        }
        Ok(Field::Fixed)
    }
}

fn packed(bytes: &[u8]) -> Result<Vec<u64>, TileError> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < bytes.len() {
        out.push(varint(bytes, &mut at).ok_or(TileError)?);
    }
    Ok(out)
}

fn zigzag(value: u64) -> i64 {
    let magnitude = i64::try_from(value >> 1).unwrap_or(i64::MAX);
    if value & 1 == 0 {
        magnitude
    } else {
        !magnitude
    }
}

/// Reads a tile's features that have a `name`, from the layers in `layers`.
pub(crate) fn named_features(tile: &[u8], layers: &[&str]) -> Result<Vec<Feature>, TileError> {
    let mut out = Vec::new();
    let mut reader = Reader::new(tile);
    while let Some((number, field)) = reader.next()? {
        if let (3, Field::Bytes(layer)) = (number, field) {
            read_layer(layer, layers, &mut out)?;
        }
    }
    Ok(out)
}

/// A property value: only strings matter here.
fn string_value(bytes: &[u8]) -> Result<Option<String>, TileError> {
    let mut reader = Reader::new(bytes);
    while let Some((number, field)) = reader.next()? {
        if let (1, Field::Bytes(text)) = (number, field) {
            return Ok(Some(String::from_utf8_lossy(text).into_owned()));
        }
    }
    Ok(None)
}

fn read_layer(bytes: &[u8], wanted: &[&str], out: &mut Vec<Feature>) -> Result<(), TileError> {
    let mut name = String::new();
    let mut keys: Vec<String> = Vec::new();
    let mut values: Vec<Option<String>> = Vec::new();
    let mut features: Vec<&[u8]> = Vec::new();
    let mut extent: u32 = 4096;
    let mut reader = Reader::new(bytes);
    while let Some((number, field)) = reader.next()? {
        match (number, field) {
            (1, Field::Bytes(text)) => name = String::from_utf8_lossy(text).into_owned(),
            (2, Field::Bytes(feature)) => features.push(feature),
            (3, Field::Bytes(key)) => keys.push(String::from_utf8_lossy(key).into_owned()),
            (4, Field::Bytes(value)) => values.push(string_value(value)?),
            (5, Field::Varint(value)) => extent = u32::try_from(value).map_err(|_| TileError)?,
            _ => {}
        }
    }
    if !wanted.contains(&name.as_str()) || extent == 0 {
        return Ok(());
    }
    for feature in features {
        if let Some(found) = read_feature(feature, &keys, &values)? {
            out.push(Feature {
                layer: name.clone(),
                name: found.name,
                kind: found.kind,
                kind_detail: found.kind_detail,
                point: found.point,
                extent,
            });
        }
    }
    Ok(())
}

struct Found {
    name: Option<String>,
    kind: Option<String>,
    kind_detail: Option<String>,
    point: TilePoint,
}

fn read_feature(
    bytes: &[u8],
    keys: &[String],
    values: &[Option<String>],
) -> Result<Option<Found>, TileError> {
    let mut tags = Vec::new();
    let mut geometry = Vec::new();
    let mut kind_of_geometry = 0;
    let mut reader = Reader::new(bytes);
    while let Some((number, field)) = reader.next()? {
        match (number, field) {
            (2, Field::Bytes(packed_tags)) => tags = packed(packed_tags)?,
            (3, Field::Varint(value)) => kind_of_geometry = value,
            (4, Field::Bytes(commands)) => geometry = packed(commands)?,
            _ => {}
        }
    }
    let mut found = Found {
        name: None,
        kind: None,
        kind_detail: None,
        point: TilePoint::default(),
    };
    for [key, value] in tags.as_chunks::<2>().0 {
        let key = usize::try_from(*key).ok().and_then(|index| keys.get(index));
        let value = usize::try_from(*value)
            .ok()
            .and_then(|index| values.get(index))
            .cloned()
            .flatten();
        match key.map(String::as_str) {
            Some("name") => found.name = value,
            Some("kind") => found.kind = value,
            Some("kind_detail") => found.kind_detail = value,
            _ => {}
        }
    }
    if found.name.as_deref().is_none_or(str::is_empty) {
        return Ok(None);
    }
    let Some(point) = representative_point(&geometry, kind_of_geometry) else {
        return Ok(None);
    };
    found.point = point;
    Ok(Some(found))
}

/// The first point of a point feature, the middle vertex of a line's first part, or the
/// average of a polygon's outer ring. Good enough to centre a map on.
fn representative_point(commands: &[u64], kind_of_geometry: u64) -> Option<TilePoint> {
    let mut cursor = (0_i64, 0_i64);
    let mut first_part: Vec<(i64, i64)> = Vec::new();
    let mut parts = 0_u32;
    let mut index = 0_usize;
    while index < commands.len() {
        let command = *commands.get(index)?;
        index = index.checked_add(1)?;
        let id = command & 7;
        let count = usize::try_from(command >> 3).ok()?;
        match id {
            1 | 2 => {
                if id == 1 {
                    parts = parts.saturating_add(1);
                }
                for _ in 0..count {
                    let dx = zigzag(*commands.get(index)?);
                    let dy = zigzag(*commands.get(index.checked_add(1)?)?);
                    index = index.checked_add(2)?;
                    cursor = (cursor.0.checked_add(dx)?, cursor.1.checked_add(dy)?);
                    if parts == 1 {
                        first_part.push(cursor);
                    }
                }
            }
            7 => {}
            _ => return None,
        }
    }
    let to_point = |(x, y): (i64, i64)| -> Option<TilePoint> {
        Some(TilePoint {
            x: f64::from(i32::try_from(x).ok()?),
            y: f64::from(i32::try_from(y).ok()?),
        })
    };
    match kind_of_geometry {
        2 => to_point(*first_part.get(first_part.len() / 2)?),
        3 => {
            let points: Vec<TilePoint> = first_part.iter().copied().filter_map(to_point).collect();
            let count = f64::from(u32::try_from(points.len()).ok()?);
            if count == 0.0 {
                return None;
            }
            Some(TilePoint {
                x: points.iter().map(|point| point.x).sum::<f64>() / count,
                y: points.iter().map(|point| point.y).sum::<f64>() / count,
            })
        }
        _ => to_point(*first_part.first()?),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn put_varint(out: &mut Vec<u8>, mut value: u64) {
        loop {
            let byte = u8::try_from(value & 0x7f).unwrap_or(0);
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn put_bytes(out: &mut Vec<u8>, number: u64, bytes: &[u8]) {
        put_varint(out, (number << 3) | 2);
        put_varint(out, u64::try_from(bytes.len()).unwrap_or(0));
        out.extend_from_slice(bytes);
    }

    fn put_packed(out: &mut Vec<u8>, number: u64, values: &[u64]) {
        let mut inner = Vec::new();
        for value in values {
            put_varint(&mut inner, *value);
        }
        put_bytes(out, number, &inner);
    }

    /// A feature for [`tile`]: name and kind indexes into the layer's values, the geometry
    /// type and its commands.
    pub(crate) struct TestFeature<'a> {
        pub(crate) name: &'a str,
        pub(crate) kind: &'a str,
        pub(crate) geometry_type: u64,
        pub(crate) commands: &'a [u64],
    }

    /// Encodes a one-layer tile with `name` and `kind` properties.
    pub(crate) fn tile(layer: &str, features: &[TestFeature<'_>]) -> Vec<u8> {
        let mut body = Vec::new();
        put_varint(&mut body, 15 << 3);
        put_varint(&mut body, 2);
        put_bytes(&mut body, 1, layer.as_bytes());
        put_bytes(&mut body, 3, b"name");
        put_bytes(&mut body, 3, b"kind");
        let mut index = 0_u64;
        for feature in features {
            let mut encoded = Vec::new();
            put_packed(&mut encoded, 2, &[0, index, 1, index.saturating_add(1)]);
            put_varint(&mut encoded, 3 << 3);
            put_varint(&mut encoded, feature.geometry_type);
            put_packed(&mut encoded, 4, feature.commands);
            put_bytes(&mut body, 2, &encoded);
            index = index.saturating_add(2);
        }
        for feature in features {
            for text in [feature.name, feature.kind] {
                let mut value = Vec::new();
                put_bytes(&mut value, 1, text.as_bytes());
                put_bytes(&mut body, 4, &value);
            }
        }
        put_varint(&mut body, 5 << 3);
        put_varint(&mut body, 4096);
        let mut out = Vec::new();
        put_bytes(&mut out, 3, &body);
        out
    }

    #[test]
    fn named_points_and_lines_are_read() -> Result<(), TileError> {
        // MoveTo(1) (+100, +200); a line: MoveTo (10, 10) LineTo x2 (+10, 0) (+10, 0).
        let encoded = tile(
            "roads",
            &[
                TestFeature {
                    name: "Swanston Street",
                    kind: "major_road",
                    geometry_type: 2,
                    commands: &[9, 20, 20, 18, 20, 0, 20, 0],
                },
                TestFeature {
                    name: "",
                    kind: "minor_road",
                    geometry_type: 2,
                    commands: &[9, 2, 2],
                },
            ],
        );
        let features = named_features(&encoded, &["roads"])?;
        assert_eq!(features.len(), 1);
        let first = features.first().ok_or(TileError)?;
        assert_eq!(first.name.as_deref(), Some("Swanston Street"));
        assert_eq!(first.kind.as_deref(), Some("major_road"));
        assert_eq!(first.point, TilePoint { x: 20.0, y: 10.0 });
        assert_eq!(first.extent, 4096);
        assert!(named_features(&encoded, &["pois"])?.is_empty());
        Ok(())
    }

    #[test]
    fn zigzag_decodes_negative_values() {
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(1), -1);
        assert_eq!(zigzag(2), 1);
        assert_eq!(zigzag(3), -2);
    }

    #[test]
    fn truncated_tiles_are_errors() {
        let encoded = tile(
            "pois",
            &[TestFeature {
                name: "Cafe",
                kind: "cafe",
                geometry_type: 1,
                commands: &[9, 2, 2],
            }],
        );
        let cut = encoded.get(..encoded.len() - 3).unwrap_or_default();
        assert_eq!(named_features(cut, &["pois"]), Err(TileError));
    }
}
