// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error types. None of them carries a key, a credential or a push token.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

pub use crate::fcm_auth::{ServiceAccountError, TokenError};

/// Why the relay could not start or keep serving.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RelayError {
    /// A file named by the command line or the configuration could not be read.
    #[error("cannot read {path}")]
    ReadFile {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// The configuration is not valid.
    #[error("invalid configuration")]
    Config(#[source] ConfigError),
    /// The FCM service-account file is not a usable service-account key.
    #[error("invalid FCM service account file {path}")]
    ServiceAccount {
        /// The file.
        path: PathBuf,
        /// What is wrong, by field name only.
        #[source]
        source: ServiceAccountError,
    },
    /// The command line is not one of the forms in [`crate::command::USAGE`].
    #[error("{usage}", usage = crate::command::USAGE)]
    Usage,
    /// The operating system's random number generator failed (`keygen`).
    #[error("cannot read the operating system's random number generator")]
    Entropy,
    /// Standard input could not be read (`credential-hash`).
    #[error("cannot read standard input")]
    ReadInput(#[source] io::Error),
    /// Standard input is not the hash key and the credential, one per line
    /// (`credential-hash`).
    #[error("standard input must hold the hash key (64 hex) and the credential, one per line")]
    CredentialInput,
    /// Standard output could not be written.
    #[error("cannot write standard output")]
    WriteOutput(#[source] io::Error),
    /// No key is published at the current time: every key is retired or not yet active.
    #[error("no relay key is active now; check activated_at and retired_at")]
    NoActiveKey,
    /// The HTTP client for push services could not be built.
    #[error("cannot build the HTTP client")]
    HttpClient(#[source] reqwest::Error),
    /// The listening socket could not be opened.
    #[error("cannot listen on {address}")]
    Listen {
        /// The address from the configuration.
        address: SocketAddr,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// The HTTP server stopped with an error.
    #[error("the HTTP server failed")]
    Serve(#[source] io::Error),
}

/// What is wrong with the configuration. Messages name fields and positions, never values,
/// because the file holds private keys and server credentials.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file is not valid TOML or does not match the schema. The parser's own message is
    /// not kept: it quotes the offending line, which may be a key or a credential.
    #[error("syntax or schema error at line {line}")]
    Syntax {
        /// One-based line of the error, or 0 if the parser gave no position.
        line: usize,
    },
    /// A field holds a value outside its allowed range.
    #[error("{field} is out of range")]
    OutOfRange {
        /// The field.
        field: &'static str,
    },
    /// `idle_timeout_s` is not over the 20 seconds pingers-server keeps an idle connection to
    /// a push relay ([`pingers_proto::SERVER_PUSH_POOL_IDLE`]): the relay would close
    /// connections the server is about to send a push on.
    #[error(
        "{field} must be over {minimum_s} seconds, the time a server keeps an idle connection to the relay"
    )]
    IdleTimeoutTooShort {
        /// The field.
        field: &'static str,
        /// The value it must exceed, in seconds.
        minimum_s: u64,
    },
    /// A hex field has the wrong length or non-hex characters.
    #[error("{field} must be {bytes} bytes of hex")]
    BadHex {
        /// The field.
        field: &'static str,
        /// The required length in bytes.
        bytes: usize,
    },
    /// A private key from which no public key can be derived.
    #[error("key {key_id} is not a valid X25519 private key")]
    InvalidKey {
        /// The key.
        key_id: u8,
    },
    /// No `[[keys]]` entry.
    #[error("at least one [[keys]] entry is required")]
    NoKeys,
    /// Two keys share a key id.
    #[error("key id {key_id} appears twice")]
    DuplicateKeyId {
        /// The repeated id.
        key_id: u8,
    },
    /// Two keys activate at the same second, so which one is published would be ambiguous.
    #[error("two keys activate at the same time")]
    DuplicateActivation,
    /// A key retires before it activates.
    #[error("key {key_id} retires before it activates")]
    RetiredBeforeActivated {
        /// The key.
        key_id: u8,
    },
    /// A key retires less than the overlap window after the key that replaces it activates,
    /// so tokens sealed to it shortly before the rotation would stop working early.
    #[error("key {key_id} retires before the overlap window after its successor has passed")]
    OverlapTooShort {
        /// The retiring key.
        key_id: u8,
    },
    /// No `[[servers]]` entry: nobody could use the relay.
    #[error("at least one [[servers]] entry is required")]
    NoServers,
    /// Two servers share an id.
    #[error("server id {server_id} appears twice")]
    DuplicateServerId {
        /// The repeated id (an operator-chosen label, not a secret).
        server_id: String,
    },
    /// Two servers share a credential hash.
    #[error("two servers share a credential")]
    DuplicateCredential,
    /// A `[[servers]]` entry has both or neither of `credential` and `credential_hash`.
    #[error("server {server_id}: give exactly one of credential and credential_hash")]
    CredentialSource {
        /// The entry's id (an operator-chosen label, not a secret).
        server_id: String,
    },
    /// A plain credential is shorter than
    /// [`MIN_CREDENTIAL_LENGTH`](crate::credentials::MIN_CREDENTIAL_LENGTH), so it could never
    /// authenticate.
    #[error("a server credential is shorter than 32 characters")]
    CredentialTooShort,
    /// A credential could not be hashed (it always can with a 32-byte key).
    #[error("cannot hash a server credential")]
    CredentialHash,
    /// `[fcm]` has no `project_id`, and its `service_account_json` names none either (a key file
    /// given as `service_account_file` needs `project_id`).
    #[error("fcm.project_id is missing, and the service account key names no project_id")]
    NoFcmProject,
    /// `[fcm]` has both or neither of `service_account_file` and `service_account_json`.
    #[error("[fcm]: give exactly one of service_account_file and service_account_json")]
    ServiceAccountSource,
    /// `fcm.service_account_json` is not a usable service account key.
    #[error("fcm.service_account_json is not a usable service account key")]
    ServiceAccount(#[source] ServiceAccountError),
    /// The FCM endpoint is not a valid URL.
    #[error("fcm.endpoint is not a valid http(s) URL")]
    BadFcmEndpoint,
    /// Neither UnifiedPush nor FCM is enabled, so no push could be sent.
    #[error("enable [unifiedpush] or [fcm]")]
    NoPlatform,
}
