// SPDX-License-Identifier: AGPL-3.0-only
//! Manifests and catalog entries, checked once (style guide §1): a [`RegionManifest`] names
//! files that stay inside the maps directory and carries checksums that parse.
//!
//! The wire format is pingers-map-packs' (`manifest.rs`); manifests the Android shell wrote
//! before the core took over downloads lack `source` and parse the same.

use pingers_crypto::ContentDigest;
use pingers_map_packs::manifest::{FORMAT, FileEntry, Manifest};
use serde_json::Value;

use crate::MapsError;

/// A region id's file-name stem (`oceania.australia.victoria`): lowercase `[a-z0-9-]`
/// segments joined by `.`, so a file named from it cannot leave the maps directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StemName(String);

impl StemName {
    /// The stem of region id `id` (`/` becomes `.`).
    ///
    /// # Errors
    ///
    /// [`MapsError::BadAnswer`] unless every segment is lowercase `[a-z0-9-]`, not starting
    /// with `-`.
    pub fn of_region(id: &str) -> Result<Self, MapsError> {
        let safe = !id.is_empty()
            && id.split('/').all(|segment| {
                segment
                    .bytes()
                    .next()
                    .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
                    && segment.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            });
        if safe {
            Ok(Self(id.replace('/', ".")))
        } else {
            Err(MapsError::BadAnswer)
        }
    }

    /// The stem.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The manifest's file name.
    #[must_use]
    pub fn manifest_file(&self) -> String {
        format!("{}.json", self.0)
    }

    /// The pack's file name.
    #[must_use]
    pub fn pack_file(&self) -> String {
        format!("{}.pmtiles", self.0)
    }

    /// The place index's file name.
    #[must_use]
    pub fn index_file(&self) -> String {
        format!("{}.places.tsv", self.0)
    }
}

/// West, south, east, north in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundingBox {
    /// Western edge.
    pub west: f64,
    /// Southern edge.
    pub south: f64,
    /// Eastern edge.
    pub east: f64,
    /// Northern edge.
    pub north: f64,
}

impl BoundingBox {
    fn parse(values: [f64; 4]) -> Result<Self, MapsError> {
        let [west, south, east, north] = values;
        let valid = values.iter().all(|value| value.is_finite())
            && (-180.0..=180.0).contains(&west)
            && (-180.0..=180.0).contains(&east)
            && (-90.0..=90.0).contains(&south)
            && (-90.0..=90.0).contains(&north)
            && south <= north
            && west <= east;
        if valid {
            Ok(Self {
                west,
                south,
                east,
                north,
            })
        } else {
            Err(MapsError::BadAnswer)
        }
    }

    fn values(self) -> [f64; 4] {
        [self.west, self.south, self.east, self.north]
    }
}

/// One file of a region: its name, length and checksum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackFile {
    /// The file name in the maps directory and on the host.
    pub file: String,
    /// Its length in bytes.
    pub size: u64,
    /// The SHA-256 of its bytes.
    pub sha256: ContentDigest,
    /// For an index: how many places it lists.
    pub entries: Option<u64>,
}

impl PackFile {
    fn parse(entry: &FileEntry, expected: String) -> Result<Self, MapsError> {
        if entry.file != expected {
            return Err(MapsError::BadAnswer);
        }
        Ok(Self {
            file: expected,
            size: entry.size,
            sha256: ContentDigest::from_hex(&entry.sha256).map_err(|_| MapsError::BadAnswer)?,
            entries: entry.entries,
        })
    }

    fn to_entry(&self) -> FileEntry {
        FileEntry {
            file: self.file.clone(),
            size: self.size,
            sha256: self.sha256.to_hex(),
            entries: self.entries,
        }
    }
}

/// A region's manifest, checked: its files are named from its id and stay in the maps
/// directory, and its checksums parse.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionManifest {
    /// The region id (`oceania/australia/victoria`).
    pub id: String,
    /// The stem its files share.
    pub stem: StemName,
    /// The name people see.
    pub name: String,
    /// The parent's name, for grouping sub-regions.
    pub group: Option<String>,
    /// What the pack covers.
    pub bbox: BoundingBox,
    /// The deepest zoom in the pack.
    pub max_zoom: u8,
    /// `YYYY-MM-DD` of the Protomaps build.
    pub build_date: String,
    /// The build it was cut from, if the manifest says.
    pub source: Option<String>,
    /// The PMTiles pack.
    pub pack: PackFile,
    /// The place-name search index.
    pub index: PackFile,
}

