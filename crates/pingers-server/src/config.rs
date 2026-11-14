// SPDX-License-Identifier: AGPL-3.0-only
//! The server's configuration file, read once at start.
//!
//! ```toml
//! listen = "0.0.0.0:8080"          # the public API (identified and public routes); it does
//!                                  # not serve POST /ohttp, which it answers with 404
//! server_id = "https://pingers.example"   # this server's origin: RLN proofs and account
//!                                  # registrations are bound to it
//! server_operator = "Example Ltd"  # optional, shown in /v1/params (default: server_id)
//! registration_codes = ["<code>"]  # optional: without it anyone may register an account;
//!                                  # a device already registered needs none
//! signups_per_hour = 60            # new accounts the server takes per hour
//! ohttp_relay_urls = ["https://relay.example/ohttp"]   # advertised in /v1/params
//! map_host = "https://maps.example"                   # optional, advertised in /v1/params
//! delivery_db = "/var/lib/pingers/groups.sqlite"       # optional: without it, in memory
//! quota_db = "/var/lib/pingers/quota.sqlite"           # optional: accounts, bearer digests,
//!                                  # the membership tree, issuer keys (without it, in memory)
//! push_relay_ca = "/etc/pingers/push-relay-ca.pem"     # optional: extra trust roots for
//!                                  # the push relays' TLS certificates (still https only)
//! logging = "off"                  # or "counts": request totals every counts_interval_s
//! counts_interval_s = 3600
//! request_timeout_s = 30           # how long a client may take to send a request body
//! idle_timeout_s = 120             # how long a connection may stay open with no request in
//!                                  # progress (both listeners)
//! max_connections = 4096           # public connections held at once; more are closed
//! mailbox_db = "/var/lib/pingers/mailboxes.sqlite"   # optional: without it, in memory
//! mailbox_posts_per_bucket = 60
//! push_timeout_s = 10              # how long a push relay has to answer
//! expire_interval_s = 60           # how often expired board segments and mailboxes go
//!
//! [ohttp]
//! listen = "127.0.0.1:9471"        # the gateway's own listener: POST /ohttp and
//!                                  # GET /v1/ohttp/config only
//! relays = ["10.0.0.5", "fd00::5"] # the only addresses POST /ohttp is accepted from
//! trusted_proxies = ["127.0.0.1"]  # optional: proxies whose X-Forwarded-For is believed
//!                                  # on the gateway listener (default: none, header ignored)
//! served_key_id = 1                # optional: the key served at GET /v1/ohttp/config
//!                                  # (default: the first [[ohttp.keys]] entry)
//! max_connections = 256            # gateway connections held at once
//!
//! [[ohttp.keys]]                   # every key opens requests; the one not served is the
//! key_id = 1                       # other rotation slot (1 to 255, distinct)
//! secret = "<64 hex>"              # `pingers-server keygen ohttp` prints a new one
//!
//! [[ohttp.keys]]
//! key_id = 2
//! secret = "<64 hex>"
//!
//! [metrics]
//! allow = ["127.0.0.1"]            # who may GET /metrics; without it there is no such route
//!
//! [[push_relays]]                  # the relays mailbox wakes are forwarded to
//! relay_id = 1                     # what devices name in RelayRef::Listed
//! url = "https://push.example"     # where this server sends wakes: https://, or http:// to
//!                                  # a loopback address literal only, such as
//!                                  # "http://127.0.0.1:9472" for a relay on this machine
//! public_url = "https://push.example"   # where devices reach the relay, as /v1/params lists
//!                                  # it: https:// only (the same as url unless url is local)
//! credential = "<the bearer credential this relay issued to this server>"
//! key_config_hash = "<pin>"        # optional, with operator: listed in /v1/params. The relay
//!                                  # key's pin as `pingers-push-relay pins` prints it: 43
//!                                  # base64url characters (KeyConfigHash::to_pin)
//! operator = "Push Relay Ltd"
//!
//! [app_links]                      # optional: the Android apps that may open this server's
//!                                  # invite links (https://<server>/j) directly, served at
//!                                  # /.well-known/assetlinks.json (404 without this table)
//! packages = ["org.pingers.app", "org.pingers.app.foss"]
//! certificates = ["AA:44:E4:54:...:18:16"]   # SHA-256 of the signing certificates
//! ```
//!
//! Every field above `[ohttp]` other than `listen` and `server_id` is optional; so are those
//! marked optional below it. Unknown fields are refused.
//!
//! **The relay-only rule** (design B.12). Only the OHTTP relay may reach `POST /ohttp`, or the
//! gateway would see clients' addresses. Two checks enforce it:
//!
//! 1. The gateway has its own listener (`[ohttp] listen`). Bind it where only the relay can
//!    connect (loopback when the relay runs on the same machine, a private network or tunnel
//!    otherwise) and never expose it through the public reverse proxy. A reverse proxy in
//!    front of `listen` then cannot reach the gateway at all, whatever it forwards.
//! 2. `[ohttp] relays` is an address allow-list of connections to the gateway listener. If a
//!    reverse proxy must stand in front of the gateway too (a relay on another host), list it
//!    in `trusted_proxies`: the client address is then read from the `X-Forwarded-For` it
//!    writes (the rightmost entry that is not a trusted proxy), and `relays` names the relay's
//!    own addresses. With no trusted proxies the header is ignored.
//!
//! **Key rotation** (two slots). Configure two keys and serve one. Pin both key config hashes in
//! the app (`pingers-server pins <config.toml>` prints them). To rotate, change
//! `served_key_id` to the other slot; requests sealed to either key still open. Once no
//! supported app build still uses the old key, replace its secret, and ship the new pin set
//! with the next app release.
//!
//! The file holds the gateway's private keys: keep it readable by the server only.

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use pingers_mailbox::{ListedRelays, PushRelayUrl, RelayCredential, RelayTarget};
use pingers_ohttp::{GatewayKey, KeyConfig, OhttpError};
use pingers_proto::{
    KeyConfigHash, MapHostUrl, PushRelayInfo, RelayId, RelayUrl, SecretText, ServerId,
};
use pingers_quota::SignupsPerHour;
use serde::Deserialize;
use zeroize::{Zeroize as _, Zeroizing};

