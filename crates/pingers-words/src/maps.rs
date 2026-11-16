// SPDX-License-Identifier: AGPL-3.0-only
//! Maps: the map host row on Advanced and what the map host learns, for "What your server
//! sees" (design B.12, "Map host").
//!
//! The map host is another operator, separate from the Pingers server and the relays. It serves
//! whole region packs; the app never asks it for tiles of the area on screen and searches
//! places on the phone. What it can learn is the device's internet address and which regions
//! it downloads, unless downloads go through the privacy relay. When the chosen host does not
//! carry a region, the app falls back down the chain (user override, the server's host, the
//! publisher's) for that region only, and the fallback host learns that download too.

use crate::Language;

/// Where the map host in use came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MapHostSource {
    /// The user set it in Advanced.
    UserOverride,
    /// The Pingers server recommends it (`map_host` in `/v1/params`).
    Server,
    /// The publisher's built-in default.
    Publisher,
}

/// What the map host lines are written from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapsConfiguration {
    /// The map host's host name.
    pub host: String,
    /// Where it came from.
    pub source: MapHostSource,
    /// Whether "Download maps through privacy relay" is on.
    pub through_relay: bool,
}

/// The maps lines, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapsText {
    /// "Maps", the section heading.
    pub heading: String,
    /// "Maps: maps.example.org", the row on Advanced.
    pub row: String,
    /// Under the row: where the host came from.
    pub source: String,
    /// What the map host can see.
    pub can_see: Vec<String>,
    /// What it never sees.
    pub never_sees: Vec<String>,
    /// The per-region fallback, explained.
    pub fallback: String,
    /// The relay setting's label.
    pub relay_setting: String,
    /// The relay setting's explanation.
    pub relay_detail: String,
}

/// The maps lines for a configuration.
#[must_use]
pub fn maps(language: Language, configuration: &MapsConfiguration) -> MapsText {
    let Language::English = language;
    let source = match configuration.source {
        MapHostSource::UserOverride => "You chose this map host",
        MapHostSource::Server => "Recommended by your Pingers server",
        MapHostSource::Publisher => "The Pingers app's default map host",
    };
    let address = if configuration.through_relay {
        "Not your internet address: downloads go through your privacy relay and your Pingers \
         server, which pass them on sealed"
    } else {
        "Your internet address, when you download a map"
    };
    let fallback = if configuration.through_relay {
        "Through the privacy relay, maps come only from your server's map host: your server \
         passes map downloads to that host and no other. Your server and the relay see that \
         you download a map and roughly how large it is, never which area."
    } else {
        "If this host doesn't have an area, Pingers gets that area from the next host instead: \
         the one you chose, then your server's, then the app's default. That host learns the \
         download too."
    };
    MapsText {
        heading: "Maps".into(),
        row: format!("Maps: {}", configuration.host),
        source: source.into(),
        can_see: vec![address.into(), "Which areas you download maps for".into()],
        never_sees: vec![
            "Where you are or which part of the map you look at: maps are downloaded whole and \
             shown from this phone"
                .into(),
            "What you search for: search runs on this phone".into(),
            "Your Parties, Spots or anyone's location".into(),
        ],
        fallback: fallback.into(),
        relay_setting: "Download maps through privacy relay".into(),
        relay_detail: "Hides your internet address from the map host, and which areas you \
                       download from your server and the relay. Only your server's map host \
                       can be used. Downloads are slower."
            .into(),
    }
}

/// Downloaded maps (canvas `DownloadedMaps`), in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadedMapsText {
    /// "Downloaded maps".
    pub title: String,
    /// "Back to Settings", for screen readers.
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