impl RegionManifest {
    /// Checks a manifest as pingers-map-packs reads it.
    ///
    /// # Errors
    ///
    /// [`MapsError::BadAnswer`] for another format, an unsafe id, file names that are not the
    /// id's, a checksum that does not parse, or an impossible bounding box.
    pub fn parse(manifest: &Manifest) -> Result<Self, MapsError> {
        if manifest.format != FORMAT {
            return Err(MapsError::BadAnswer);
        }
        let stem = StemName::of_region(&manifest.id)?;
        Ok(Self {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            group: manifest.group.clone(),
            bbox: BoundingBox::parse(manifest.bbox)?,
            max_zoom: manifest.max_zoom,
            build_date: manifest.build_date.clone(),
            source: Some(manifest.source.clone()).filter(|source| !source.is_empty()),
            pack: PackFile::parse(&manifest.pack, stem.pack_file())?,
            index: PackFile::parse(&manifest.index, stem.index_file())?,
            stem,
        })
    }

    /// Parses a manifest's JSON bytes.
    ///
    /// # Errors
    ///
    /// [`MapsError::BadAnswer`] if it is not JSON in the manifest format, or as
    /// [`RegionManifest::parse`].
    pub fn from_json(bytes: &[u8]) -> Result<Self, MapsError> {
        let manifest: Manifest = serde_json::from_slice(bytes).map_err(|_| MapsError::BadAnswer)?;
        Self::parse(&manifest)
    }

    /// The manifest as JSON, as written into the maps directory.
    ///
    /// # Errors
    ///
    /// [`MapsError::Internal`] if it cannot be encoded (it always can).
    pub fn to_json(&self) -> Result<Vec<u8>, MapsError> {
        let manifest = Manifest {
            format: FORMAT,
            id: self.id.clone(),
            name: self.name.clone(),
            group: self.group.clone(),
            bbox: self.bbox.values(),
            max_zoom: self.max_zoom,
            build_date: self.build_date.clone(),
            source: self.source.clone().unwrap_or_default(),
            pack: self.pack.to_entry(),
            index: self.index.to_entry(),
        };
        serde_json::to_vec_pretty(&manifest).map_err(|_| MapsError::Internal)
    }

    /// The index and the pack together, in bytes.
    #[must_use]
    pub const fn total_size(&self) -> u64 {
        self.index.size.saturating_add(self.pack.size)
    }
}

/// One region a host lists.
#[derive(Clone, Debug, PartialEq)]
pub enum CatalogEntry {
    /// Cut and ready to download.
    Ready(Box<RegionManifest>),
    /// A cache-mode host cuts it on first request.
    OnDemand {
        /// The region id.
        id: String,
        /// Its stem.
        stem: StemName,
        /// The name people see.
        name: String,
        /// The parent's name.
        group: Option<String>,
    },
}

impl CatalogEntry {
    /// The region id.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Ready(manifest) => &manifest.id,
            Self::OnDemand { id, .. } => id,
        }
    }

    /// The name people see.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Ready(manifest) => &manifest.name,
            Self::OnDemand { name, .. } => name,
        }
    }

    /// The parent's name.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        match self {
            Self::Ready(manifest) => manifest.group.as_deref(),
            Self::OnDemand { group, .. } => group.as_deref(),
        }
    }

    /// The stem the region's files share.
    #[must_use]
    pub const fn stem(&self) -> &StemName {
        match self {
            Self::Ready(manifest) => &manifest.stem,
            Self::OnDemand { stem, .. } => stem,
        }
    }
}

/// The entries of a `catalog.json`; entries that do not parse are left out, as one bad
/// manifest must not hide a host's other regions.
///
/// # Errors
///
/// [`MapsError::BadAnswer`] if the body is not a JSON object with a `regions` array.
pub(crate) fn parse_catalog(bytes: &[u8]) -> Result<Vec<CatalogEntry>, MapsError> {
    let catalog: Value = serde_json::from_slice(bytes).map_err(|_| MapsError::BadAnswer)?;
    let regions = catalog
        .get("regions")
        .and_then(Value::as_array)
        .ok_or(MapsError::BadAnswer)?;
    Ok(regions.iter().filter_map(catalog_entry).collect())
}

