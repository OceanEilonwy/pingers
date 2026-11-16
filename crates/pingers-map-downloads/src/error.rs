// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type. No variant carries a region, a host, a path or a URL.

use std::time::Duration;

/// Why a catalog or a download failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MapsError {
    /// The request could not be made or answered.
    #[error("map request failed")]
    Transport(#[source] pingers_transport::TransportError),
    /// The map host answered with an unexpected status.
    #[error("map host answered {status}")]
    Status {
        /// The status code.
        status: u16,
    },
    /// A cache-mode map host is cutting the region; ask again after this long.
    #[error("the map host is preparing this area")]
    Preparing {
        /// The host's `Retry-After`.
        retry_after: Duration,
    },
    /// The map host does not offer the file (404).
    #[error("the map host does not offer this area")]
    NotOffered,
    /// A downloaded file did not match its manifest's SHA-256, or was longer than it says.
    #[error("a downloaded file did not match its checksum")]
    Checksum,
    /// The map host's answer was not what was asked for: a piece of the wrong range or
    /// length, or a body that is not a catalog or manifest.
    #[error("the map host answered with something else")]
    BadAnswer,
    /// The host is not the one the Pingers server forwards to, so it cannot be reached through
    /// the privacy relay.
    #[error("this map host cannot be reached through the privacy relay")]
    NotThroughRelay,
    /// The relay route is not set up: the app has no Pingers server yet.
    #[error("downloads through the privacy relay need a Pingers server")]
    NoRelayRoute,
    /// The map host's gateway key differs from the one pinned on first use: relayed
    /// downloads from it stop until the pin is forgotten.
    #[error("the map host's key changed")]
    KeyChanged,
    /// Sealing or opening a relayed request failed.
    #[error("relayed map request could not be sealed or opened")]
    Ohttp(#[source] pingers_ohttp::OhttpError),
    /// The pin store could not be read or written.
    #[error("the map host key pin could not be stored")]
    PinStore,
    /// The maps directory could not be read or written.
    #[error("the maps directory could not be used")]
    Io(#[source] std::io::Error),
    /// A blocking task failed to finish.
    #[error("internal error")]
    Internal,
}

impl From<pingers_transport::TransportError> for MapsError {
    fn from(error: pingers_transport::TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<pingers_ohttp::OhttpError> for MapsError {
    fn from(error: pingers_ohttp::OhttpError) -> Self {
        Self::Ohttp(error)
    }
}

impl From<std::io::Error> for MapsError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
