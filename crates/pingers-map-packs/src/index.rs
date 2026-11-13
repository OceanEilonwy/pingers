// SPDX-License-Identifier: AGPL-3.0-only
//! The place-name search index shipped beside each pack, so "Search a place" runs on the
//! phone over a small file instead of a geocoder.
//!
//! Format (`<stem>.places.tsv`, UTF-8): a first line `# pingers-places 1`, then one place per
//! line, `key TAB kind TAB latitude TAB longitude TAB name`, sorted by key. `key` is the name
//! normalised by [`search_key`] (lowercase, Latin accents folded, punctuation as single
//! spaces), so the app matches the user's typing with the same function and a binary search
//! or a scan. `kind` is `country`, `region`, `city`, `suburb`, `street` or `poi`. Coordinates
//! have five decimals (about a metre).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Read, Seek};

use crate::mvt::{Feature, named_features};
use crate::pmtiles::{Pack, PackError, TileId};

/// The index's first line.
pub const INDEX_HEADER: &str = "# pingers-places 1";

/// What a place is, coarsest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlaceKind {
    /// A country.
    Country,
    /// A state, province or similar.
    Region,
    /// A city, town or village.
    City,
    /// A suburb or neighbourhood.
    Suburb,
    /// A named street or path.
    Street,
    /// A named point of interest.
    Poi,
}

impl PlaceKind {
    /// The word in the index.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Country => "country",
            Self::Region => "region",
            Self::City => "city",
            Self::Suburb => "suburb",
            Self::Street => "street",
            Self::Poi => "poi",
        }
    }

    /// How far apart two same-named features may be and still be one place, in metres.
    const fn merge_distance(self) -> f64 {
        match self {
            Self::Country | Self::Region => 200_000.0,
            Self::City | Self::Suburb => 5_000.0,
            Self::Street => 1_500.0,
            Self::Poi => 100.0,
        }
    }
}

/// One entry of the index.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// The name as mapped.
    pub name: String,
    /// What it is.
    pub kind: PlaceKind,
    /// Degrees north.
    pub latitude: f64,
    /// Degrees east.
    pub longitude: f64,
}

/// Normalises a name for matching: lowercase, accents on Latin letters folded, and every run
/// of characters that are not letters or digits turned into one space. The app's Kotlin
/// `searchKey` must agree with this for Latin text; other scripts pass through lowercased.
#[must_use]
pub fn search_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_space = false;
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            fold(character, &mut out);
        } else {
            pending_space = true;
        }
    }
    out
}

/// Folds one lowercase Latin letter with a diacritic to its base letters.
fn fold(character: char, out: &mut String) {
    let base = match character {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'þ' => "th",
        _ => {
            out.push(character);
            return;
        }
    };
    out.push_str(base);
}

/// The index kind of a Protomaps basemap (v4) feature, or `None` to leave it out.
fn kind_of(feature: &Feature) -> Option<PlaceKind> {
    match (feature.layer.as_str(), feature.kind.as_deref()) {
        ("places", Some("country")) => Some(PlaceKind::Country),
        ("places", Some("region")) => Some(PlaceKind::Region),
        ("places", Some("locality")) => Some(PlaceKind::City),
        ("places", Some("neighbourhood" | "macrohood")) => Some(PlaceKind::Suburb),
        ("roads", Some(_)) => Some(PlaceKind::Street),
        ("pois", Some(_)) => Some(PlaceKind::Poi),
        _ => None,
    }
}

fn to_degrees(tile: TileId, feature: &Feature) -> Option<(f64, f64)> {
    let tiles = f64::from(1_u32.checked_shl(u32::from(tile.z))?);
    let extent = f64::from(feature.extent);
    let x = (f64::from(tile.x) + feature.point.x / extent) / tiles;
    let y = (f64::from(tile.y) + feature.point.y / extent) / tiles;
    let longitude = x * 360.0 - 180.0;
    let latitude = (core::f64::consts::PI * (1.0 - 2.0 * y))
        .sinh()
        .atan()
        .to_degrees();
    Some((latitude, longitude))
}

fn distance_m(a: &Place, b: &Place) -> f64 {
    let mean_latitude = f64::midpoint(a.latitude, b.latitude).to_radians();
    let dx = (a.longitude - b.longitude).to_radians() * mean_latitude.cos();
    let dy = (a.latitude - b.latitude).to_radians();
    6_371_000.0 * dx.hypot(dy)
}

/// Same-named features of one kind near each other (a street cut into tiles and segments) as
/// one place: the member nearest the cluster's average.
fn merge(places: Vec<Place>) -> Vec<Place> {
    let mut groups: BTreeMap<(String, PlaceKind), Vec<Vec<Place>>> = BTreeMap::new();
    for place in places {
        let clusters = groups
            .entry((search_key(&place.name), place.kind))
            .or_default();
        let near = clusters.iter_mut().find(|cluster| {
            cluster
                .first()
                .is_some_and(|first| distance_m(first, &place) <= place.kind.merge_distance())
        });
        match near {
            Some(cluster) => cluster.push(place),
            None => clusters.push(vec![place]),
        }
    }
    let mut out = Vec::new();
    for clusters in groups.into_values() {
        for cluster in clusters {
            let Ok(count) = u32::try_from(cluster.len()) else {
                continue;
            };
            let count = f64::from(count);
            let centre = Place {
                name: String::new(),
                kind: PlaceKind::Poi,
                latitude: cluster.iter().map(|place| place.latitude).sum::<f64>() / count,
                longitude: cluster.iter().map(|place| place.longitude).sum::<f64>() / count,
            };
            let nearest = cluster
                .into_iter()
                .min_by(|a, b| distance_m(a, &centre).total_cmp(&distance_m(b, &centre)));
            out.extend(nearest);
        }
    }
    out
}

