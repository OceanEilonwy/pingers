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
//! hash_key = "<64 hex>"            # keys the credential hashes and the token rate limits
//!
//! [[keys]]
//! key_id = 1
//! private_key = "<64 hex>"            # the public key is derived from it
//! activated_at = 1790000000        # Unix seconds
//! retired_at = 1795000000          # optional; at least 30 days after the next key activates
//!
//! [[servers]]
//! server_id = "pingers.example"
//! credential_hash = "<64 hex>"     # HMAC of the server's credential; see below
//! pushes_per_minute = 600
//!
//! [unifiedpush]
//! allow_insecure_local_endpoints = false   # true only for tests against a local ntfy
//!
//! [fcm]                            # optional: FCM HTTP v1
//! project_id = "my-project"        # the Firebase project id
//! service_account_file = "/etc/pingers-push-relay/fcm-service-account.json"
//! endpoint = "https://fcm.googleapis.com/"   # optional; the default
//! ```
//!
//! The file holds private keys and the hash key: keep it readable by the relay only. It holds
//! no server credential, only its keyed hash, so a copy of the file does not let anyone push.
//!
//! Issuing a server credential (pingers-crypto's `keyed_hash` with purpose
//! `ServerCredential`: `HMAC-SHA-256(hash_key, "pingers/v1/relay-credential" || credential)`):
//!
//! ```sh
//! openssl rand -hex 32 > hash_key           # once per relay; paste into hash_key
//! credential=$(openssl rand -hex 32)        # give this to the server operator only
//! printf '%s%s' 'pingers/v1/relay-credential' "$credential" \
//!   | openssl dgst -sha256 -mac HMAC -macopt hexkey:"$(cat hash_key)" -r | cut -d' ' -f1
//! ```
//!
//! The last line prints the `credential_hash` for the server's entry. The server presents the
//! credential as `Authorization: Bearer <credential>`; credentials shorter than 32 characters
//! are refused. Changing `hash_key` invalidates every credential hash.
//!
//! # FCM setup
//!
//! The relay signs its own OAuth 2 assertions with a Google service-account key and exchanges
//! them for access tokens, which it caches and renews five minutes before they expire. To set
//! it up:
//!
//! 1. In the Google Cloud console for the Firebase project, under IAM & Admin > Service
//!    Accounts, create a service account used for nothing else (for example
//!    `pingers-push-relay`), and grant it the single role **Firebase Cloud Messaging API
//!    Admin** (`roles/firebasecloudmessaging.admin`). Do not use the Firebase Admin SDK
//!    account the console creates: it can do far more than send messages.
//! 2. Make sure the Firebase Cloud Messaging API (V1) is enabled for the project.
//! 3. Under the account's Keys tab, add a JSON key and download it. Copy it to the relay host
//!    and make it readable by the relay's user only:
//!
//!    ```sh
//!    install -m 600 -o pingers-relay key.json /etc/pingers-push-relay/fcm-service-account.json
//!    shred -u key.json
//!    ```
//!
//! 4. Set `service_account_file` to that path and `project_id` to the Firebase project id.
//!
//! The relay reads `client_email`, `private_key`, `token_uri` and `private_key_id` from the file
//! at start and never writes, logs or reports any of them; a malformed file stops the relay
//! with an error naming the field, not its value. To rotate the key, add a new key to the
//! account, replace the file, restart the relay, then delete the old key in the console.

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use pingers_crypto::{KeyedHash, KeyedHashKey, PushRelayPrivateKey};
use pingers_proto::{PushRelayKeyId, TimeToLive};
use pingers_time::UnixSeconds;
use reqwest::Url;
use serde::Deserialize;
use zeroize::{Zeroize as _, Zeroizing};

use crate::credentials::{ServerEntry, ServerRegistry};
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
    /// The service account's JSON key file, read at start.
    pub service_account_file: PathBuf,
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
    pub max_ttl: TimeToLive,
    /// Keys and their rotation schedule.
    pub keys: KeyRing,
    /// Servers allowed to use the relay.
    pub servers: ServerRegistry,
    /// Keys the per-token rate-limit fingerprints: the configured `hash_key`, used with its
    /// own purpose label.
    pub token_hash_key: KeyedHashKey,
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
    hash_key: String,
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
    activated_at: u64,
    retired_at: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServer {
    server_id: String,
    credential_hash: String,
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
    service_account_file: PathBuf,
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
        let mut file: FileConfig = toml::from_str(text).map_err(|error| ConfigError::Syntax {
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
        let hash_key = hex_32("hash_key", &file.hash_key);
        file.hash_key.zeroize();
        let hash_key = hash_key?;
        let token_hash_key = KeyedHashKey::from_bytes(Zeroizing::new(*hash_key));
        Ok(Self {
            listen: file.listen,
            operator: file.operator,
            logging,
            upstream_timeout,
            token_limit: rate("token_pushes_per_minute", file.token_pushes_per_minute)?,
            max_ttl: TimeToLive::new(Duration::from_secs(file.max_ttl_s))
                .map_err(|_range| ConfigError::OutOfRange { field: "max_ttl_s" })?,
            keys: KeyRing::new(
                file.keys
                    .into_iter()
                    .map(relay_key)
                    .collect::<Result<_, _>>()?,
            )?,
            servers: ServerRegistry::new(
                KeyedHashKey::from_bytes(hash_key),
                file.servers
                    .into_iter()
                    .map(server_entry)
                    .collect::<Result<_, _>>()?,
            )?,
            token_hash_key,
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
    RelayKey::new(
        PushRelayKeyId::new(key.key_id),
        PushRelayPrivateKey::from_bytes(private_key?),
        KeyLifetime {
            activated_at: UnixSeconds::new(key.activated_at),
            retired_at: key.retired_at.map(UnixSeconds::new),
        },
    )
    .map_err(|_error| ConfigError::InvalidKey { key_id: key.key_id })
}

fn server_entry(server: FileServer) -> Result<ServerEntry, ConfigError> {
    Ok(ServerEntry {
        credential_hash: KeyedHash::from_bytes(*hex_32(
            "servers.credential_hash",
            &server.credential_hash,
        )?),
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
        service_account_file: fcm.service_account_file,
    })
}

#[cfg(test)]
mod tests;
