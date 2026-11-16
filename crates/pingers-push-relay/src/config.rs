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
//! max_connections = 256            # connections held at once; one more is closed at once
//! idle_timeout_s = 30              # a connection with no request for this long is closed
//! hash_key = "<64 hex>"            # keys the credential hashes and the token rate limits
//!
//! [[keys]]
//! key_id = 1                       # 0 to 255, distinct
//! private_key = "<64 hex>"         # the public key is derived from it
//! activated_at = 1790000000        # Unix seconds
//! retired_at = 1795000000          # optional, 0 = not scheduled; at least 30 days after the
//!                                  # next key activates
//!
//! [[keys]]                         # the next key: published at GET /v1/config/next, and
//! key_id = 2                       # printed by `pins`, before it activates
//! private_key = "<64 hex>"
//! activated_at = 4102444800        # far in the future until the rotation
//! retired_at = 0
//!
//! [[servers]]
//! server_id = "pingers.example"
//! credential_hash = "<64 hex>"     # HMAC of the server's credential; see below
//! pushes_per_minute = 600
//!
//! [[servers]]
//! server_id = "local"
//! credential = "<at least 32 characters>"   # or the credential itself, hashed at load
//! pushes_per_minute = 600
//!
//! [unifiedpush]
//! allow_insecure_local_endpoints = false   # true only for tests against a local ntfy
//!
//! [fcm]                            # optional: FCM HTTP v1
//! project_id = "my-project"        # the Firebase project id; optional with service_account_json,
//!                                  # whose own project_id is used when it is left out
//! service_account_file = "/etc/pingers-push-relay/fcm-service-account.json"
//! # or, instead of the file, the key file's JSON itself (one line is fine):
//! # service_account_json = '{"type": "service_account", ...}'
//! endpoint = "https://fcm.googleapis.com/"   # optional; the default
//! ```
//!
//! Each `[[servers]]` entry has exactly one of `credential_hash` and `credential`, and `[fcm]`
//! exactly one of `service_account_file` and `service_account_json`. `retired_at = 0` means
//! the same as leaving it out, for templates that cannot leave a key out.
//!
//! The file holds private keys and the hash key: keep it readable by the relay only. With
//! `credential_hash` it holds no server credential, so a copy of the file does not let anyone
//! push. A plain `credential` (for a relay whose configuration is rendered into memory beside
//! the server's, as on an appliance) is hashed when the file is read and the text is wiped;
//! `service_account_json` is checked when the file is read and kept only in zeroized memory.
//! Configuration errors name fields, never values.
//!
//! Issuing a server credential (pingers-crypto's `keyed_hash` with purpose
//! `ServerCredential`: `HMAC-SHA-256(hash_key, "pingers/v1/relay-credential" || credential)`):
//!
//! ```sh
//! pingers-push-relay keygen > hash_key      # once per relay; paste into hash_key
//! pingers-push-relay keygen > credential    # give this to the server operator only
//! cat hash_key credential | pingers-push-relay credential-hash
//! ```
//!
//! `credential-hash` reads the hash key and the credential, one per line, on standard input
//! (never the command line, which other users can see) and prints the `credential_hash` for
//! the server's entry. The server presents the credential as `Authorization: Bearer
//! <credential>`; credentials shorter than 32 characters are refused. Changing `hash_key`
//! invalidates every `credential_hash` (a plain `credential` is simply hashed anew).
//!
//! # Rotating the HPKE key (two slots, yearly)
//!
//! Keep two `[[keys]]`: A active, and B with a far-future `activated_at`. The app pins both:
//! `pingers-push-relay pins <file>` prints one pin per configured key, earliest activation
//! first. To rotate, set B's `activated_at` to now and A's `retired_at` to at least 30 days
//! later. Once no supported app build needs A, replace A with a fresh key (`pingers-push-relay
//! keygen`), a far-future `activated_at` and `retired_at = 0`, and ship the new pin set.
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
//!    Where the configuration is itself a sealed secret rendered into memory (an appliance),
//!    paste the key file's JSON into `service_account_json` instead and keep no file.
//!
//! The relay reads `client_email`, `private_key`, `token_uri` and `private_key_id` from the key
//! at start and never writes, logs or reports any of them; a malformed key stops the relay
//! with an error naming the field, not its value. To rotate the key, add a new key to the
//! account, replace the file (or the JSON), restart the relay, then delete the old key in the
//! console.

use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use pingers_crypto::{KeyedHash, KeyedHashKey, KeyedHashPurpose, PushRelayPrivateKey, keyed_hash};
use pingers_proto::{PushRelayKeyId, TimeToLive};
use pingers_time::UnixSeconds;
use reqwest::Url;
use serde::Deserialize;
use zeroize::{Zeroize as _, Zeroizing};

use crate::credentials::{MIN_CREDENTIAL_LENGTH, ServerEntry, ServerRegistry};
use crate::delivery::EndpointPolicy;
use crate::error::ConfigError;
use crate::fcm_auth::ServiceAccount;
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

/// A service account key file's JSON, held in zeroized memory. No `Clone`; `Debug` prints
/// only the type name.
pub struct ServiceAccountJson(Zeroizing<String>);

impl ServiceAccountJson {
    /// The JSON text, to parse into a [`ServiceAccount`].
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ServiceAccountJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ServiceAccountJson(redacted)")
    }
}

/// Where the FCM service account key comes from.
#[derive(Debug)]
#[non_exhaustive]
pub enum ServiceAccountSource {
    /// `service_account_file`: the JSON key file, read at start.
    File(PathBuf),
    /// `service_account_json`: the key file's text, from the configuration itself.
    Inline(ServiceAccountJson),
}