/// Reads every named place, street and point of interest in `pack`. Streets and points come
/// from the deepest zoom's tiles, which hold all of them; places from every zoom, since some
/// are only labelled at lower zooms. Low-zoom tiles reach far beyond the region (a zoom-3 tile
/// holds half a continent), so only places inside the pack's bounds, with a small margin, are
/// kept: a search should not offer somewhere the pack cannot show.
///
/// # Errors
///
/// [`PackError`] if the pack cannot be read or a tile is malformed.
pub fn places_in<F: Read + Seek>(pack: &mut Pack<F>) -> Result<Vec<Place>, PackError> {
    let deepest = pack.header.max_zoom;
    let bounds = pack.header.bounds;
    let margin_lat = (bounds.north - bounds.south) * 0.05;
    let margin_lon = (bounds.east - bounds.west) * 0.05;
    let inside = |latitude: f64, longitude: f64| {
        (bounds.south - margin_lat..=bounds.north + margin_lat).contains(&latitude)
            && (bounds.west - margin_lon..=bounds.east + margin_lon).contains(&longitude)
    };
    let mut found = Vec::new();
    for tile in pack.tiles()? {
        let layers: &[&str] = if tile.tile.z == deepest {
            &["places", "roads", "pois"]
        } else {
            &["places"]
        };
        let bytes = pack.tile(tile)?;
        let features = named_features(&bytes, layers).map_err(|_| PackError::Directory)?;
        for feature in features {
            let (Some(kind), Some(name)) = (kind_of(&feature), feature.name.clone()) else {
                continue;
            };
            let Some((latitude, longitude)) = to_degrees(tile.tile, &feature) else {
                continue;
            };
            if !inside(latitude, longitude) {
                continue;
            }
            found.push(Place {
                name,
                kind,
                latitude,
                longitude,
            });
        }
    }
    Ok(merge(found))
}

/// The index file's text for `places`.
#[must_use]
pub fn index_text(places: &[Place]) -> String {
    let mut rows: Vec<(String, &Place)> = places
        .iter()
        .map(|place| (search_key(&place.name), place))
        .collect();
    rows.retain(|(key, _)| !key.is_empty());
    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.kind.cmp(&b.1.kind))
            .then(a.1.name.cmp(&b.1.name))
    });
    let mut out = String::from(INDEX_HEADER);
    out.push('\n');
    for (key, place) in rows {
        let name: String = place
            .name
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        let _infallible = writeln!(
            out,
            "{key}\t{}\t{:.5}\t{:.5}\t{name}",
            place.kind.as_str(),
            place.latitude,
            place.longitude
        );
    }
    out
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn keys_fold_case_accents_and_punctuation() {
        assert_eq!(
            search_key("Flinders Street Station"),
            "flinders street station"
        );
        assert_eq!(
            search_key("  Café   Éclair—Pâtisserie "),
            "cafe eclair patisserie"
        );
        assert_eq!(search_key("Straße"), "strasse");
        assert_eq!(search_key("St. Kilda Rd"), "st kilda rd");
        assert_eq!(search_key("東京"), "東京");
        assert_eq!(search_key("--"), "");
    }

    fn place(name: &str, kind: PlaceKind, latitude: f64, longitude: f64) -> Place {
        Place {
            name: name.into(),
            kind,
            latitude,
            longitude,
        }
    }

    #[test]
    fn street_segments_merge_but_distant_namesakes_do_not() {
        let merged = merge(vec![
            place("Collins Street", PlaceKind::Street, -37.8150, 144.9650),
            place("Collins Street", PlaceKind::Street, -37.8160, 144.9690),
            place("Collins Street", PlaceKind::Street, -37.8170, 144.9700),
            place("Collins Street", PlaceKind::Street, -38.5000, 145.5000),
            place("Collins Street", PlaceKind::Poi, -37.8150, 144.9650),
        ]);
        let streets = merged
            .iter()
            .filter(|p| p.kind == PlaceKind::Street)
            .count();
        assert_eq!(streets, 2);
        assert_eq!(merged.len(), 3);
        // The kept segment is the one nearest the cluster's middle.
        assert!(
            merged
                .iter()
                .any(|p| p.kind == PlaceKind::Street && (p.longitude - 144.9690).abs() < 1e-9)
        );
    }

    #[test]
    fn the_index_is_sorted_by_key_with_a_header() {
        let text = index_text(&[
            place("Southbank", PlaceKind::Suburb, -37.8225, 144.9643),
            place("Block Arcade", PlaceKind::Poi, -37.81561, 144.96458),
            place("…", PlaceKind::Poi, 0.0, 0.0),
        ]);
        assert_eq!(
            text,
            "# pingers-places 1\nblock arcade\tpoi\t-37.81561\t144.96458\tBlock Arcade\nsouthbank\tsuburb\t-37.82250\t144.96430\tSouthbank\n"
        );
    }

    #[test]
    fn tile_points_become_degrees() -> Result<(), PackError> {
        let feature = Feature {
            layer: "pois".into(),
            name: Some("x".into()),
            kind: Some("cafe".into()),
            kind_detail: None,
            point: crate::mvt::TilePoint {
                x: 2048.0,
                y: 2048.0,
            },
            extent: 4096,
        };
        let (latitude, longitude) =
            to_degrees(TileId { z: 1, x: 1, y: 1 }, &feature).ok_or(PackError::Directory)?;
        assert!((longitude - 90.0).abs() < 1e-9);
        assert!((latitude + 66.513_26).abs() < 1e-4);
        Ok(())
    }
}
