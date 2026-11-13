// SPDX-License-Identifier: AGPL-3.0-only
//! Region map packs.
//!
//! Pingers renders maps from whole-region PMTiles packs that phones download once and keep, so
//! no tile request ever tells anyone where a user is looking (design B.12, "Map tiles"). This
//! crate is what the operator tool `pingers-tiles` and the map host `pingers-map-host` share:
//!
//! - [`regions`]: the region list (hierarchical ids, so large countries split into
//!   sub-regions) and a host's allow-list with per-region zoom caps;
//! - [`extract`]: cutting packs from the Protomaps build with `pmtiles extract`, estimating
//!   their size with `--dry-run`, and publishing each with its index and manifest;
//! - [`index`]: the place-name search index, read from the pack's own labels;
//! - [`manifest`]: the manifest and catalog formats the app reads;
//! - [`size`]: byte sizes and the disk budget check.
#![forbid(unsafe_code)]

pub mod extract;
pub mod index;
pub mod manifest;
mod mvt;
pub mod pmtiles;
pub mod regions;
pub mod size;

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::io::Cursor;

    use crate::index::{PlaceKind, places_in};
    use crate::mvt::tests::{TestFeature, tile};
    use crate::pmtiles::{Pack, PackError};

    /// A one-tile PMTiles v3 archive (zoom 0, no compression) holding `tile_bytes`.
    fn archive(tile_bytes: &[u8]) -> Vec<u8> {
        // Root directory: one entry, tile id 0, run length 1, its length, offset 0 (encoded 1).
        let length = u8::try_from(tile_bytes.len()).unwrap_or(0);
        let directory = [1, 0, 1, length, 1];
        let root_offset: u64 = 127;
        let data_offset = root_offset.saturating_add(5);
        let mut header = vec![0_u8; 127];
        header.splice(0..8, b"PMTiles\x03".iter().copied());
        let mut put = |at: usize, value: u64| {
            header.splice(at..at.saturating_add(8), value.to_le_bytes());
        };
        put(8, root_offset);
        put(16, 5);
        put(40, data_offset);
        put(56, data_offset);
        put(64, u64::from(length));
        let mut bytes = header;
        if let Some(fields) = bytes.get_mut(97..102) {
            fields.copy_from_slice(&[1, 1, 1, 0, 0]);
        }
        // Bounds: the whole world, in degrees times 10^7.
        for (at, degrees) in [(102_usize, -180_i32), (106, -85), (110, 180), (114, 85)] {
            let e7: i32 = i32::saturating_mul(degrees, 10_000_000);
            bytes.splice(at..at.saturating_add(4), e7.to_le_bytes());
        }
        bytes.extend_from_slice(&directory);
        bytes.extend_from_slice(tile_bytes);
        bytes
    }

    #[test]
    fn places_are_read_from_a_pack() -> Result<(), PackError> {
        let tile_bytes = tile(
            "pois",
            &[TestFeature {
                name: "Flinders Street Station",
                kind: "train_station",
                geometry_type: 1,
                commands: &[9, 4096, 4096],
            }],
        );
        let mut pack = Pack::open(Cursor::new(archive(&tile_bytes)))?;
        assert_eq!(pack.header.max_zoom, 0);
        let places = places_in(&mut pack)?;
        assert_eq!(places.len(), 1);
        let place = places.first().ok_or(PackError::Directory)?;
        assert_eq!(place.name, "Flinders Street Station");
        assert_eq!(place.kind, PlaceKind::Poi);
        // (2048, 2048) of 4096 on the single zoom-0 tile is the map's centre.
        assert!(place.latitude.abs() < 1e-9 && place.longitude.abs() < 1e-9);
        Ok(())
    }
}
