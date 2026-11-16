// SPDX-License-Identifier: AGPL-3.0-only
//! The manifest written for each region (`<stem>.json`) and the catalog a host serves
//! (`catalog.json`), which the app reads to list, download and verify packs.
//!
//! ```json
//! {
//!   "format": 1,
//!   "id": "oceania/australia/victoria/melbourne-cbd",
//!   "name": "Central Melbourne",
//!   "group": "Victoria",
//!   "bbox": [144.95, -37.825, 144.98, -37.805],
//!   "max_zoom": 15,
//!   "build_date": "2026-11-12",
//!   "source": "https://build.protomaps.com/20261112.pmtiles",
//!   "pack": { "file": "oceania.australia.victoria.melbourne-cbd.pmtiles", "size": 3174276, "sha256": "…" },
//!   "index": { "file": "oceania.australia.victoria.melbourne-cbd.places.tsv", "size": 41234, "sha256": "…", "entries": 812 }
//! }
//! ```

use serde::{Deserialize, Serialize};

/// The manifest and catalog format version.
pub const FORMAT: u32 = 1;

/// One downloadable file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Its name, relative to the manifest's URL.
    pub file: String,
    /// Its length in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the whole file, checked by the app after download.
    pub sha256: String,
    /// For an index: how many places it lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<u64>,
}

/// A region's manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// [`FORMAT`].
    pub format: u32,
    /// The region id.
    pub id: String,
    /// The name people see.
    pub name: String,
    /// The parent's name, for grouping sub-regions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// West, south, east, north in degrees: what the pack covers.
    pub bbox: [f64; 4],
    /// The deepest zoom in the pack.
    pub max_zoom: u8,
    /// The date of the Protomaps build it was cut from, `YYYY-MM-DD`.
    pub build_date: String,
    /// The build it was cut from. Manifests the app wrote before 2026-11-14 leave it out.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// The PMTiles pack.
    pub pack: FileEntry,
    /// The place-name search index.
    pub index: FileEntry,
}

/// What a host lists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// [`FORMAT`].
    pub format: u32,
    /// The regions on offer.
    pub regions: Vec<Manifest>,
}

/// The `YYYY-MM-DD` date in a Protomaps daily build URL (`…/20261112.pmtiles`).
#[must_use]
pub fn build_date_of(url: &str) -> Option<String> {
    let name = url.rsplit('/').next()?;
    let digits = name.strip_suffix(".pmtiles")?;
    if digits.len() != 8 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let year = digits.get(0..4)?;
    let month = digits.get(4..6)?;
    let day = digits.get(6..8)?;
    Some(format!("{year}-{month}-{day}"))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn build_dates_come_from_daily_build_names() {
        assert_eq!(
            build_date_of("https://build.protomaps.com/20261112.pmtiles").as_deref(),
            Some("2026-11-12")
        );
        assert_eq!(build_date_of("https://example.org/planet.pmtiles"), None);
    }

    #[test]
    fn manifests_round_trip_through_json() -> Result<(), serde_json::Error> {
        let manifest = Manifest {
            format: FORMAT,
            id: "oceania/australia".into(),
            name: "Australia".into(),
            group: None,
            bbox: [112.0, -44.0, 154.0, -10.0],
            max_zoom: 14,
            build_date: "2026-11-12".into(),
            source: "https://build.protomaps.com/20261112.pmtiles".into(),
            pack: FileEntry {
                file: "oceania.australia.pmtiles".into(),
                size: 1,
                sha256: "00".into(),
                entries: None,
            },
            index: FileEntry {
                file: "oceania.australia.places.tsv".into(),
                size: 2,
                sha256: "11".into(),
                entries: Some(3),
            },
        };
        let text = serde_json::to_string(&manifest)?;
        assert!(!text.contains("group"));
        assert_eq!(serde_json::from_str::<Manifest>(&text)?, manifest);
        Ok(())
    }
}
