// SPDX-License-Identifier: AGPL-3.0-only
//! Region ids, the operator's allow-list, file names and the pingers-tiles region list.
//!
//! A region id is lowercase `[a-z0-9-]` segments joined by `/`, for example
//! `oceania/australia/victoria`. On disk and in URLs the `/` becomes `.`:
//! `oceania.australia.victoria.pmtiles`, `.places.tsv` and `.json`. Segments hold no `.`, so
//! the mapping is reversible.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Stems that name the host's own files, never a region.
const RESERVED_STEMS: [&str; 3] = ["catalog", "cache-state", "sources"];

/// A validated hierarchical region id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionId(String);

impl RegionId {
    /// Parses `a/b/c`. `None` unless every segment is non-empty lowercase `[a-z0-9-]`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::from_separated(text, '/')
    }

    /// Parses a file stem `a.b.c`.
    #[must_use]
    pub fn from_stem(stem: &str) -> Option<Self> {
        Self::from_separated(stem, '.')
    }

    fn from_separated(text: &str, separator: char) -> Option<Self> {
        if text.is_empty() || text.len() > 200 {
            return None;
        }
        let valid = text.split(separator).all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
        valid.then(|| Self(text.replace(separator, "/")))
    }

    /// The id as written: `a/b/c`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The file stem: `a.b.c`.
    #[must_use]
    pub fn stem(&self) -> String {
        self.0.replace('/', ".")
    }

    /// The parent id: `a/b` for `a/b/c`, `None` for a top-level id.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| Self(parent.to_owned()))
    }

    /// Whether this id is `prefix` or lies under it, matching whole segments: `oceania` covers
    /// `oceania` and `oceania/australia` but not `oceanian`.
    #[must_use]
    pub fn is_within(&self, prefix: &Self) -> bool {
        match self.0.strip_prefix(&prefix.0) {
            Some(rest) => rest.is_empty() || rest.starts_with('/'),
            None => false,
        }
    }

    /// The number of segments.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.0.split('/').count()
    }
}

impl fmt::Display for RegionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Which of a region's three files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// `<stem>.json`: the manifest.
    Manifest,
    /// `<stem>.pmtiles`: the tile pack.
    Pack,
    /// `<stem>.places.tsv`: the place search index.
    Index,
}

impl FileKind {
    /// All three, in the order a cut writes them last-to-first safe for readers.
    pub const ALL: [Self; 3] = [Self::Pack, Self::Index, Self::Manifest];

    /// The file name suffix, with its dot.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Manifest => ".json",
            Self::Pack => ".pmtiles",
            Self::Index => ".places.tsv",
        }
    }

    /// The `Content-Type`.
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Manifest => "application/json",
            Self::Pack => "application/vnd.pmtiles",
            Self::Index => "text/tab-separated-values; charset=utf-8",
        }
    }

    /// The `Cache-Control`: packs and indexes are versioned by their manifest's sha256, so
    /// caches may keep them; manifests change when a region is rebuilt.
    #[must_use]
    pub const fn cache_control(self) -> &'static str {
        match self {
            Self::Manifest => "public, max-age=300",
            Self::Pack | Self::Index => "public, max-age=31536000, immutable",
        }
    }
}

/// A served file: a region and which of its files.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RegionFile {
    /// The region.
    pub region: RegionId,
    /// Which file.
    pub kind: FileKind,
}

impl RegionFile {
    /// Parses a file name such as `oceania.australia.places.tsv`. `None` for anything else,
    /// including the host's own files (`catalog.json`, `cache-state.json`, `sources.json`).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        // `.places.tsv` before `.json`/`.pmtiles` is irrelevant (no overlap), but keep the
        // longest suffix first in case one is ever added that overlaps.
        let (stem, kind) = [FileKind::Index, FileKind::Pack, FileKind::Manifest]
            .into_iter()
            .find_map(|kind| name.strip_suffix(kind.suffix()).map(|stem| (stem, kind)))?;
        if RESERVED_STEMS.contains(&stem) {
            return None;
        }
        Some(Self {
            region: RegionId::from_stem(stem)?,
            kind,
        })
    }

    /// The file name.
    #[must_use]
    pub fn name(&self) -> String {
        let mut name = self.region.stem();
        name.push_str(self.kind.suffix());
        name
    }
}

