// SPDX-License-Identifier: AGPL-3.0-only
//! The relay's configuration file (design C.1a), read once at start.
//!
//! ```toml
//! listen = "0.0.0.0:8443"
//! operator = "Example Relay Ltd"
//! logging = "off"                  # or "counts": outcome totals every counts_interval_s
//! counts_interval_s = 3600
//! upstream_timeout_ms = 10000
//! token_pushes_per_minute = 30
//! max_ttl_s = 2592000
//!
//! [[keys]]
//! key_id = 1
//! private_key = "<64 hex>"
//! public_key = "<64 hex>"
//! activated_at = 1790000000        # Unix seconds
//! retired_at = 1795000000          # optional; at least 30 days after the next key activates
//!
//! [[servers]]
//! server_id = "pingers.example"
//! credential = "<random, at least 32 characters>"
//! pushes_per_minute = 600
//!
//! [unifiedpush]
//! allow_insecure_local_endpoints = false   # true only for tests against a local ntfy
//!
//! [fcm]                            # optional: FCM HTTP v1
//! project_id = "my-project"
//! access_token_file = "/run/secrets/fcm-access-token"
//! endpoint = "https://fcm.googleapis.com/"
//! ```
//!
//! The file holds private keys and credentials: keep it readable by the relay only.

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use pingers_crypto::{PushRelayPrivateKey, PushRelayPublicKey};
use pingers_time::UnixSeconds;
use reqwest::Url;
use serde::Deserialize;
use zeroize::{Zeroize as _, Zeroizing};

use crate::credentials::{ServerCredential, ServerEntry, ServerRegistry};
use crate::delivery::EndpointPolicy;
use crate::error::ConfigError;
use crate::keys::{KeyLifetime, KeyRing, RelayKey};
use crate::rate::RateLimit;

/// Whether the relay reports anything (design A.2: off by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logging {
    /// Nothing is written.
    Off,
    /// Outcome totals, once per interval. Never tokens, servers, addresses or single requests.
    Counts {
        /// How often.
        interval: Duration,
    },
}

/// FCM HTTP v1 settings from the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FcmConfig {
    /// The Firebase project.
    pub project_id: String,
    /// Where to send; normally `https://fcm.googleapis.com/`.
    pub endpoint: Url,
    /// A file holding an OAuth 2 access token for the project.
    pub access_token_file: PathBuf,
}