use crate::forwarded::TrustedProxies;

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
    /// Two push relays share an id.
    #[error("configuration: push relay id {relay_id} is used twice")]
    DuplicateRelayId {
        /// The repeated id.
        relay_id: u16,
    },
    /// A push relay URL is neither `https://` nor `http://` to a loopback address literal.
    #[error("configuration: push_relays.url must be https://, or http:// to a loopback address")]
    RelayUrl,
    /// A push relay's `public_url`, where devices reach it, is not `https://`.
    #[error("configuration: push_relays.public_url must start with https://")]
    PublicRelayUrl,
    /// `served_key_id` names no configured key.
    #[error("configuration: ohttp.served_key_id {key_id} is not an [[ohttp.keys]] key_id")]
    UnknownServedKey {
        /// The id asked for.
        key_id: u8,
    },
    /// `server_id` is not an `https://` origin.
    #[error("configuration: server_id must be an https:// origin")]
    ServerId,
    /// An `ohttp_relay_urls` entry is not `https://`.
    #[error("configuration: ohttp_relay_urls must start with https://")]
    OhttpRelayUrl,
    /// `map_host` was not an `https://` URL.
    #[error("configuration: map_host must start with https://")]
    MapHostUrl,
    /// A push relay's `key_config_hash` is not a pin (43 base64url characters, as
    /// `pingers-push-relay pins` prints), or only one of `key_config_hash` and `operator` is
    /// given.
    #[error(
        "configuration: push_relays.key_config_hash must be a 43-character base64url pin, \
         given with operator"
    )]
    RelayInfo,
    /// An `[app_links]` package id is not a Java package name, a certificate is not a SHA-256
    /// fingerprint (32 hex pairs separated by colons), or a list is empty.
    #[error(
        "configuration: app_links needs package ids like org.pingers.app and SHA-256 \
         fingerprints like AA:44:...:16"
    )]
    AppLinks,
    /// A duration or count is zero.
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

/// Which configured key `GET /v1/ohttp/config` serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServedKey {
    /// The first key listed.
    First,
    /// The key with this id (`[ohttp] served_key_id`).
    Id(u8),
}

/// The gateway's keys: at least one, with distinct ids, one of them served.
pub struct OhttpKeys {
    keys: Vec<GatewayKey>,
    served: KeyConfig,
}

impl OhttpKeys {
    /// Checks the keys, serving the first at `GET /v1/ohttp/config`. Every key opens requests,
    /// so the others still work during a rotation.
    ///
    /// # Errors
    ///
    /// As [`OhttpKeys::serving`].
    pub fn new(keys: Vec<GatewayKey>) -> Result<Self, ConfigError> {
        Self::serving(keys, ServedKey::First)
    }

