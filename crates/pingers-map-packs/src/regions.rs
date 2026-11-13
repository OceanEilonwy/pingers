// SPDX-License-Identifier: AGPL-3.0-only
//! The region list an operator keeps (`regions.toml`) and the selection of regions a host
//! carries (its allow-list and per-region zoom overrides).
//!
//! Region ids are paths of lowercase segments, `continent/country/state/...`, so a large
//! country can be split into sub-regions that each become a small pack, and an allow-list can
//! name a whole continent or one state.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Why a region list or selection was rejected.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegionError {
    /// The TOML did not parse or has the wrong shape.
    #[error("region list: {0}")]
    Toml(String),
    /// An id is not lowercase `[a-z0-9-]` segments joined by `/`.
    #[error("`{0}` is not a region id (lowercase a-z, 0-9 and - segments joined by /)")]
    BadId(String),
    /// Two regions share an id.
    #[error("region `{0}` is listed twice")]
    Duplicate(String),
    /// A zoom beyond what the Protomaps build has.
    #[error("region `{id}`: max_zoom {zoom} is above 15")]
    Zoom {
        /// The region.
        id: String,
        /// The zoom asked for.
        zoom: u8,
    },
}

/// The deepest zoom of the Protomaps basemap build.
pub const MAX_ZOOM: u8 = 15;

/// A region id such as `oceania/australia/victoria`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionId(String);

impl RegionId {
    /// Checks and wraps an id.
    ///
    /// # Errors
    ///
    /// [`RegionError::BadId`] unless it is lowercase `[a-z0-9-]` segments joined by `/`.
    pub fn new(id: &str) -> Result<Self, RegionError> {
        let segment_ok = |segment: &str| {
            !segment.is_empty()
                && !segment.starts_with('-')
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        };
        if id.split('/').all(segment_ok) {
            Ok(Self(id.to_owned()))
        } else {
            Err(RegionError::BadId(id.to_owned()))
        }
    }

    /// The id as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The file name stem for the region's files: the id with `/` replaced by `.`, so a host
    /// serves one flat directory (`oceania.australia.victoria.pmtiles`).
    #[must_use]
    pub fn file_stem(&self) -> String {
        self.0.replace('/', ".")
    }

    /// The id a file stem names, if it is one.
    #[must_use]
    pub fn from_file_stem(stem: &str) -> Option<Self> {
        Self::new(&stem.replace('.', "/")).ok()
    }

