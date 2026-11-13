// SPDX-License-Identifier: AGPL-3.0-only
//! A small PMTiles v3 reader: the header, the directories and the tiles, from a seekable file.
//! Only what the search index needs; the specification is
//! <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md>.

use std::io::{Read, Seek, SeekFrom};

use flate2::read::GzDecoder;

/// Why a pack could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// The file is not PMTiles version 3.
    #[error("not a PMTiles v3 archive")]
    NotPmtiles,
    /// A compression the reader does not implement (only none and gzip are).
    #[error("unsupported compression {0}")]
    Compression(u8),
    /// A directory or varint ran past its end, or nests too deep.
    #[error("malformed directory")]
    Directory,
    /// Reading the file failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The fields of the 127-byte header that the tool uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    root_offset: u64,
    root_length: u64,
    leaf_offset: u64,
    tile_data_offset: u64,
    internal_compression: u8,
    tile_compression: u8,
    /// 1 is Mapbox Vector Tiles.
    pub tile_type: u8,
    /// The deepest zoom in the archive.
    pub max_zoom: u8,
    /// What the archive covers.
    pub bounds: Bounds,
}

/// A bounding box in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    /// Western edge, degrees east.
    pub west: f64,
    /// Southern edge, degrees north.
    pub south: f64,
    /// Eastern edge, degrees east.
    pub east: f64,
    /// Northern edge, degrees north.
    pub north: f64,
}

/// One tile's place in the archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRef {
    /// Which tile.
    pub tile: TileId,
    offset: u64,
    length: u64,
}

/// A tile address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileId {
    /// Zoom.
    pub z: u8,
    /// Column, from the west.
    pub x: u32,
    /// Row, from the north.
    pub y: u32,
}

const HEADER_LENGTH: usize = 127;
const COMPRESSION_NONE: u8 = 1;
const COMPRESSION_GZIP: u8 = 2;
/// The specification allows a root and one level of leaves; extracts never nest deeper.
const MAX_DEPTH: u8 = 3;
/// Larger than any sane directory or vector tile, small enough that a bad length cannot make
/// the tool allocate gigabytes.
const MAX_BLOB: u64 = 64 * 1024 * 1024;

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    let slice = bytes.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(slice.try_into().ok()?))
}

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    let slice = bytes.get(at..at.checked_add(4)?)?;
    Some(i32::from_le_bytes(slice.try_into().ok()?))
}

fn degrees(e7: i32) -> f64 {
    f64::from(e7) / 10_000_000.0
}

/// Parses the header from the first 127 bytes.
///
/// # Errors
///
/// [`PackError::NotPmtiles`] unless they are a PMTiles v3 header.
pub fn parse_header(bytes: &[u8]) -> Result<Header, PackError> {
    let parse = || -> Option<Header> {
        if bytes.get(0..7)? != b"PMTiles" || *bytes.get(7)? != 3 {
            return None;
        }
        Some(Header {
            root_offset: u64_at(bytes, 8)?,
            root_length: u64_at(bytes, 16)?,
            leaf_offset: u64_at(bytes, 40)?,
            tile_data_offset: u64_at(bytes, 56)?,
            internal_compression: *bytes.get(97)?,
            tile_compression: *bytes.get(98)?,
            tile_type: *bytes.get(99)?,
            max_zoom: *bytes.get(101)?,
            bounds: Bounds {
                west: degrees(i32_at(bytes, 102)?),
                south: degrees(i32_at(bytes, 106)?),
                east: degrees(i32_at(bytes, 110)?),
                north: degrees(i32_at(bytes, 114)?),
            },
        })
    };
    parse().ok_or(PackError::NotPmtiles)
}

/// An open archive.
pub struct Pack<F> {
    file: F,
    /// The archive's header.
    pub header: Header,
}