    /// Checks the keys, serving the one `served` names.
    ///
    /// # Errors
    ///
    /// [`ConfigError::NoOhttpKey`] if `keys` is empty, [`ConfigError::DuplicateKeyId`] if two
    /// share an id, [`ConfigError::UnknownServedKey`] if no key has the id `served` names.
    pub fn serving(keys: Vec<GatewayKey>, served: ServedKey) -> Result<Self, ConfigError> {
        let mut seen = Vec::with_capacity(keys.len());
        for key in &keys {
            let key_id = key.config().key_id();
            if seen.contains(&key_id) {
                return Err(ConfigError::DuplicateKeyId { key_id });
            }
            seen.push(key_id);
        }
        let served = match served {
            ServedKey::First => keys.first().ok_or(ConfigError::NoOhttpKey)?.config(),
            ServedKey::Id(key_id) => keys
                .iter()
                .map(GatewayKey::config)
                .find(|config| config.key_id() == key_id)
                .ok_or(if keys.is_empty() {
                    ConfigError::NoOhttpKey
                } else {
                    ConfigError::UnknownServedKey { key_id }
                })?,
        };
        Ok(Self { keys, served })
    }

    /// The key config clients fetch and pin.
    #[must_use]
    pub fn served(&self) -> &KeyConfig {
        &self.served
    }

    /// Every key's public config, in the order configured: what the app pins, so that
    /// switching the served key never shows it an unpinned one.
    #[must_use]
    pub fn configs(&self) -> Vec<KeyConfig> {
        self.keys.iter().map(GatewayKey::config).collect()
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
    /// This server's origin. RLN external nullifiers and registration proofs commit to it.
    pub server_id: ServerId,
    /// Who runs the server, as published in `/v1/params`.
    pub server_operator: String,
    /// Codes that allow a new account at `POST /v1/accounts`; empty means anyone may register
    /// (design B.1). A device already registered needs none.
    pub registration_codes: Vec<SecretText>,
    /// How many new accounts the server takes per hour.
    pub signups_per_hour: SignupsPerHour,
    /// The OHTTP relays advertised in `/v1/params`.
    pub ohttp_relay_urls: Vec<RelayUrl>,
    /// The push relays advertised in `/v1/params` (those configured with a key hash and an
    /// operator).
    pub advertised_push_relays: Vec<PushRelayInfo>,
    /// The map host advertised in `/v1/params`, if this server names one.
    pub map_host: Option<MapHostUrl>,
    /// Extra trust roots for the push relays' TLS certificates, besides the system's.
    pub push_relay_ca: Option<CertificateSource>,
    /// Where group logs, subscriptions and snapshots are kept.
    pub delivery_store: Database,
    /// Where the membership tree, issuer keys and spent tokens are kept.
    pub quota_store: Database,
    /// Whether anything is written.
    pub logging: Logging,
    /// How long a client may take to send a whole request body (one-shot requests, the head
    /// of a live channel, direct requests). Live channel bodies are not limited.
    pub request_timeout: Duration,
    /// How long a connection may stay open with no request in progress, on either listener:
    /// an idle connection, or one that never finishes sending a request head, is closed.
    pub idle_timeout: Duration,
    /// How many connections the public listener holds at once; more are closed on arrival.
    pub max_connections: NonZeroU32,
    /// How many connections the gateway listener holds at once.
    pub gateway_max_connections: NonZeroU32,
    /// The gateway's keys.
    pub ohttp_keys: OhttpKeys,
    /// Where the binary binds the gateway's own listener, which serves only `POST /ohttp` and
    /// `GET /v1/ohttp/config`.
    pub gateway_listen: SocketAddr,
    /// Who may `POST /ohttp`.
    pub relays: RelayAllowList,
    /// The proxies whose `X-Forwarded-For` is believed on the gateway listener.
    pub trusted_proxies: TrustedProxies,
    /// Who may `GET /metrics`; empty means the route does not exist.
    pub metrics: AddressList,
    /// Where mailboxes are kept.
    pub mailbox_store: Database,
    /// Posts one mailbox accepts per bucket (pingers-mailbox's `PostsPerBucket`).
    pub mailbox_posts_per_bucket: NonZeroU32,
    /// The push relays mailbox wakes are forwarded to.
    pub push_relays: ListedRelays,
    /// How long a push relay has to answer, on the injected timer.
    pub push_timeout: Duration,
    /// How often expired board segments and mailboxes are swept.
    pub expire_interval: Duration,
    /// The Android apps that may open this server's invite links directly (App Links), served
    /// at `/.well-known/assetlinks.json`; `None`: that path is 404.
    pub app_links: Option<AppLinks>,
}

/// The apps Android verifies for this server's `https://<server>/j` links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppLinks {
    /// Their package ids (Play and F-Droid builds; `.debug` for local builds).
    pub packages: Vec<AndroidPackage>,
    /// The SHA-256 fingerprints of the certificates they are signed with.
    pub certificates: Vec<CertificateFingerprint>,
}

