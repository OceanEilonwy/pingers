// SPDX-License-Identifier: AGPL-3.0-only
//! Why a scenario step failed.

/// Why a scenario step failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum E2eError {
    /// A step did not go as the scenario expects.
    #[error("{0}")]
    Step(String),
    /// The server answered with an unexpected status.
    #[error("{what}: HTTP {status}")]
    Status {
        /// The request.
        what: String,
        /// The status.
        status: u16,
    },
    /// A socket or file operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The relay transport failed.
    #[error(transparent)]
    Transport(#[from] pingers_transport::TransportError),
    /// OHTTP set-up failed.
    #[error(transparent)]
    Ohttp(#[from] pingers_ohttp::OhttpError),
    /// An encoding failed.
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    /// The presence protocol refused something.
    #[error(transparent)]
    Presence(#[from] pingers_presence::PresenceError),
    /// MLS refused something.
    #[error(transparent)]
    Mls(#[from] pingers_mls::MlsError),
    /// A crypto helper failed.
    #[error(transparent)]
    Crypto(#[from] pingers_crypto::CryptoError),
    /// The delivery service refused something.
    #[error(transparent)]
    Delivery(#[from] pingers_delivery::DeliveryError),
    /// A fix or fence was invalid.
    #[error(transparent)]
    Geo(#[from] pingers_geo::GeoError),
    /// A time value was out of range.
    #[error(transparent)]
    Time(#[from] pingers_time::TimeError),
    /// The server configuration was rejected.
    #[error(transparent)]
    Config(#[from] pingers_server::ConfigError),
    /// An SQLite file could not be inspected.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// A direct HTTP request failed.
    #[error(transparent)]
    Http(#[from] hyper::Error),
    /// A direct HTTP request could not be built.
    #[error(transparent)]
    HttpRequest(#[from] hyper::http::Error),
}

impl E2eError {
    /// A failed scenario step.
    pub fn step(message: impl Into<String>) -> Self {
        Self::Step(message.into())
    }
}
