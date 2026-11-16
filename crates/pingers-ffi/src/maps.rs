// SPDX-License-Identifier: AGPL-3.0-only
//! Region maps for the app: the catalog, downloads and installed regions (pingers-client's map
//! calls over pingers-map-downloads), and the words for the map host (pingers-words `maps`).
//! The core does every network call (design E.1); the shell renders the installed packs with
//! MapLibre and searches their indexes.

use std::sync::Arc;
use std::time::Duration;

use pingers_client::ports;
use pingers_map_downloads::catalog::{self, HostFailure};
use pingers_map_downloads::install::DownloadProgress;
use pingers_map_downloads::{CatalogEntry, MapsError, RegionManifest, StemName};
use pingers_proto::MapHostUrl;
use pingers_words::Language;
use pingers_words::maps::{MapHostSource as WordsSource, MapsConfiguration, maps};

use crate::CoreError;

/// One file of a region.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MapFile {
    /// Its name in the maps directory.
    pub file: String,
    /// Its length in bytes.
    pub size: u64,
    /// Its SHA-256, lowercase hex.
    pub sha256: String,
    /// For an index: how many places it lists.
    pub entries: Option<u64>,
}

/// A region's manifest.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MapManifest {
    /// The region id (`oceania/australia/victoria`).
    pub id: String,
    /// The name people see.
    pub name: String,
    /// The parent's name, for grouping sub-regions.
    pub group: Option<String>,
    /// Western edge, degrees.
    pub west: f64,
    /// Southern edge, degrees.
    pub south: f64,
    /// Eastern edge, degrees.
    pub east: f64,
    /// Northern edge, degrees.
    pub north: f64,
    /// The deepest zoom in the pack.
    pub max_zoom: u8,
    /// `YYYY-MM-DD` of the Protomaps build.
    pub build_date: String,
    /// The PMTiles pack.
    pub pack: MapFile,
    /// The place-name search index.
    pub index: MapFile,
}

impl From<&RegionManifest> for MapManifest {
    fn from(manifest: &RegionManifest) -> Self {
        let file = |file: &pingers_map_downloads::PackFile| MapFile {
            file: file.file.clone(),
            size: file.size,
            sha256: file.sha256.to_hex(),
            entries: file.entries,
        };
        Self {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            group: manifest.group.clone(),
            west: manifest.bbox.west,
            south: manifest.bbox.south,
            east: manifest.bbox.east,
            north: manifest.bbox.north,
            max_zoom: manifest.max_zoom,
            build_date: manifest.build_date.clone(),
            pack: file(&manifest.pack),
            index: file(&manifest.index),
        }
    }
}

impl TryFrom<&MapManifest> for RegionManifest {
    type Error = CoreError;

    fn try_from(manifest: &MapManifest) -> Result<Self, CoreError> {
        let file = |file: &MapFile| pingers_map_packs::manifest::FileEntry {
            file: file.file.clone(),
            size: file.size,
            sha256: file.sha256.clone(),
            entries: file.entries,
        };
        Self::parse(&pingers_map_packs::manifest::Manifest {
            format: pingers_map_packs::manifest::FORMAT,
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            group: manifest.group.clone(),
            bbox: [manifest.west, manifest.south, manifest.east, manifest.north],
            max_zoom: manifest.max_zoom,
            build_date: manifest.build_date.clone(),
            source: String::new(),
            pack: file(&manifest.pack),
            index: file(&manifest.index),
        })
        .map_err(|_invalid| CoreError::invalid("not a region manifest"))
    }
}

/// A map host and where it came from.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MapHostInfo {
    /// Its origin (`https://...`).
    pub url: String,
    /// Why it is in the chain.
    pub source: MapHostSource,
}

impl From<&pingers_map_downloads::MapHost> for MapHostInfo {
    fn from(host: &pingers_map_downloads::MapHost) -> Self {
        Self {
            url: host.url().as_str().to_owned(),
            source: match host.source() {
                pingers_map_downloads::MapHostSource::UserOverride => MapHostSource::UserOverride,
                pingers_map_downloads::MapHostSource::Server => MapHostSource::Server,
                pingers_map_downloads::MapHostSource::Publisher => MapHostSource::Publisher,
            },
        }
    }
}

