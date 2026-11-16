// SPDX-License-Identifier: AGPL-3.0-only
//! The words for the map host (pingers-words `maps`). The map host is chosen and used by the
//! app shell (it downloads region packs and renders them with MapLibre), so the shell says
//! which host is in use and pingers-words writes what that host learns.

use pingers_words::Language;
use pingers_words::maps::{MapHostSource as WordsSource, MapsConfiguration, maps};

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
    use super::*;

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

    #[test]
    fn words_come_from_pingers_words() {
        let words = maps_words("maps.example".into(), MapHostSource::UserOverride, false);
        assert_eq!(words.row, "Maps: maps.example");
        assert_eq!(words.source, "You chose this map host");
        assert_eq!(words.can_see.len(), 2);
    }
}