/// An Android package id, such as `org.pingers.app`: two or more dot-separated parts, each a
/// letter then letters, digits or underscores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidPackage(String);

impl AndroidPackage {
    /// Checks the form.
    ///
    /// # Errors
    ///
    /// [`ConfigError::AppLinks`].
    pub fn new(id: String) -> Result<Self, ConfigError> {
        let part = |part: &str| {
            part.chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic())
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        };
        if id.split('.').count() >= 2 && id.split('.').all(part) {
            Ok(Self(id))
        } else {
            Err(ConfigError::AppLinks)
        }
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A signing certificate's SHA-256 fingerprint as `keytool` prints it: 32 upper-case hex pairs
/// separated by colons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateFingerprint(String);

impl CertificateFingerprint {
    /// Checks the form; lower-case hex is accepted and kept upper-case.
    ///
    /// # Errors
    ///
    /// [`ConfigError::AppLinks`].
    pub fn new(fingerprint: &str) -> Result<Self, ConfigError> {
        let pairs: Vec<&str> = fingerprint.split(':').collect();
        if pairs.len() == 32
            && pairs
                .iter()
                .all(|pair| pair.len() == 2 && pair.chars().all(|digit| digit.is_ascii_hexdigit()))
        {
            Ok(Self(fingerprint.to_ascii_uppercase()))
        } else {
            Err(ConfigError::AppLinks)
        }
    }

    /// The fingerprint.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where one of the server's stores is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Database {
    /// In memory: lost on restart.
    Memory,
    /// An SQLite file.
    File(PathBuf),
}

/// PEM certificates to trust as roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertificateSource {
    /// A PEM file, read when the server starts.
    File(PathBuf),
    /// PEM text (the in-process harness's test CA).
    Pem(Vec<u8>),
}

/// What every configuration must name.
#[derive(Debug)]
pub struct Required {
    /// Where the binary listens.
    pub listen: SocketAddr,
    /// This server's origin.
    pub server_id: ServerId,
    /// The gateway's keys.
    pub ohttp_keys: OhttpKeys,
    /// Where the binary binds the gateway's listener.
    pub gateway_listen: SocketAddr,
    /// Who may `POST /ohttp`.
    pub relays: RelayAllowList,
}

impl ServerConfig {
    /// A configuration with the required fields, logging off, no metrics, no trusted proxies,
    /// every store in memory, open registration, no push relays, and the file defaults for
    /// everything else.
    #[must_use]
    pub fn new(required: Required) -> Self {
        let Required {
            listen,
            server_id,
            ohttp_keys,
            gateway_listen,
            relays,
        } = required;
        Self {
            listen,
            server_operator: server_id.as_str().to_owned(),
            server_id,
            registration_codes: Vec::new(),
            signups_per_hour: DEFAULT_SIGNUPS_PER_HOUR,
            ohttp_relay_urls: Vec::new(),
            advertised_push_relays: Vec::new(),
            map_host: None,
            push_relay_ca: None,
            delivery_store: Database::Memory,
            quota_store: Database::Memory,
            logging: Logging::Off,
            request_timeout: Duration::from_secs(default_request_timeout()),
            idle_timeout: Duration::from_secs(default_idle_timeout()),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            gateway_max_connections: DEFAULT_GATEWAY_MAX_CONNECTIONS,
            ohttp_keys,
            gateway_listen,
            relays,
            trusted_proxies: TrustedProxies::default(),
            metrics: AddressList::default(),
            mailbox_store: Database::Memory,
            mailbox_posts_per_bucket: DEFAULT_POSTS_PER_BUCKET,
            push_relays: ListedRelays::default(),
            push_timeout: Duration::from_secs(default_push_timeout()),
            expire_interval: Duration::from_secs(default_expire_interval()),
            app_links: None,
        }
    }
}