impl TryFrom<&MapHostInfo> for pingers_map_downloads::MapHost {
    type Error = CoreError;

    fn try_from(host: &MapHostInfo) -> Result<Self, CoreError> {
        let source = match host.source {
            MapHostSource::UserOverride => pingers_map_downloads::MapHostSource::UserOverride,
            MapHostSource::Server => pingers_map_downloads::MapHostSource::Server,
            MapHostSource::Publisher => pingers_map_downloads::MapHostSource::Publisher,
        };
        Ok(Self::new(&MapHostUrl::new(host.url.clone())?, source))
    }
}

/// One region a host lists.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
#[expect(
    clippy::large_enum_variant,
    reason = "a UniFFI enum crosses the boundary by value; boxing is not available there"
)]
pub enum MapEntry {
    /// Cut and ready.
    Ready {
        /// Its manifest.
        manifest: MapManifest,
    },
    /// A cache-mode host cuts it on first request.
    OnDemand {
        /// The region id.
        id: String,
        /// The name people see.
        name: String,
        /// The parent's name.
        group: Option<String>,
    },
}

/// A region on offer, with the host that will serve it.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MapOffer {
    /// The region.
    pub entry: MapEntry,
    /// Where it comes from.
    pub host: MapHostInfo,
}

impl From<&catalog::RegionOffer> for MapOffer {
    fn from(offer: &catalog::RegionOffer) -> Self {
        let entry = match &offer.entry {
            CatalogEntry::Ready(manifest) => MapEntry::Ready {
                manifest: manifest.as_ref().into(),
            },
            CatalogEntry::OnDemand {
                id, name, group, ..
            } => MapEntry::OnDemand {
                id: id.clone(),
                name: name.clone(),
                group: group.clone(),
            },
        };
        Self {
            entry,
            host: (&offer.host).into(),
        }
    }
}

impl TryFrom<&MapOffer> for catalog::RegionOffer {
    type Error = CoreError;

    fn try_from(offer: &MapOffer) -> Result<Self, CoreError> {
        let entry = match &offer.entry {
            MapEntry::Ready { manifest } => CatalogEntry::Ready(Box::new(manifest.try_into()?)),
            MapEntry::OnDemand { id, name, group } => CatalogEntry::OnDemand {
                id: id.clone(),
                stem: StemName::of_region(id)
                    .map_err(|_invalid| CoreError::invalid("not a region id"))?,
                name: name.clone(),
                group: group.clone(),
            },
        };
        Ok(Self {
            entry,
            host: (&offer.host).try_into()?,
        })
    }
}

/// Why a host offered nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MapHostProblem {
    /// It did not answer, or not with a catalog.
    Unreachable,
    /// With the relay setting on, only the server's own map host can be reached.
    NotThroughRelay,
    /// The relay route has no Pingers server yet.
    NoRelayRoute,
    /// Its gateway key, as the server forwarded it, is not among the pins.
    KeyChanged,
}

impl From<HostFailure> for MapHostProblem {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "HostFailure is non-exhaustive; anything new is reported as unreachable"
    )]
    fn from(failure: HostFailure) -> Self {
        match failure {
            HostFailure::NotThroughRelay => Self::NotThroughRelay,
            HostFailure::NoRelayRoute => Self::NoRelayRoute,
            HostFailure::KeyChanged => Self::KeyChanged,
            _ => Self::Unreachable,
        }
    }
}

/// A host that offered nothing, and why.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MapHostFailure {
    /// The host.
    pub host: MapHostInfo,
    /// Why.
    pub problem: MapHostProblem,
}

/// What the chain offers.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MapCatalog {
    /// Regions, grouped and sorted for the picker.
    pub offers: Vec<MapOffer>,
    /// Hosts that offered nothing.
    pub failed: Vec<MapHostFailure>,
}

impl From<&catalog::Catalog> for MapCatalog {
    fn from(catalog: &catalog::Catalog) -> Self {
        Self {
            offers: catalog.offers.iter().map(Into::into).collect(),
            failed: catalog
                .failed
                .iter()
                .map(|failed| MapHostFailure {
                    host: (&failed.host).into(),
                    problem: failed.reason.into(),
                })
                .collect(),
        }
    }
}

