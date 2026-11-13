// SPDX-License-Identifier: AGPL-3.0-only
//! The server's configuration file, read once at start.
//!
//! ```toml
//! listen = "0.0.0.0:8080"
//! logging = "off"                  # or "counts": request totals every counts_interval_s
//! counts_interval_s = 3600
//! request_timeout_s = 30           # how long a client may take to send a request body
//!
//! [ohttp]
//! relays = ["10.0.0.5", "fd00::5"] # the only addresses POST /ohttp is accepted from
//!
//! [[ohttp.keys]]                   # the first is served at GET /v1/ohttp/config; the others
//! key_id = 1                       # still open requests while clients move to it (rotation)
//! secret = "<64 hex>"
//!
//! [metrics]
//! allow = ["127.0.0.1"]            # who may GET /metrics; without it there is no such route
//! ```
//!
//! The relay-only rule (design B.12) is an address allow-list: the relay leg runs on a private
//! network or tunnel, as the reference relay's documentation says. The file holds the gateway's
//! private keys: keep it readable by the server only.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use pingers_ohttp::{GatewayKey, KeyConfig, OhttpError};
use serde::Deserialize;
use zeroize::{Zeroize as _, Zeroizing};

/// Why a configuration was refused. Never carries key material.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// Not valid TOML, or a field is missing, unknown or of the wrong type.
    #[error("configuration: syntax or schema error at line {line}")]
    Syntax {
        /// The line the parser stopped at (0 if unknown).
        line: usize,
    },
    /// A key secret is not 64 hex digits.
    #[error("configuration: ohttp.keys.secret must be 64 hex digits")]
    BadHex,
    /// A key secret is not a valid X25519 private key.
    #[error("configuration: an OHTTP key cannot be loaded")]
    BadKey(#[source] OhttpError),
    /// No OHTTP key is configured, so the gateway could open nothing.
    #[error("configuration: at least one [[ohttp.keys]] entry is needed")]
    NoOhttpKey,
    /// Two keys share an id, so requests could not say which one they used.
    #[error("configuration: OHTTP key id {key_id} is used twice")]
    DuplicateKeyId {
        /// The repeated id.
        key_id: u8,
    },
    /// No relay is allowed, so no anonymous request could ever arrive (design B.12: a relay
    /// is mandatory).
    #[error("configuration: ohttp.relays must list at least one relay address")]
    NoRelays,
    /// A duration is zero.
    #[error("configuration: {field} must be positive")]
    OutOfRange {
        /// The field.
        field: &'static str,
    },
}

/// Whether the server reports anything (design §9: logs off by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logging {
    /// Nothing is written.
    Off,
    /// Request totals per channel and outcome, once per interval. Never paths, identifiers,
    /// addresses or single requests.
    Counts {
        /// How often.
        interval: Duration,
    },
}

/// A set of peer addresses, compared after unmapping IPv4-mapped IPv6 addresses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddressList {
    addresses: Vec<IpAddr>,
}

impl AddressList {
    /// A list of exactly these addresses.
    #[must_use]
    pub fn new(addresses: Vec<IpAddr>) -> Self {
        Self {
            addresses: addresses
                .into_iter()
                .map(|address| address.to_canonical())
                .collect(),
        }
    }

    /// Whether `address` is on the list.
    #[must_use]
    pub fn contains(&self, address: IpAddr) -> bool {
        self.addresses.contains(&address.to_canonical())
    }
}

/// The relays allowed to reach `POST /ohttp`: never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayAllowList {
    addresses: AddressList,
}

impl RelayAllowList {
    /// Checks that at least one relay is allowed.
    ///
    /// # Errors
    ///
    /// [`ConfigError::NoRelays`] if `addresses` is empty.
    pub fn new(addresses: Vec<IpAddr>) -> Result<Self, ConfigError> {
        if addresses.is_empty() {
            return Err(ConfigError::NoRelays);
        }
        Ok(Self {
            addresses: AddressList::new(addresses),
        })
    }

    /// Whether a connection from `address` may use the gateway.
    #[must_use]
    pub fn allows(&self, address: IpAddr) -> bool {
        self.addresses.contains(address)
    }
}

/// The gateway's keys: at least one, with distinct ids. The first is the one served.
pub struct OhttpKeys {
    keys: Vec<GatewayKey>,
    served: KeyConfig,
}