/// Which regions the operator hosts, and per-region zoom overrides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllowList {
    /// Hosted prefixes.
    pub hosted: Vec<RegionId>,
    /// Zoom overrides by prefix; the longest matching prefix wins.
    pub max_zoom: Vec<(RegionId, u8)>,
}

impl AllowList {
    /// Whether `region` is hosted: it is, or lies under, a hosted prefix.
    #[must_use]
    pub fn allows(&self, region: &RegionId) -> bool {
        self.hosted.iter().any(|prefix| region.is_within(prefix))
    }

    /// The zoom override for `region`, if any prefix of it has one.
    #[must_use]
    pub fn max_zoom(&self, region: &RegionId) -> Option<u8> {
        self.max_zoom
            .iter()
            .filter(|(prefix, _)| region.is_within(prefix))
            .max_by_key(|(prefix, _)| prefix.depth())
            .map(|(_, zoom)| *zoom)
    }
}

/// One region from the pingers-tiles region list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionEntry {
    /// The id.
    pub id: RegionId,
    /// The display name.
    pub name: String,
    /// The display name of the parent region, if any.
    pub group: Option<String>,
    /// The GeoJSON polygon file, resolved against the list's directory.
    pub polygon: PathBuf,
    /// The deepest zoom to cut.
    pub max_zoom: u8,
}

/// Why a region list could not be used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegionListError {
    /// Not valid TOML, or not the schema.
    #[error("syntax or schema error at line {line}")]
    Syntax {
        /// One-based line, or 0 if unknown.
        line: usize,
    },
    /// An id that is not lowercase `[a-z0-9-]` segments joined by `/`.
    #[error("region {index} has an invalid id")]
    BadId {
        /// Zero-based position in the list.
        index: usize,
    },
    /// The same id twice.
    #[error("region {id} appears twice")]
    Duplicate {
        /// The id.
        id: String,
    },
}

#[derive(Deserialize)]
struct FileRegionList {
    #[serde(default)]
    region: Vec<FileRegion>,
}

#[derive(Deserialize)]
struct FileRegion {
    id: String,
    name: String,
    group: Option<String>,
    polygon: PathBuf,
    max_zoom: u8,
}