/// The shell's map settings (Advanced): its two hosts and the relay setting.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MapSettings {
    /// The user's map host, if set.
    pub user_override: Option<String>,
    /// The publisher's default from the build, if any.
    pub publisher: Option<String>,
    /// "Download maps through privacy relay".
    pub through_relay: bool,
}

impl MapSettings {
    /// The hosts, checked; a host that is not an `https://` URL is left out.
    pub(crate) fn hosts(&self) -> ports::MapHosts {
        let url = |text: &Option<String>| {
            text.as_ref()
                .map(|text| text.trim())
                .filter(|text| !text.is_empty())
                .and_then(|text| MapHostUrl::new(text.to_owned()).ok())
        };
        ports::MapHosts {
            user_override: url(&self.user_override),
            publisher: url(&self.publisher),
        }
    }

    /// The route the relay setting chooses.
    pub(crate) const fn route(&self) -> ports::MapRoute {
        if self.through_relay {
            ports::MapRoute::Relay
        } else {
            ports::MapRoute::Direct
        }
    }
}

/// One download, as [`crate::Core::download_map`] takes it.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MapDownloadRequest {
    /// The region and its host, from a catalog.
    pub offer: MapOffer,
    /// The map settings: the relay setting chooses the route.
    pub settings: MapSettings,
    /// The app's maps directory.
    pub directory: String,
}

/// Why a download failed, for the picker's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MapFailure {
    /// The host could not be reached, or answered wrongly.
    Network,
    /// A file did not match its checksum.
    Checksum,
    /// The host does not offer the region.
    NotOffered,
    /// The host was still preparing the region after every retry.
    Preparing,
    /// This host cannot be reached through the privacy relay.
    NotThroughRelay,
    /// The relay route has no Pingers server yet.
    NoRelayRoute,
    /// The map host's gateway key is not among the pins.
    KeyChanged,
    /// The maps directory could not be written.
    Storage,
}

impl From<&MapsError> for MapFailure {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "MapsError is non-exhaustive; anything new is reported as a network failure"
    )]
    fn from(error: &MapsError) -> Self {
        match error {
            MapsError::Checksum => Self::Checksum,
            MapsError::NotOffered => Self::NotOffered,
            MapsError::Preparing { .. } => Self::Preparing,
            MapsError::NotThroughRelay | MapsError::KeyNotPinned => Self::NotThroughRelay,
            MapsError::NoRelayRoute => Self::NoRelayRoute,
            MapsError::KeyChanged => Self::KeyChanged,
            MapsError::Io(_) => Self::Storage,
            _ => Self::Network,
        }
    }
}

/// What the app hears while a download runs.
#[uniffi::export(with_foreign)]
pub trait MapDownloadListener: Send + Sync {
    /// `done` of `total` bytes are on the phone.
    fn downloaded(&self, done: u64, total: u64);
    /// The host is cutting the region; the core asks again after this many seconds.
    fn preparing(&self, retry_after_seconds: u64);
}

/// The listener as pingers-map-downloads' progress.
pub(crate) struct ListenerAdapter(pub(crate) Arc<dyn MapDownloadListener>);

impl DownloadProgress for ListenerAdapter {
    fn downloaded(&self, done: u64, total: u64) {
        self.0.downloaded(done, total);
    }

    fn preparing(&self, retry_after: Duration) {
        self.0.preparing(retry_after.as_secs());
    }
}

/// Where the map host in use came from, highest priority first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MapHostSource {
    /// The user set it in Advanced.
    UserOverride,
    /// The Pingers server recommends it.
    Server,
    /// The publisher's built-in default.
    Publisher,
}

/// The maps lines for Advanced and "What your server sees".
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MapsWords {
    /// "Maps".
    pub heading: String,
    /// "Maps: <host>".
    pub row: String,
    /// Where the host came from.
    pub source: String,
    /// What the map host can see.
    pub can_see: Vec<String>,
    /// What it never sees.
    pub never_sees: Vec<String>,
    /// The per-region fallback, explained.
    pub fallback: String,
    /// "Download maps through privacy relay".
    pub relay_setting: String,
    /// What that setting does.
    pub relay_detail: String,
}