    /// Whether this region is `prefix` or lies inside it, comparing whole segments.
    #[must_use]
    pub fn is_within(&self, prefix: &Self) -> bool {
        self.0 == prefix.0
            || self
                .0
                .strip_prefix(prefix.0.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

impl fmt::Display for RegionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One region of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    /// Its id.
    pub id: RegionId,
    /// The name people see ("Victoria").
    pub name: String,
    /// The parent's name, for grouping in the app ("Australia").
    pub group: Option<String>,
    /// The GeoJSON Polygon or MultiPolygon file `pmtiles extract --region` reads.
    pub polygon: PathBuf,
    /// The deepest zoom cut for this region.
    pub max_zoom: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListFile {
    #[serde(default)]
    region: Vec<RegionFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionFile {
    id: String,
    name: String,
    group: Option<String>,
    polygon: PathBuf,
    max_zoom: u8,
}

/// A parsed region list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionList {
    /// The regions, in file order.
    pub regions: Vec<Region>,
}

impl RegionList {
    /// Parses `regions.toml`; relative polygon paths are taken relative to `base`, the list's
    /// directory.
    ///
    /// # Errors
    ///
    /// A [`RegionError`] for bad TOML, ids, duplicates or zooms.
    pub fn parse(text: &str, base: &Path) -> Result<Self, RegionError> {
        let file: ListFile =
            toml::from_str(text).map_err(|error| RegionError::Toml(error.message().to_owned()))?;
        let mut regions: Vec<Region> = Vec::new();
        for entry in file.region {
            let id = RegionId::new(&entry.id)?;
            if regions.iter().any(|region| region.id == id) {
                return Err(RegionError::Duplicate(entry.id));
            }
            if entry.max_zoom > MAX_ZOOM {
                return Err(RegionError::Zoom {
                    id: entry.id,
                    zoom: entry.max_zoom,
                });
            }
            regions.push(Region {
                id,
                name: entry.name,
                group: entry.group,
                polygon: base.join(entry.polygon),
                max_zoom: entry.max_zoom,
            });
        }
        Ok(Self { regions })
    }
}

/// Which regions a host carries, and at what zoom.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Selection {
    /// Allow-listed ids: each hosts itself and every region inside it.
    pub hosted: Vec<RegionId>,
    /// Zoom caps by id prefix; the longest matching prefix wins.
    pub max_zoom: BTreeMap<RegionId, u8>,
}

impl Selection {
    /// Whether `id` is allow-listed.
    #[must_use]
    pub fn hosts(&self, id: &RegionId) -> bool {
        self.hosted.iter().any(|prefix| id.is_within(prefix))
    }

    /// The zoom to cut `region` at: the region's own, lowered by the longest matching override.
    #[must_use]
    pub fn zoom_for(&self, region: &Region) -> u8 {
        self.max_zoom
            .iter()
            .filter(|(prefix, _)| region.id.is_within(prefix))
            .max_by_key(|(prefix, _)| prefix.as_str().len())
            .map_or(region.max_zoom, |(_, zoom)| (*zoom).min(region.max_zoom))
    }

    /// The allow-listed regions of `list`, with their zoom applied. Regions that contain other
    /// listed regions are kept too: an allow-list entry names exactly what is hosted.
    #[must_use]
    pub fn select(&self, list: &RegionList) -> Vec<Region> {
        list.regions
            .iter()
            .filter(|region| self.hosts(&region.id))
            .map(|region| Region {
                max_zoom: self.zoom_for(region),
                ..region.clone()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    const LIST: &str = r#"
[[region]]
id = "oceania/australia/victoria"
name = "Victoria"
group = "Australia"
polygon = "victoria.geojson"
max_zoom = 15

[[region]]
id = "oceania/australia/new-south-wales"
name = "New South Wales"
group = "Australia"
polygon = "/abs/nsw.geojson"
max_zoom = 15

[[region]]
id = "europe/germany"
name = "Germany"
polygon = "germany.geojson"
max_zoom = 14
"#;

    fn id(text: &str) -> Result<RegionId, RegionError> {
        RegionId::new(text)
    }

    #[test]
    fn lists_parse_with_paths_relative_to_the_list() -> Result<(), RegionError> {
        let list = RegionList::parse(LIST, Path::new("/srv/regions"))?;
        assert_eq!(list.regions.len(), 3);
        let first = list
            .regions
            .first()
            .ok_or(RegionError::Toml(String::new()))?;
        assert_eq!(
            first.polygon,
            PathBuf::from("/srv/regions/victoria.geojson")
        );
        assert_eq!(first.group.as_deref(), Some("Australia"));
        assert_eq!(
            list.regions.get(1).map(|region| region.polygon.clone()),
            Some(PathBuf::from("/abs/nsw.geojson"))
        );
        Ok(())
    }

    #[test]
    fn bad_lists_are_refused() {
        let duplicate = format!(
            "{LIST}\n[[region]]\nid = \"europe/germany\"\nname = \"x\"\npolygon = \"x\"\nmax_zoom = 3\n"
        );
        assert_eq!(
            RegionList::parse(&duplicate, Path::new(".")),
            Err(RegionError::Duplicate("europe/germany".into()))
        );
        let zoom = "[[region]]\nid = \"a\"\nname = \"A\"\npolygon = \"a\"\nmax_zoom = 16\n";
        assert!(matches!(
            RegionList::parse(zoom, Path::new(".")),
            Err(RegionError::Zoom { .. })
        ));
        assert!(RegionId::new("Europe/Germany").is_err());
        assert!(RegionId::new("europe//germany").is_err());
        assert!(RegionId::new("../etc").is_err());
    }

    #[test]
    fn ids_match_whole_segments_and_map_to_flat_file_names() -> Result<(), RegionError> {
        let victoria = id("oceania/australia/victoria")?;
        assert!(victoria.is_within(&id("oceania")?));
        assert!(victoria.is_within(&victoria));
        assert!(!victoria.is_within(&id("oceania/austral")?));
        assert_eq!(victoria.file_stem(), "oceania.australia.victoria");
        assert_eq!(
            RegionId::from_file_stem("oceania.australia.victoria"),
            Some(victoria)
        );
        Ok(())
    }

    #[test]
    fn selections_keep_allow_listed_regions_and_cap_their_zoom() -> Result<(), RegionError> {
        let list = RegionList::parse(LIST, Path::new("."))?;
        let selection = Selection {
            hosted: vec![id("oceania")?],
            max_zoom: BTreeMap::from([
                (id("oceania")?, 13),
                (id("oceania/australia/victoria")?, 14),
            ]),
        };
        let chosen = selection.select(&list);
        let zooms: Vec<(&str, u8)> = chosen
            .iter()
            .map(|region| (region.id.as_str(), region.max_zoom))
            .collect();
        assert_eq!(
            zooms,
            vec![
                ("oceania/australia/victoria", 14),
                ("oceania/australia/new-south-wales", 13)
            ]
        );
        assert!(!selection.hosts(&id("europe/germany")?));
        Ok(())
    }
}
