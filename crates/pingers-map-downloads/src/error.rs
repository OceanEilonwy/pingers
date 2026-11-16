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
    /// The key config the server forwarded for the map host is not among the pins: relayed
    /// downloads from it stop (the server may have substituted a key of its own).
    #[error("the map host's key is not pinned")]
    KeyChanged,
    /// No map host key pins are known for the server, so nothing is fetched through its relay.
    #[error("no map host key pins")]
    KeyNotPinned,
    /// Sealing or opening a relayed request failed.
    #[error("relayed map request could not be sealed or opened")]
    Ohttp(#[source] pingers_ohttp::OhttpError),
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