/// The maps lines for `host` (a host name), chosen from `source`, with downloads through the
/// privacy relay or not.
#[uniffi::export]
#[must_use]
pub fn maps_words(host: String, source: MapHostSource, through_relay: bool) -> MapsWords {
    let source = words_source(source);
    let text = maps(
        Language::English,
        &MapsConfiguration {
            host,
            source,
            through_relay,
        },
    );
    MapsWords {
        heading: text.heading,
        row: text.row,
        source: text.source,
        can_see: text.can_see,
        never_sees: text.never_sees,
        fallback: text.fallback,
        relay_setting: text.relay_setting,
        relay_detail: text.relay_detail,
    }
}

/// Downloaded maps (canvas `DownloadedMaps`), in words.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DownloadedMapsWords {
    /// "Downloaded maps".
    pub title: String,
    /// "Back to Settings".
    pub back: String,
    /// How maps are downloaded and shown.
    pub intro: String,
    /// "On this phone".
    pub installed: String,
    /// "Available".
    pub available: String,
    /// "Update available".
    pub update_available: String,
    /// "Update".
    pub update: String,
    /// "Delete".
    pub delete: String,
    /// "Download".
    pub download: String,
    /// "This area".
    pub this_area: String,
    /// "From maps.pingers.org".
    pub host_title: String,
    /// Who the host is and what it sees.
    pub host_detail: String,
    /// "Map data from OpenStreetMap contributors".
    pub attribution: String,
}

/// Downloaded maps' words, for the map host in use.
#[uniffi::export]
#[must_use]
pub fn downloaded_maps_words(
    host: String,
    source: MapHostSource,
    through_relay: bool,
) -> DownloadedMapsWords {
    let text = pingers_words::maps::downloaded_maps(
        Language::English,
        &MapsConfiguration {
            host,
            source: words_source(source),
            through_relay,
        },
    );
    DownloadedMapsWords {
        title: text.title,
        back: text.back,
        intro: text.intro,
        installed: text.installed,
        available: text.available,
        update_available: text.update_available,
        update: text.update,
        delete: text.delete,
        download: text.download,
        this_area: text.this_area,
        host_title: text.host_title,
        host_detail: text.host_detail,
        attribution: text.attribution,
    }
}

/// A size on disk, as screens say it: "184 MB".
#[uniffi::export]
#[must_use]
pub fn size_words(bytes: u64) -> String {
    pingers_words::device::size(
        Language::English,
        pingers_screens::device::Bytes::new(bytes),
    )
}

/// A download under way: "Downloading 120 MB of 312 MB".
#[uniffi::export]
#[must_use]
pub fn downloading_words(done: u64, total: u64) -> String {
    pingers_words::maps::downloading(
        Language::English,
        pingers_screens::device::Bytes::new(done),
        pingers_screens::device::Bytes::new(total),
    )
}

