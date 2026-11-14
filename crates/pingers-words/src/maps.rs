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

#[cfg(test)]
mod tests {
    use super::*;

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