impl<F: Read + Seek> Pack<F> {
    /// Reads the header of `file`.
    ///
    /// # Errors
    ///
    /// [`PackError`] if it is not a PMTiles v3 archive with a supported compression.
    pub fn open(mut file: F) -> Result<Self, PackError> {
        let mut bytes = [0_u8; HEADER_LENGTH];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut bytes)?;
        let header = parse_header(&bytes)?;
        for compression in [header.internal_compression, header.tile_compression] {
            if compression != COMPRESSION_NONE && compression != COMPRESSION_GZIP {
                return Err(PackError::Compression(compression));
            }
        }
        Ok(Self { file, header })
    }

    fn read_blob(
        &mut self,
        offset: u64,
        length: u64,
        compression: u8,
    ) -> Result<Vec<u8>, PackError> {
        if length > MAX_BLOB {
            return Err(PackError::Directory);
        }
        let mut raw = Vec::new();
        self.file.seek(SeekFrom::Start(offset))?;
        (&mut self.file).take(length).read_to_end(&mut raw)?;
        if compression == COMPRESSION_GZIP {
            let mut out = Vec::new();
            GzDecoder::new(raw.as_slice())
                .take(MAX_BLOB)
                .read_to_end(&mut out)?;
            Ok(out)
        } else {
            Ok(raw)
        }
    }

    /// Every tile in the archive, in directory order.
    ///
    /// # Errors
    ///
    /// [`PackError`] if a directory cannot be read or is malformed.
    pub fn tiles(&mut self) -> Result<Vec<TileRef>, PackError> {
        let mut out = Vec::new();
        let root = Span {
            offset: self.header.root_offset,
            length: self.header.root_length,
        };
        self.walk(root, 0, &mut out)?;
        Ok(out)
    }

    fn walk(&mut self, span: Span, depth: u8, out: &mut Vec<TileRef>) -> Result<(), PackError> {
        if depth > MAX_DEPTH {
            return Err(PackError::Directory);
        }
        let bytes = self.read_blob(span.offset, span.length, self.header.internal_compression)?;
        for entry in parse_directory(&bytes)? {
            if entry.run_length == 0 {
                let leaf = self
                    .header
                    .leaf_offset
                    .checked_add(entry.offset)
                    .ok_or(PackError::Directory)?;
                self.walk(
                    Span {
                        offset: leaf,
                        length: entry.length,
                    },
                    depth.saturating_add(1),
                    out,
                )?;
            } else {
                let mut id = entry.tile_id;
                for _ in 0..entry.run_length {
                    let tile = tile_from_id(id).ok_or(PackError::Directory)?;
                    out.push(TileRef {
                        tile,
                        offset: entry.offset,
                        length: entry.length,
                    });
                    id = id.checked_add(1).ok_or(PackError::Directory)?;
                }
            }
        }
        Ok(())
    }

    /// The decompressed bytes of one tile.
    ///
    /// # Errors
    ///
    /// [`PackError`] if it cannot be read or decompressed.
    pub fn tile(&mut self, tile: TileRef) -> Result<Vec<u8>, PackError> {
        let offset = self
            .header
            .tile_data_offset
            .checked_add(tile.offset)
            .ok_or(PackError::Directory)?;
        self.read_blob(offset, tile.length, self.header.tile_compression)
    }
}

#[derive(Clone, Copy)]
struct Span {
    offset: u64,
    length: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Entry {
    tile_id: u64,
    run_length: u64,
    length: u64,
    offset: u64,
}

/// Reads an unsigned LEB128 varint at `*at`, moving `*at` past it.
pub(crate) fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value: u64 = 0;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at = at.checked_add(1)?;
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn parse_directory(bytes: &[u8]) -> Result<Vec<Entry>, PackError> {
    let mut at = 0;
    let mut next = || varint(bytes, &mut at).ok_or(PackError::Directory);
    let count = usize::try_from(next()?).map_err(|_| PackError::Directory)?;
    // Every entry takes at least four bytes, so a count beyond that is malformed.
    if count > bytes.len() {
        return Err(PackError::Directory);
    }
    let mut entries = vec![Entry::default(); count];
    let mut last_id: u64 = 0;
    for entry in &mut entries {
        last_id = last_id.checked_add(next()?).ok_or(PackError::Directory)?;
        entry.tile_id = last_id;
    }
    for entry in &mut entries {
        entry.run_length = next()?;
    }
    for entry in &mut entries {
        entry.length = next()?;
    }
    let mut previous: Option<Entry> = None;
    for entry in &mut entries {
        let value = next()?;
        entry.offset = match (value, previous) {
            (0, Some(before)) => before
                .offset
                .checked_add(before.length)
                .ok_or(PackError::Directory)?,
            _ => value.checked_sub(1).ok_or(PackError::Directory)?,
        };
        previous = Some(*entry);
    }
    Ok(entries)
}

/// The tile a PMTiles tile id addresses: ids count tiles zoom by zoom, each zoom along a
/// Hilbert curve.
#[must_use]
pub fn tile_from_id(id: u64) -> Option<TileId> {
    let mut base: u64 = 0;
    for z in 0..=31_u8 {
        let count = 1_u64.checked_shl(u32::from(z).checked_mul(2)?)?;
        let end = base.checked_add(count)?;
        if id < end {
            let (x, y) = hilbert_to_xy(z, id.checked_sub(base)?)?;
            return Some(TileId { z, x, y });
        }
        base = end;
    }
    None
}

/// The inverse of [`tile_from_id`].
#[cfg(test)]
#[expect(
    clippy::many_single_char_names,
    reason = "the Hilbert curve's usual names"
)]
pub(crate) fn id_from_tile(tile: TileId) -> Option<u64> {
    let mut base: u64 = 0;
    for z in 0..tile.z {
        base = base.checked_add(1_u64.checked_shl(u32::from(z).checked_mul(2)?)?)?;
    }
    let n = 1_u64.checked_shl(u32::from(tile.z))?;
    let (mut x, mut y) = (u64::from(tile.x), u64::from(tile.y));
    let mut d: u64 = 0;
    let mut s = n.checked_shr(1)?;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        d = d.checked_add(s.checked_mul(s)?.checked_mul(rx.checked_mul(3)? ^ ry)?)?;
        rotate(n, &mut x, &mut y, Quadrant { rx, ry })?;
        s = s.checked_shr(1)?;
    }
    base.checked_add(d)
}