/// The validated configuration.
pub struct RelayConfig {
    /// Address to listen on.
    pub listen: SocketAddr,
    /// Who runs the relay; published in the key config.
    pub operator: String,
    /// Reporting.
    pub logging: Logging,
    /// The longest to wait for a push service.
    pub upstream_timeout: Duration,
    /// Per-token limit.
    pub token_limit: RateLimit,
    /// Published: the longest lifetime a device should seal.
    pub max_ttl: Duration,
    /// Keys and their rotation schedule.
    pub keys: KeyRing,
    /// Servers allowed to use the relay.
    pub servers: ServerRegistry,
    /// UnifiedPush, if enabled.
    pub unifiedpush: Option<EndpointPolicy>,
    /// FCM, if enabled.
    pub fcm: Option<FcmConfig>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum FileLogging {
    Off,
    Counts,
}

const fn default_logging() -> FileLogging {
    FileLogging::Off
}
const fn default_counts_interval() -> u64 {
    3600
}
const fn default_upstream_timeout() -> u64 {
    10_000
}
const fn default_token_rate() -> u32 {
    30
}
const fn default_max_ttl() -> u64 {
    30 * 24 * 60 * 60
}
fn default_fcm_endpoint() -> String {
    "https://fcm.googleapis.com/".to_owned()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    listen: SocketAddr,
    operator: String,
    #[serde(default = "default_logging")]
    logging: FileLogging,
    #[serde(default = "default_counts_interval")]
    counts_interval_s: u64,
    #[serde(default = "default_upstream_timeout")]
    upstream_timeout_ms: u64,
    #[serde(default = "default_token_rate")]
    token_pushes_per_minute: u32,
    #[serde(default = "default_max_ttl")]
    max_ttl_s: u64,
    #[serde(default)]
    keys: Vec<FileKey>,
    #[serde(default)]
    servers: Vec<FileServer>,
    unifiedpush: Option<FileUnifiedPush>,
    fcm: Option<FileFcm>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileKey {
    key_id: u8,
    private_key: String,
    public_key: String,
    activated_at: u64,
    retired_at: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServer {
    server_id: String,
    credential: String,
    pushes_per_minute: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileUnifiedPush {
    #[serde(default)]
    allow_insecure_local_endpoints: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFcm {
    project_id: String,
    access_token_file: PathBuf,
    #[serde(default = "default_fcm_endpoint")]
    endpoint: String,
}

impl RelayConfig {
    /// Parses and checks a configuration file's text.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Syntax`] for TOML or schema errors, or the variant naming the field or
    /// rule that failed.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let file: FileConfig = toml::from_str(text).map_err(|error| ConfigError::Syntax {
            line: error.span().map_or(0, |span| line_of(text, span.start)),
        })?;
        if file.unifiedpush.is_none() && file.fcm.is_none() {
            return Err(ConfigError::NoPlatform);
        }
        let logging = match file.logging {
            FileLogging::Off => Logging::Off,
            FileLogging::Counts => Logging::Counts {
                interval: positive_seconds("counts_interval_s", file.counts_interval_s)?,
            },
        };
        let upstream_timeout = match file.upstream_timeout_ms {
            0 => {
                return Err(ConfigError::OutOfRange {
                    field: "upstream_timeout_ms",
                });
            }
            millis => Duration::from_millis(millis),
        };
        let fcm = file.fcm.map(fcm_config).transpose()?;
        Ok(Self {
            listen: file.listen,
            operator: file.operator,
            logging,
            upstream_timeout,
            token_limit: rate("token_pushes_per_minute", file.token_pushes_per_minute)?,
            max_ttl: positive_seconds("max_ttl_s", file.max_ttl_s)?,
            keys: KeyRing::new(
                file.keys
                    .into_iter()
                    .map(relay_key)
                    .collect::<Result<_, _>>()?,
            )?,
            servers: ServerRegistry::new(
                file.servers
                    .into_iter()
                    .map(server_entry)
                    .collect::<Result<_, _>>()?,
            )?,
            unifiedpush: file.unifiedpush.map(|unifiedpush| {
                if unifiedpush.allow_insecure_local_endpoints {
                    EndpointPolicy::AllowInsecureLocal
                } else {
                    EndpointPolicy::PublicHttpsOnly
                }
            }),
            fcm,
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

fn rate(field: &'static str, per_minute: u32) -> Result<RateLimit, ConfigError> {
    NonZeroU32::new(per_minute)
        .map(RateLimit::per_minute)
        .ok_or(ConfigError::OutOfRange { field })
}

fn hex_32(field: &'static str, text: &str) -> Result<Zeroizing<[u8; 32]>, ConfigError> {
    let mut bytes = Zeroizing::new([0; 32]);
    hex::decode_to_slice(text, bytes.as_mut_slice())
        .map_err(|_| ConfigError::BadHex { field, bytes: 32 })?;
    Ok(bytes)
}

fn relay_key(mut key: FileKey) -> Result<RelayKey, ConfigError> {
    let private_key = hex_32("keys.private_key", &key.private_key);
    key.private_key.zeroize();
    let public_key = hex_32("keys.public_key", &key.public_key)?;
    Ok(RelayKey::new(
        PushRelayPrivateKey::from_bytes(private_key?),
        PushRelayPublicKey::new(key.key_id, *public_key),
        KeyLifetime {
            activated_at: UnixSeconds::new(key.activated_at),
            retired_at: key.retired_at.map(UnixSeconds::new),
        },
    ))
}

fn server_entry(server: FileServer) -> Result<ServerEntry, ConfigError> {
    Ok(ServerEntry {
        credential: ServerCredential::new(Zeroizing::new(server.credential.into_bytes())),
        limit: rate("servers.pushes_per_minute", server.pushes_per_minute)?,
        server_id: server.server_id,
    })
}

fn fcm_config(fcm: FileFcm) -> Result<FcmConfig, ConfigError> {
    let endpoint = Url::parse(&fcm.endpoint).map_err(|_| ConfigError::BadFcmEndpoint)?;
    if !matches!(endpoint.scheme(), "https" | "http") {
        return Err(ConfigError::BadFcmEndpoint);
    }
    if !crate::delivery::is_valid_fcm_project_id(&fcm.project_id) {
        return Err(ConfigError::OutOfRange {
            field: "fcm.project_id",
        });
    }
    Ok(FcmConfig {
        project_id: fcm.project_id,
        endpoint,
        access_token_file: fcm.access_token_file,
    })
}

#[cfg(test)]
mod tests;