/// FCM HTTP v1 settings from the file.
#[derive(Debug)]
pub struct FcmConfig {
    /// The Firebase project.
    pub project_id: String,
    /// Where to send; normally `https://fcm.googleapis.com/`.
    pub endpoint: Url,
    /// The service account's key.
    pub service_account: ServiceAccountSource,
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
    /// Connections held at once, and how long one may stay idle.
    pub connections: crate::connections::ConnectionLimits,
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
const fn default_max_connections() -> u32 {
    crate::connections::DEFAULT_MAX_CONNECTIONS
}
const fn default_idle_timeout() -> u64 {
    crate::connections::DEFAULT_IDLE_TIMEOUT_S
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
    #[serde(default = "default_max_connections")]
    max_connections: u32,
    #[serde(default = "default_idle_timeout")]
    idle_timeout_s: u64,
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
    credential_hash: Option<String>,
    credential: Option<Zeroizing<String>>,
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
    project_id: Option<String>,
    service_account_file: Option<PathBuf>,
    service_account_json: Option<Zeroizing<String>>,
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
        let connections = crate::connections::ConnectionLimits {
            max_connections: match file.max_connections {
                0 => {
                    return Err(ConfigError::OutOfRange {
                        field: "max_connections",
                    });
                }
                count => usize::try_from(count).unwrap_or(usize::MAX),
            },
            idle_timeout: positive_seconds("idle_timeout_s", file.idle_timeout_s)?,
            // Reading a 4 KiB body and one wait for the push service, with room to spare.
            request_timeout: upstream_timeout.saturating_add(Duration::from_secs(10)),
        };
        let fcm = file.fcm.map(fcm_config).transpose()?;
        let hash_key = hex_32("hash_key", &file.hash_key);
        file.hash_key.zeroize();
        let hash_key = hash_key?;
        let token_hash_key = KeyedHashKey::from_bytes(Zeroizing::new(*hash_key));
        let credential_key = KeyedHashKey::from_bytes(hash_key);
        let servers = file
            .servers
            .into_iter()
            .map(|server| server_entry(&credential_key, server))
            .collect::<Result<_, _>>()?;
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
            servers: ServerRegistry::new(credential_key, servers)?,
            token_hash_key,
            unifiedpush: file.unifiedpush.map(|unifiedpush| {
                if unifiedpush.allow_insecure_local_endpoints {
                    EndpointPolicy::AllowInsecureLocal
                } else {
                    EndpointPolicy::PublicHttpsOnly
                }
            }),
            fcm,
            connections,
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

pub(crate) fn hex_32(field: &'static str, text: &str) -> Result<Zeroizing<[u8; 32]>, ConfigError> {
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
            // 0: not scheduled. No key can retire at the epoch, since none activates before it.
            retired_at: key.retired_at.filter(|at| *at != 0).map(UnixSeconds::new),
        },
    )
    .map_err(|_error| ConfigError::InvalidKey { key_id: key.key_id })
}

/// The keyed hash a server's entry stores for `credential` (`credential-hash` prints it).
///
/// # Errors
///
/// [`ConfigError::CredentialTooShort`] for a credential that could never authenticate, or
/// [`ConfigError::CredentialHash`] if hashing fails (it cannot with a 32-byte key).
pub fn hash_credential(
    hash_key: &KeyedHashKey,
    credential: &[u8],
) -> Result<KeyedHash, ConfigError> {
    if credential.len() < MIN_CREDENTIAL_LENGTH {
        return Err(ConfigError::CredentialTooShort);
    }
    keyed_hash(hash_key, KeyedHashPurpose::ServerCredential, credential)
        .map_err(|_error| ConfigError::CredentialHash)
}

fn server_entry(hash_key: &KeyedHashKey, server: FileServer) -> Result<ServerEntry, ConfigError> {
    let credential_hash = match (server.credential_hash, server.credential) {
        (Some(hash), None) => KeyedHash::from_bytes(*hex_32("servers.credential_hash", &hash)?),
        // The plain text is wiped when `credential` drops at the end of this arm.
        (None, Some(credential)) => hash_credential(hash_key, credential.as_bytes())?,
        (Some(_), Some(_)) | (None, None) => {
            return Err(ConfigError::CredentialSource {
                server_id: server.server_id,
            });
        }
    };
    Ok(ServerEntry {
        credential_hash,
        limit: rate("servers.pushes_per_minute", server.pushes_per_minute)?,
        server_id: server.server_id,
    })
}

fn fcm_config(fcm: FileFcm) -> Result<FcmConfig, ConfigError> {
    let endpoint = Url::parse(&fcm.endpoint).map_err(|_| ConfigError::BadFcmEndpoint)?;
    if !matches!(endpoint.scheme(), "https" | "http") {
        return Err(ConfigError::BadFcmEndpoint);
    }
    let (service_account, named) = match (fcm.service_account_file, fcm.service_account_json) {
        (Some(path), None) => (ServiceAccountSource::File(path), None),
        (None, Some(json)) => {
            // Checked now, so a bad paste stops the relay before it serves anything.
            let account = ServiceAccount::from_json(&json).map_err(ConfigError::ServiceAccount)?;
            let named = account.project_id().map(str::to_owned);
            (
                ServiceAccountSource::Inline(ServiceAccountJson(json)),
                named,
            )
        }
        (Some(_), Some(_)) | (None, None) => return Err(ConfigError::ServiceAccountSource),
    };
    // Given, it wins; left out, an inline key file names its own project.
    let project_id = fcm.project_id.or(named).ok_or(ConfigError::NoFcmProject)?;
    if !crate::delivery::is_valid_fcm_project_id(&project_id) {
        return Err(ConfigError::OutOfRange {
            field: "fcm.project_id",
        });
    }
    Ok(FcmConfig {
        project_id,
        endpoint,
        service_account,
    })
}

#[cfg(test)]
mod tests;