#[derive(Clone, Copy)]
struct Quadrant {
    rx: u64,
    ry: u64,
}

fn rotate(n: u64, x: &mut u64, y: &mut u64, quadrant: Quadrant) -> Option<()> {
    if quadrant.ry == 0 {
        if quadrant.rx == 1 {
            *x = n.checked_sub(1)?.checked_sub(*x)?;
            *y = n.checked_sub(1)?.checked_sub(*y)?;
        }
        core::mem::swap(x, y);
    }
    Some(())
}

#[expect(
    clippy::many_single_char_names,
    reason = "the Hilbert curve's usual names"
)]
fn hilbert_to_xy(z: u8, position: u64) -> Option<(u32, u32)> {
    let n = 1_u64.checked_shl(u32::from(z))?;
    let (mut x, mut y) = (0_u64, 0_u64);
    let mut t = position;
    let mut s: u64 = 1;
    while s < n {
        let rx = 1 & (t / 2);
        let ry = 1 & (t ^ rx);
        rotate(s, &mut x, &mut y, Quadrant { rx, ry })?;
        x = x.checked_add(s.checked_mul(rx)?)?;
        y = y.checked_add(s.checked_mul(ry)?)?;
        t /= 4;
        s = s.checked_mul(2)?;
    }
    Some((u32::try_from(x).ok()?, u32::try_from(y).ok()?))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn tile_ids_follow_the_specification() {
        // Values from the PMTiles v3 specification and its reference implementation.
        assert_eq!(tile_from_id(0), Some(TileId { z: 0, x: 0, y: 0 }));
        assert_eq!(tile_from_id(1), Some(TileId { z: 1, x: 0, y: 0 }));
        assert_eq!(tile_from_id(2), Some(TileId { z: 1, x: 0, y: 1 }));
        assert_eq!(tile_from_id(3), Some(TileId { z: 1, x: 1, y: 1 }));
        assert_eq!(tile_from_id(4), Some(TileId { z: 1, x: 1, y: 0 }));
        assert_eq!(tile_from_id(5), Some(TileId { z: 2, x: 0, y: 0 }));
        assert_eq!(id_from_tile(TileId { z: 3, x: 3, y: 3 }), Some(31));
        assert_eq!(
            id_from_tile(TileId { z: 20, x: 0, y: 0 }),
            Some(366_503_875_925)
        );
    }

    #[test]
    fn tile_ids_round_trip() {
        for z in 0..=8_u8 {
            let n = 1_u32.checked_shl(u32::from(z)).unwrap_or(0);
            for x in (0..n).step_by(3) {
                for y in (0..n).step_by(5) {
                    let tile = TileId { z, x, y };
                    let id = id_from_tile(tile);
                    assert_eq!(id.and_then(tile_from_id), Some(tile));
                }
            }
        }
    }

    #[test]
    fn varints_decode_and_stop_at_the_end() {
        let mut at = 0;
        assert_eq!(varint(&[0xac, 0x02, 0x05], &mut at), Some(300));
        assert_eq!(varint(&[0xac, 0x02, 0x05], &mut at), Some(5));
        assert_eq!(varint(&[0xac, 0x02, 0x05], &mut at), None);
        assert_eq!(varint(&[0x80], &mut 0), None);
    }

    #[test]
    fn directories_decode_offsets_relative_to_the_previous_entry() -> Result<(), PackError> {
        // Two entries: ids 5 and 6, run lengths 1 and 1, lengths 10 and 20, offsets 0 then
        // "follows the previous one" (encoded 0).
        let bytes = [2, 5, 1, 1, 1, 10, 20, 1, 0];
        let entries = parse_directory(&bytes)?;
        assert_eq!(
            entries,
            vec![
                Entry {
                    tile_id: 5,
                    run_length: 1,
                    length: 10,
                    offset: 0
                },
                Entry {
                    tile_id: 6,
                    run_length: 1,
                    length: 20,
                    offset: 10
                },
            ]
        );
        assert!(parse_directory(&[200, 1]).is_err());
        Ok(())
    }

    #[test]
    fn headers_are_rejected_unless_v3() {
        assert!(matches!(
            parse_header(&[0; 127]),
            Err(PackError::NotPmtiles)
        ));
    }
}