const fn words_source(source: MapHostSource) -> WordsSource {
    match source {
        MapHostSource::UserOverride => WordsSource::UserOverride,
        MapHostSource::Server => WordsSource::Server,
        MapHostSource::Publisher => WordsSource::Publisher,
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    /// Checksums only need to be well formed here: nothing is downloaded.
    fn manifest() -> MapManifest {
        MapManifest {
            id: "oceania/test".into(),
            name: "Test".into(),
            group: Some("Oceania".into()),
            west: 144.0,
            south: -38.0,
            east: 145.0,
            north: -37.0,
            max_zoom: 15,
            build_date: "2026-11-12".into(),
            pack: MapFile {
                file: "oceania.test.pmtiles".into(),
                size: 4,
                sha256: "ab".repeat(32),
                entries: None,
            },
            index: MapFile {
                file: "oceania.test.places.tsv".into(),
                size: 3,
                sha256: "cd".repeat(32),
                entries: Some(1),
            },
        }
    }

    #[test]
    fn manifests_and_offers_cross_the_boundary_both_ways() -> Result<(), CoreError> {
        let manifest = manifest();
        let parsed = RegionManifest::try_from(&manifest)?;
        assert_eq!(MapManifest::from(&parsed), manifest);
        let offer = MapOffer {
            entry: MapEntry::Ready {
                manifest: manifest.clone(),
            },
            host: MapHostInfo {
                url: "https://maps.example".into(),
                source: MapHostSource::Server,
            },
        };
        let region = catalog::RegionOffer::try_from(&offer)?;
        assert_eq!(MapOffer::from(&region), offer);
        let mut escaping = manifest;
        escaping.pack.file = "../oceania.test.pmtiles".into();
        assert!(RegionManifest::try_from(&escaping).is_err());
        let bad_id = MapOffer {
            entry: MapEntry::OnDemand {
                id: "../x".into(),
                name: "X".into(),
                group: None,
            },
            ..offer
        };
        assert!(catalog::RegionOffer::try_from(&bad_id).is_err());
        Ok(())
    }

    #[test]
    fn settings_choose_hosts_and_route() -> Result<(), CoreError> {
        let settings = MapSettings {
            user_override: Some("  ".into()),
            publisher: Some("https://publisher.example".into()),
            through_relay: false,
        };
        let hosts = settings.hosts();
        assert_eq!(hosts.user_override, None);
        assert_eq!(
            hosts.publisher,
            Some(MapHostUrl::new("https://publisher.example".to_owned())?)
        );
        assert_eq!(settings.route(), ports::MapRoute::Direct);
        let relayed = MapSettings {
            through_relay: true,
            ..settings
        };
        assert_eq!(relayed.route(), ports::MapRoute::Relay);
        Ok(())
    }

    #[test]
    fn failures_and_problems_name_what_the_picker_says() {
        for (error, failure) in [
            (MapsError::Checksum, MapFailure::Checksum),
            (MapsError::NotOffered, MapFailure::NotOffered),
            (
                MapsError::Preparing {
                    retry_after: Duration::from_secs(1),
                },
                MapFailure::Preparing,
            ),
            (MapsError::NotThroughRelay, MapFailure::NotThroughRelay),
            (MapsError::NoRelayRoute, MapFailure::NoRelayRoute),
            (MapsError::KeyChanged, MapFailure::KeyChanged),
            (MapsError::KeyNotPinned, MapFailure::NotThroughRelay),
            (MapsError::Status { status: 500 }, MapFailure::Network),
        ] {
            assert_eq!(MapFailure::from(&error), failure);
        }
        for (failure, problem) in [
            (
                HostFailure::NotThroughRelay,
                MapHostProblem::NotThroughRelay,
            ),
            (HostFailure::NoRelayRoute, MapHostProblem::NoRelayRoute),
            (HostFailure::KeyChanged, MapHostProblem::KeyChanged),
            (HostFailure::Unreachable, MapHostProblem::Unreachable),
        ] {
            assert_eq!(MapHostProblem::from(failure), problem);
        }
    }

    #[test]
    fn downloaded_maps_words_name_the_host_and_sizes() {
        let words = downloaded_maps_words("maps.pingers.org".into(), MapHostSource::Server, true);
        assert_eq!(words.host_title, "From maps.pingers.org");
        assert!(
            words
                .host_detail
                .starts_with("The map host your Pingers server recommends.")
        );
        assert_eq!(size_words(184_000_000), "184 MB");
        assert_eq!(
            downloading_words(120_000_000, 312_000_000),
            "Downloading 120 MB of 312 MB"
        );
    }

    /// What a listener heard.
    #[derive(Default)]
    struct Heard(std::sync::Mutex<Vec<(u64, u64)>>);

    impl MapDownloadListener for Heard {
        fn downloaded(&self, done: u64, total: u64) {
            if let Ok(mut heard) = self.0.lock() {
                heard.push((done, total));
            }
        }

        fn preparing(&self, retry_after_seconds: u64) {
            if let Ok(mut heard) = self.0.lock() {
                heard.push((retry_after_seconds, 0));
            }
        }
    }

    #[test]
    fn progress_reaches_the_apps_listener() {
        let heard = Arc::new(Heard::default());
        let adapter = ListenerAdapter(heard.clone());
        adapter.downloaded(5, 10);
        adapter.preparing(Duration::from_secs(30));
        assert_eq!(
            heard
                .0
                .lock()
                .map(|heard| heard.clone())
                .unwrap_or_default(),
            [(5, 10), (30, 0)]
        );
    }

    #[test]
    fn words_come_from_pingers_words() {
        let words = maps_words("maps.example".into(), MapHostSource::UserOverride, false);
        assert_eq!(words.row, "Maps: maps.example");
        assert_eq!(words.source, "You chose this map host");
        assert_eq!(words.can_see.len(), 2);
    }
}
