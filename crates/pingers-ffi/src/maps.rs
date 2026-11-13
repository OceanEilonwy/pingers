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
    let source = match source {
        MapHostSource::UserOverride => WordsSource::UserOverride,
        MapHostSource::Server => WordsSource::Server,
        MapHostSource::Publisher => WordsSource::Publisher,
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_come_from_pingers_words() {
        let words = maps_words("maps.example".into(), MapHostSource::UserOverride, false);
        assert_eq!(words.row, "Maps: maps.example");
        assert_eq!(words.source, "You chose this map host");
        assert_eq!(words.can_see.len(), 2);
    }
}
