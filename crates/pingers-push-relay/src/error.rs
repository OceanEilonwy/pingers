// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error types. None of them carries a key, a credential or a push token.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

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
    /// The command line is not `pingers-push-relay <config.toml>`.
    #[error("usage: pingers-push-relay <config.toml>")]
    Usage,
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
    /// The FCM endpoint is not a valid URL.
    #[error("fcm.endpoint is not a valid http(s) URL")]
    BadFcmEndpoint,
    /// Neither UnifiedPush nor FCM is enabled, so no push could be sent.
    #[error("enable [unifiedpush] or [fcm]")]
    NoPlatform,
}
