// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

use crate::config::ConfigError;

/// Why the server could not start. Never carries addresses, keys or request contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServerError {
    /// The binary's arguments are not one of its commands.
    #[error(
        "usage: pingers-server <config.toml> | pingers-server pins <config.toml> | \
         pingers-server keygen ohttp"
    )]
    Usage,
    /// The configuration file cannot be read.
    #[error("cannot read the configuration file")]
    ReadConfig(#[source] std::io::Error),
    /// The configuration is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The operating system's random number generator cannot be read.
    #[error("cannot read the operating system's random number generator")]
    Entropy(#[source] std::io::Error),
    /// The mailbox database cannot be opened.
    #[error("cannot open the mailbox store")]
    MailboxStore(#[source] pingers_mailbox::StorageError),
    /// The group database cannot be opened.
    #[error("cannot open the group store")]
    DeliveryStore(#[source] pingers_delivery::DeliveryError),
    /// The quota database cannot be opened, or its membership tree does not match its root.
    #[error("cannot open the quota store")]
    QuotaStore(#[source] pingers_quota::QuotaError),
    /// The push relay trust roots cannot be read.
    #[error("cannot read the push relay certificates")]
    ReadCertificates(#[source] std::io::Error),
    /// The push relay client cannot be built.
    #[error("cannot build the push relay client")]
    HttpClient(#[source] reqwest::Error),
    /// The async runtime cannot be started.
    #[error("cannot start the runtime")]
    Runtime(#[source] std::io::Error),
    /// The listen address cannot be bound.
    #[error("cannot bind the listen address")]
    Listen(#[source] std::io::Error),
    /// The gateway's listen address (`[ohttp] listen`) cannot be bound.
    #[error("cannot bind the gateway listen address (ohttp.listen)")]
    ListenGateway(#[source] std::io::Error),
    /// A new OHTTP gateway key cannot be made.
    #[error("cannot make an OHTTP gateway key")]
    OhttpKey(#[source] pingers_ohttp::OhttpError),
}