/// Public connections unless configured: thousands of phones, each holding one HTTP/2
/// connection for its identified requests (anonymous ones go through the relay).
const DEFAULT_MAX_CONNECTIONS: NonZeroU32 = match NonZeroU32::new(4096) {
    Some(count) => count,
    None => NonZeroU32::MIN,
};

/// Gateway connections unless configured: only the OHTTP relays connect there, each with a
/// few pooled connections.
const DEFAULT_GATEWAY_MAX_CONNECTIONS: NonZeroU32 = match NonZeroU32::new(256) {
    Some(count) => count,
    None => NonZeroU32::MIN,
};

/// New accounts per hour unless configured: far above a household's or a club's sign-ups,
/// far below what would fill the membership tree.
const DEFAULT_SIGNUPS_PER_HOUR: SignupsPerHour = match NonZeroU32::new(60) {
    Some(count) => SignupsPerHour::new(count),
    None => SignupsPerHour::new(NonZeroU32::MIN),
};

/// Posts per mailbox per bucket unless configured: well above the few knocks a lease draws.
const DEFAULT_POSTS_PER_BUCKET: NonZeroU32 = match NonZeroU32::new(60) {
    Some(count) => count,
    None => NonZeroU32::MIN,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    listen: SocketAddr,
    server_id: String,
    server_operator: Option<String>,
    #[serde(default)]
    registration_codes: Vec<String>,
    signups_per_hour: Option<u32>,
    #[serde(default)]
    ohttp_relay_urls: Vec<String>,
    map_host: Option<String>,
    delivery_db: Option<PathBuf>,
    quota_db: Option<PathBuf>,
    push_relay_ca: Option<PathBuf>,
    #[serde(default)]
    logging: FileLogging,
    #[serde(default = "default_counts_interval")]
    counts_interval_s: u64,
    #[serde(default = "default_request_timeout")]
    request_timeout_s: u64,
    #[serde(default = "default_idle_timeout")]
    idle_timeout_s: u64,
    max_connections: Option<u32>,
    ohttp: FileOhttp,
    #[serde(default)]
    metrics: FileMetrics,
    mailbox_db: Option<PathBuf>,
    mailbox_posts_per_bucket: Option<u32>,
    #[serde(default = "default_push_timeout")]
    push_timeout_s: u64,
    #[serde(default = "default_expire_interval")]
    expire_interval_s: u64,
    #[serde(default)]
    push_relays: Vec<FilePushRelay>,
    app_links: Option<FileAppLinks>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAppLinks {
    packages: Vec<String>,
    certificates: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePushRelay {
    relay_id: u16,
    url: String,
    credential: String,
    key_config_hash: Option<String>,
    operator: Option<String>,
    public_url: String,
}

const fn default_push_timeout() -> u64 {
    10
}

const fn default_expire_interval() -> u64 {
    60
}

/// The configured relays, and those of them to advertise.
struct PushRelays {
    listed: ListedRelays,
    advertised: Vec<PushRelayInfo>,
}

fn push_relays(relays: Vec<FilePushRelay>) -> Result<PushRelays, ConfigError> {
    let mut seen = Vec::with_capacity(relays.len());
    let mut listed = Vec::with_capacity(relays.len());
    let mut advertised = Vec::new();
    for relay in relays {
        if seen.contains(&relay.relay_id) {
            return Err(ConfigError::DuplicateRelayId {
                relay_id: relay.relay_id,
            });
        }
        seen.push(relay.relay_id);
        let url = PushRelayUrl::new(relay.url).map_err(|_| ConfigError::RelayUrl)?;
        let public_url =
            RelayUrl::new(relay.public_url).map_err(|_| ConfigError::PublicRelayUrl)?;
        match (relay.key_config_hash, relay.operator) {
            (None, None) => {}
            (Some(hash), Some(operator)) => advertised.push(PushRelayInfo {
                relay_id: RelayId::new(relay.relay_id),
                url: public_url,
                key_config_hash: KeyConfigHash::from_pin(&hash)
                    .map_err(|_| ConfigError::RelayInfo)?,
                operator,
            }),
            (Some(_), None) | (None, Some(_)) => return Err(ConfigError::RelayInfo),
        }
        listed.push((
            RelayId::new(relay.relay_id),
            RelayTarget {
                url,
                credential: RelayCredential::new(relay.credential),
            },
        ));
    }
    Ok(PushRelays {
        listed: ListedRelays::new(listed),
        advertised,
    })
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
    listen: SocketAddr,
    relays: Vec<IpAddr>,
    #[serde(default)]
    trusted_proxies: Vec<IpAddr>,
    served_key_id: Option<u8>,
    max_connections: Option<u32>,
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

const fn default_idle_timeout() -> u64 {
    120
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
        let server_id = ServerId::new(file.server_id).map_err(|_| ConfigError::ServerId)?;
        let push = push_relays(file.push_relays)?;
        Ok(Self {
            listen: file.listen,
            server_operator: file
                .server_operator
                .unwrap_or_else(|| server_id.as_str().to_owned()),
            server_id,
            registration_codes: file
                .registration_codes
                .into_iter()
                .map(SecretText::new)
                .collect(),
            signups_per_hour: match file.signups_per_hour {
                None => DEFAULT_SIGNUPS_PER_HOUR,
                Some(count) => {
                    SignupsPerHour::new(NonZeroU32::new(count).ok_or(ConfigError::OutOfRange {
                        field: "signups_per_hour",
                    })?)
                }
            },
            ohttp_relay_urls: file
                .ohttp_relay_urls
                .into_iter()
                .map(|url| RelayUrl::new(url).map_err(|_| ConfigError::OhttpRelayUrl))
                .collect::<Result<_, _>>()?,
            advertised_push_relays: push.advertised,
            map_host: file
                .map_host
                .map(|url| MapHostUrl::new(url).map_err(|_| ConfigError::MapHostUrl))
                .transpose()?,
            push_relay_ca: file.push_relay_ca.map(CertificateSource::File),
            delivery_store: file.delivery_db.map_or(Database::Memory, Database::File),
            quota_store: file.quota_db.map_or(Database::Memory, Database::File),
            logging,
            request_timeout: positive_seconds("request_timeout_s", file.request_timeout_s)?,
            idle_timeout: positive_seconds("idle_timeout_s", file.idle_timeout_s)?,
            max_connections: count_or(
                "max_connections",
                file.max_connections,
                DEFAULT_MAX_CONNECTIONS,
            )?,
            gateway_max_connections: count_or(
                "ohttp.max_connections",
                file.ohttp.max_connections,
                DEFAULT_GATEWAY_MAX_CONNECTIONS,
            )?,
            ohttp_keys: OhttpKeys::serving(
                keys,
                file.ohttp
                    .served_key_id
                    .map_or(ServedKey::First, ServedKey::Id),
            )?,
            gateway_listen: file.ohttp.listen,
            relays: RelayAllowList::new(file.ohttp.relays)?,
            trusted_proxies: TrustedProxies::new(file.ohttp.trusted_proxies),
            metrics: AddressList::new(file.metrics.allow),
            mailbox_store: file.mailbox_db.map_or(Database::Memory, Database::File),
            mailbox_posts_per_bucket: match file.mailbox_posts_per_bucket {
                None => DEFAULT_POSTS_PER_BUCKET,
                Some(count) => NonZeroU32::new(count).ok_or(ConfigError::OutOfRange {
                    field: "mailbox_posts_per_bucket",
                })?,
            },
            push_relays: push.listed,
            push_timeout: positive_seconds("push_timeout_s", file.push_timeout_s)?,
            expire_interval: positive_seconds("expire_interval_s", file.expire_interval_s)?,
            app_links: file.app_links.map(app_links).transpose()?,
        })
    }
}

fn app_links(file: FileAppLinks) -> Result<AppLinks, ConfigError> {
    if file.packages.is_empty() || file.certificates.is_empty() {
        return Err(ConfigError::AppLinks);
    }
    Ok(AppLinks {
        packages: file
            .packages
            .into_iter()
            .map(AndroidPackage::new)
            .collect::<Result<_, _>>()?,
        certificates: file
            .certificates
            .iter()
            .map(|certificate| CertificateFingerprint::new(certificate))
            .collect::<Result<_, _>>()?,
    })
}

fn line_of(text: &str, offset: usize) -> usize {
    text.bytes()
        .take(offset)
        .filter(|byte| *byte == b'\n')
        .count()
        .saturating_add(1)
}

/// A configured count, or `default` when absent; zero is refused.
fn count_or(
    field: &'static str,
    configured: Option<u32>,
    default: NonZeroU32,
) -> Result<NonZeroU32, ConfigError> {
    match configured {
        None => Ok(default),
        Some(count) => NonZeroU32::new(count).ok_or(ConfigError::OutOfRange { field }),
    }
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