/// Parses a pingers-tiles region list. `base` is the list file's directory: polygon paths are
/// relative to it. A region without a `group` gets its parent's name, if the parent is listed.
///
/// # Errors
///
/// [`RegionListError`] naming the first problem.
pub fn parse_region_list(text: &str, base: &Path) -> Result<Vec<RegionEntry>, RegionListError> {
    let file: FileRegionList = toml::from_str(text).map_err(|error| RegionListError::Syntax {
        line: error
            .span()
            .map_or(0, |span| crate::config::line_of(text, span.start)),
    })?;
    let mut entries = Vec::with_capacity(file.region.len());
    for (index, region) in file.region.into_iter().enumerate() {
        let id = RegionId::parse(&region.id).ok_or(RegionListError::BadId { index })?;
        if entries.iter().any(|entry: &RegionEntry| entry.id == id) {
            return Err(RegionListError::Duplicate { id: region.id });
        }
        entries.push(RegionEntry {
            id,
            name: region.name,
            group: region.group,
            polygon: base.join(region.polygon),
            max_zoom: region.max_zoom,
        });
    }
    let names: Vec<(RegionId, String)> = entries
        .iter()
        .map(|entry| (entry.id.clone(), entry.name.clone()))
        .collect();
    for entry in &mut entries {
        if entry.group.is_none() {
            let parent = entry.id.parent();
            entry.group = names
                .iter()
                .find(|(id, _)| Some(id) == parent.as_ref())
                .map(|(_, name)| name.clone());
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> Option<RegionId> {
        RegionId::parse(text)
    }

    #[test]
    fn ids_are_lowercase_segments() {
        assert!(id("oceania/australia/victoria").is_some());
        assert!(id("north-america/us/new-york-2").is_some());
        for bad in ["", "/a", "a/", "a//b", "A", "a.b", "a b", "a/../b", "é"] {
            assert_eq!(id(bad), None, "{bad}");
        }
    }

    #[test]
    fn stems_round_trip() {
        let region = RegionId::parse("oceania/australia");
        assert_eq!(
            region.as_ref().map(RegionId::stem).as_deref(),
            Some("oceania.australia")
        );
        assert_eq!(RegionId::from_stem("oceania.australia"), region);
    }

    #[test]
    fn file_names_parse() {
        let parsed = RegionFile::parse("oceania.australia.places.tsv");
        assert_eq!(
            parsed
                .as_ref()
                .map(|file| (file.region.as_str(), file.kind)),
            Some(("oceania/australia", FileKind::Index))
        );
        assert_eq!(
            parsed.map(|file| file.name()).as_deref(),
            Some("oceania.australia.places.tsv")
        );
        assert_eq!(
            RegionFile::parse("europe.pmtiles").map(|file| file.kind),
            Some(FileKind::Pack)
        );
        for bad in [
            "catalog.json",
            "cache-state.json",
            "sources.json",
            ".json",
            "a.tsv",
            "a.txt",
            "..json",
            "a..b.json",
            "A.json",
            "a/b.json",
        ] {
            assert_eq!(RegionFile::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn prefixes_match_whole_segments() {
        let allow = AllowList {
            hosted: ["oceania", "europe/germany"]
                .into_iter()
                .filter_map(RegionId::parse)
                .collect(),
            max_zoom: [("europe", 12), ("europe/germany", 13)]
                .into_iter()
                .filter_map(|(prefix, zoom)| Some((RegionId::parse(prefix)?, zoom)))
                .collect(),
        };
        let allows = |text: &str| id(text).is_some_and(|region| allow.allows(&region));
        assert!(allows("oceania"));
        assert!(allows("oceania/australia/victoria"));
        assert!(allows("europe/germany/berlin"));
        assert!(!allows("oceanian"));
        assert!(!allows("europe"));
        assert!(!allows("europe/france"));
        let zoom = |text: &str| id(text).and_then(|region| allow.max_zoom(&region));
        assert_eq!(zoom("europe/germany/berlin"), Some(13));
        assert_eq!(zoom("europe/france"), Some(12));
        assert_eq!(zoom("oceania"), None);
    }

    #[test]
    fn region_lists_resolve_polygons_and_groups() {
        let text = "[[region]]\nid = \"oceania/australia\"\nname = \"Australia\"\n\
                    polygon = \"au.geojson\"\nmax_zoom = 14\n\
                    [[region]]\nid = \"oceania/australia/victoria\"\nname = \"Victoria\"\n\
                    polygon = \"vic.geojson\"\nmax_zoom = 15\n";
        let entries = parse_region_list(text, Path::new("/lists"));
        let entries = entries.as_deref().unwrap_or_default();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries
                .get(1)
                .map(|entry| (entry.group.as_deref(), entry.polygon.clone())),
            Some((Some("Australia"), PathBuf::from("/lists/vic.geojson")))
        );
        assert_eq!(entries.first().and_then(|entry| entry.group.clone()), None);
    }

    #[test]
    fn region_lists_refuse_bad_ids_and_duplicates() {
        let entry = |id: &str| {
            format!("[[region]]\nid = \"{id}\"\nname = \"N\"\npolygon = \"p\"\nmax_zoom = 1\n")
        };
        assert_eq!(
            parse_region_list(&entry("Bad"), Path::new("/")),
            Err(RegionListError::BadId { index: 0 })
        );
        assert_eq!(
            parse_region_list(&format!("{}{}", entry("a"), entry("a")), Path::new("/")),
            Err(RegionListError::Duplicate { id: "a".to_owned() })
        );
    }
}