/// Downloaded maps, in words.
#[must_use]
pub fn downloaded_maps(
    language: Language,
    configuration: &MapsConfiguration,
) -> DownloadedMapsText {
    let Language::English = language;
    let who = match configuration.source {
        MapHostSource::UserOverride => "The map host you chose.",
        MapHostSource::Server => "The map host your Pingers server recommends.",
        MapHostSource::Publisher => "The Pingers app's default map host.",
    };
    let sees = if configuration.through_relay {
        "It sees which areas you download, not your internet address: downloads go through your \
         privacy relay and your Pingers server, sealed so that neither of them sees the area."
    } else {
        "It sees your internet address when you download a map, and which areas you download."
    };
    DownloadedMapsText {
        title: "Downloaded maps".into(),
        back: "Back to Settings".into(),
        intro: "Maps are downloaded whole, one area at a time, and shown from this phone. Nobody \
                learns which part of the map you look at or what you search for."
            .into(),
        installed: "On this phone".into(),
        available: "Available".into(),
        update_available: "Update available".into(),
        update: "Update".into(),
        delete: "Delete".into(),
        download: "Download".into(),
        this_area: "This area".into(),
        host_title: format!("From {}", configuration.host),
        host_detail: format!("{who} {sees} Never where you are, your Parties or your Spots."),
        attribution: "Map data from OpenStreetMap contributors".into(),
    }
}

/// A download under way: "Downloading 120 MB of 312 MB".
#[must_use]
pub fn downloading(
    language: Language,
    done: pingers_screens::device::Bytes,
    total: pingers_screens::device::Bytes,
) -> String {
    format!(
        "Downloading {} of {}",
        crate::device::size(language, done),
        crate::device::size(language, total)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloaded_maps_read_like_the_canvas() {
        let text = downloaded_maps(
            Language::English,
            &MapsConfiguration {
                host: "maps.pingers.org".into(),
                source: MapHostSource::Publisher,
                through_relay: false,
            },
        );
        assert_eq!(text.title, "Downloaded maps");
        assert_eq!(text.host_title, "From maps.pingers.org");
        assert_eq!(
            text.host_detail,
            "The Pingers app's default map host. It sees your internet address when you download \
             a map, and which areas you download. Never where you are, your Parties or your Spots."
        );
        let relayed = downloaded_maps(
            Language::English,
            &MapsConfiguration {
                host: "maps.flat7.org".into(),
                source: MapHostSource::Server,
                through_relay: true,
            },
        );
        assert_eq!(
            relayed.host_detail,
            "The map host your Pingers server recommends. It sees which areas you download, not \
             your internet address: downloads go through your privacy relay and your Pingers \
             server, sealed so that neither of them sees the area. Never where you are, your \
             Parties or your Spots."
        );
        assert_eq!(text.attribution, "Map data from OpenStreetMap contributors");
        assert_eq!(
            downloading(
                Language::English,
                pingers_screens::device::Bytes::new(120_000_000),
                pingers_screens::device::Bytes::new(312_000_000)
            ),
            "Downloading 120 MB of 312 MB"
        );
    }

    #[test]
    fn the_relay_setting_changes_what_the_host_sees() {
        let direct = maps(
            Language::English,
            &MapsConfiguration {
                host: "maps.pingers.example".into(),
                source: MapHostSource::Publisher,
                through_relay: false,
            },
        );
        assert_eq!(direct.row, "Maps: maps.pingers.example");
        assert_eq!(
            direct.can_see,
            [
                "Your internet address, when you download a map",
                "Which areas you download maps for"
            ]
        );
        let relayed = maps(
            Language::English,
            &MapsConfiguration {
                host: "maps.flat7.org".into(),
                source: MapHostSource::Server,
                through_relay: true,
            },
        );
        assert!(
            relayed
                .can_see
                .first()
                .is_some_and(|line| line.starts_with("Not your"))
        );
        assert_eq!(relayed.source, "Recommended by your Pingers server");
        assert_eq!(relayed.never_sees.len(), 3);
        // Through the relay only the server's map host is used, and nothing names the area.
        assert!(
            relayed
                .fallback
                .contains("only from your server's map host")
        );
        assert!(relayed.fallback.contains("never which area"));
        assert!(direct.fallback.contains("learns the download too"));
        assert_ne!(direct.relay_detail, "");
    }
}