fn catalog_entry(value: &Value) -> Option<CatalogEntry> {
    if value.get("status").and_then(Value::as_str) == Some("on_demand") {
        let id = value.get("id").and_then(Value::as_str)?.to_owned();
        return Some(CatalogEntry::OnDemand {
            stem: StemName::of_region(&id).ok()?,
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_owned(),
            group: value
                .get("group")
                .and_then(Value::as_str)
                .map(str::to_owned),
            id,
        });
    }
    let manifest: Manifest = serde_json::from_value(value.clone()).ok()?;
    RegionManifest::parse(&manifest)
        .ok()
        .map(|manifest| CatalogEntry::Ready(Box::new(manifest)))
}

#[cfg(test)]
pub(crate) mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    pub(crate) fn manifest_json(id: &str, pack: &[u8], index: &[u8]) -> String {
        let stem = id.replace('/', ".");
        format!(
            r#"{{"format":1,"id":"{id}","name":"Central","group":"Victoria",
               "bbox":[144.95,-37.825,144.98,-37.805],"max_zoom":15,"build_date":"2026-11-12",
               "pack":{{"file":"{stem}.pmtiles","size":{},"sha256":"{}"}},
               "index":{{"file":"{stem}.places.tsv","size":{},"sha256":"{}","entries":3}}}}"#,
            pack.len(),
            pingers_crypto::content_digest(pack).to_hex(),
            index.len(),
            pingers_crypto::content_digest(index).to_hex(),
        )
    }

    #[test]
    fn stems_are_safe_file_names() -> Result<(), MapsError> {
        assert_eq!(
            StemName::of_region("oceania/australia/victoria")?.as_str(),
            "oceania.australia.victoria"
        );
        for id in ["", "../etc", "a//b", "Oceania", "a/-b", "a b", "a.b", "a/"] {
            assert!(StemName::of_region(id).is_err(), "{id}");
        }
        let stem = StemName::of_region("a/b-2")?;
        assert_eq!(stem.manifest_file(), "a.b-2.json");
        assert_eq!(stem.pack_file(), "a.b-2.pmtiles");
        assert_eq!(stem.index_file(), "a.b-2.places.tsv");
        Ok(())
    }

    #[test]
    fn manifests_without_a_source_parse_and_round_trip() -> Result<(), MapsError> {
        let parsed = RegionManifest::from_json(manifest_json("a/b", b"pack", b"idx").as_bytes())?;
        assert_eq!(parsed.source, None);
        assert_eq!(parsed.total_size(), 7);
        assert!((parsed.bbox.north + 37.805).abs() < 1e-9);
        assert_eq!(RegionManifest::from_json(&parsed.to_json()?)?, parsed);
        Ok(())
    }

    #[test]
    fn manifests_naming_other_files_or_bad_checksums_are_refused() {
        let good = manifest_json("a/b", b"pack", b"idx");
        for bad in [
            good.replace("a.b.pmtiles", "../a.b.pmtiles"),
            good.replace("a.b.places.tsv", "other.places.tsv"),
            good.replace("\"format\":1", "\"format\":2"),
            good.replace("144.98", "200"),
            good.replace("-37.805", "-38.9"),
            good.replace(&pingers_crypto::content_digest(b"pack").to_hex(), "zz"),
            "not json".to_owned(),
        ] {
            assert!(RegionManifest::from_json(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn catalogs_keep_good_entries_and_on_demand_stubs() -> Result<(), MapsError> {
        let catalog = format!(
            r#"{{"format":1,"regions":[{},{{"id":"a/c","name":"C","group":"A","status":"on_demand"}},
               {{"id":"../x","status":"on_demand"}},{{"id":"a/d"}}]}}"#,
            manifest_json("a/b", b"p", b"i")
        );
        let entries = parse_catalog(catalog.as_bytes())?;
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries.first(), Some(CatalogEntry::Ready(m)) if m.id == "a/b"));
        let on_demand = entries.get(1).ok_or(MapsError::BadAnswer)?;
        assert_eq!(
            (on_demand.id(), on_demand.name(), on_demand.group()),
            ("a/c", "C", Some("A"))
        );
        assert_eq!(on_demand.stem().as_str(), "a.c");
        assert!(parse_catalog(b"{}").is_err());
        assert!(parse_catalog(b"[").is_err());
        Ok(())
    }
}
