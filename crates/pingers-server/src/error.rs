// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

use crate::config::ConfigError;

/// Why the server could not start. Never carries addresses, keys or request contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServerError {
    /// The configuration file cannot be read.
    #[error("cannot read the configuration file")]
    ReadConfig(#[source] std::io::Error),
    /// The configuration is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The operating system's random number generator cannot be read.
    #[error("cannot read the operating system's random number generator")]
    Entropy(#[source] std::io::Error),
}
