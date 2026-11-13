// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error types.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Why the map host could not start or keep serving.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MapHostError {
    /// The configuration file could not be read.
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
    /// The command line is not `pingers-map-host <config.toml>`.
    #[error("usage: pingers-map-host <config.toml>")]
    Usage,
    /// The configured directory cannot be resolved.
    #[error("cannot open the directory {path}")]
    Directory {
        /// The directory from the configuration.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// The configured directory is not a directory.
    #[error("{path} is not a directory")]
    NotADirectory {
        /// The path from the configuration.
        path: PathBuf,
    },
    /// The cache-mode region list could not be read.
    #[error("cannot read the region list {path}")]
    ReadRegionList {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// The cache-mode region list is not valid.
    #[error("invalid region list {path}")]
    RegionList {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        #[source]
        source: crate::region::RegionListError,
    },
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

/// What is wrong with the configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file is not valid TOML or does not match the schema.
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
    /// `max_disk` is not a size such as `60GB` or `500MiB`.
    #[error("max_disk must be a whole number with an optional unit, such as 60GB or 500MiB")]
    BadSize,
    /// A region id in `hosted` or `max_zoom` is not lowercase `[a-z0-9-]` segments joined by
    /// `/`.
    #[error("{id} is not a valid region id")]
    BadRegionId {
        /// The id as written.
        id: String,
    },
}