impl OhttpKeys {
    /// Checks the keys. The first is served at `GET /v1/ohttp/config`; the rest still open
    /// requests during a rotation.
    ///
    /// # Errors
    ///
    /// [`ConfigError::NoOhttpKey`] if `keys` is empty, [`ConfigError::DuplicateKeyId`] if two
    /// share an id.
    pub fn new(keys: Vec<GatewayKey>) -> Result<Self, ConfigError> {
        let served = keys.first().ok_or(ConfigError::NoOhttpKey)?.config();
        let mut seen = Vec::with_capacity(keys.len());
        for key in &keys {
            let key_id = key.config().key_id();
            if seen.contains(&key_id) {
                return Err(ConfigError::DuplicateKeyId { key_id });
            }
            seen.push(key_id);
        }
        Ok(Self { keys, served })
    }

    /// The key config clients fetch and pin.
    #[must_use]
    pub fn served(&self) -> &KeyConfig {
        &self.served
    }

    pub(crate) fn into_keys(self) -> Vec<GatewayKey> {
        self.keys
    }
}

impl core::fmt::Debug for OhttpKeys {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("OhttpKeys")
            .field("count", &self.keys.len())
            .field("served_key_id", &self.served.key_id())
            .finish()
    }
}

/// Parsed and checked configuration.
#[derive(Debug)]
pub struct ServerConfig {
    /// Where the binary listens (the library is handed a bound listener instead).
    pub listen: SocketAddr,
    /// Whether anything is written.
    pub logging: Logging,
    /// How long a client may take to send a whole request body (one-shot requests, the head
    /// of a live channel, direct requests). Live channel bodies are not limited.
    pub request_timeout: Duration,
    /// The gateway's keys.
    pub ohttp_keys: OhttpKeys,
    /// Who may `POST /ohttp`.
    pub relays: RelayAllowList,
    /// Who may `GET /metrics`; empty means the route does not exist.
    pub metrics: AddressList,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    listen: SocketAddr,
    #[serde(default)]
    logging: FileLogging,
    #[serde(default = "default_counts_interval")]
    counts_interval_s: u64,
    #[serde(default = "default_request_timeout")]
    request_timeout_s: u64,
    ohttp: FileOhttp,
    #[serde(default)]
    metrics: FileMetrics,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FileLogging {
    #[default]
    Off,
    Counts,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileOhttp {
    relays: Vec<IpAddr>,
    #[serde(default)]
    keys: Vec<FileKey>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileKey {
    key_id: u8,
    secret: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMetrics {
    #[serde(default)]
    allow: Vec<IpAddr>,
}

const fn default_counts_interval() -> u64 {
    3600
}

const fn default_request_timeout() -> u64 {
    30
}

impl ServerConfig {
    /// Parses and checks a configuration file's text.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Syntax`] for TOML or schema errors, or the variant naming the rule that
    /// failed.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let file: File = toml::from_str(text).map_err(|error| ConfigError::Syntax {
            line: error.span().map_or(0, |span| line_of(text, span.start)),
        })?;
        let logging = match file.logging {
            FileLogging::Off => Logging::Off,
            FileLogging::Counts => Logging::Counts {
                interval: positive_seconds("counts_interval_s", file.counts_interval_s)?,
            },
        };
        let keys = file
            .ohttp
            .keys
            .into_iter()
            .map(gateway_key)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            listen: file.listen,
            logging,
            request_timeout: positive_seconds("request_timeout_s", file.request_timeout_s)?,
            ohttp_keys: OhttpKeys::new(keys)?,
            relays: RelayAllowList::new(file.ohttp.relays)?,
            metrics: AddressList::new(file.metrics.allow),
        })
    }
}

fn line_of(text: &str, offset: usize) -> usize {
    text.bytes()
        .take(offset)
        .filter(|byte| *byte == b'\n')
        .count()
        .saturating_add(1)
}

fn positive_seconds(field: &'static str, seconds: u64) -> Result<Duration, ConfigError> {
    match seconds {
        0 => Err(ConfigError::OutOfRange { field }),
        seconds => Ok(Duration::from_secs(seconds)),
    }
}

fn gateway_key(mut key: FileKey) -> Result<GatewayKey, ConfigError> {
    let mut secret = Zeroizing::new([0_u8; 32]);
    let decoded = hex::decode_to_slice(&key.secret, secret.as_mut_slice());
    key.secret.zeroize();
    decoded.map_err(|_| ConfigError::BadHex)?;
    GatewayKey::from_bytes(key.key_id, secret).map_err(ConfigError::BadKey)
}

#[cfg(test)]
mod tests;
