// SPDX-License-Identifier: AGPL-3.0-only
//! The relay's configuration file.

use std::net::SocketAddr;
use std::time::Duration;

use hyper::Uri;
use serde::Deserialize;

/// The configuration file, TOML:
///
/// ```toml
/// listen = "0.0.0.0:8443"
/// gateway = "http://gateway.internal:8080/ohttp"
/// # Optional: where `GET <key_config_path>` is fetched from. Without it, no key config route.
/// gateway_key_config = "http://gateway.internal:8080/v1/ohttp/config"
/// # Optional, with defaults:
/// relay_path = "/ohttp"
/// key_config_path = "/v1/ohttp/config"
/// response_timeout_seconds = 30
/// ```
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    listen: SocketAddr,
    gateway: String,
    gateway_key_config: Option<String>,
    relay_path: Option<String>,
    key_config_path: Option<String>,
    response_timeout_seconds: Option<u64>,
}

/// Parsed and checked configuration.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// Where the relay accepts HTTP/2 connections.
    pub(crate) listen: SocketAddr,
    /// The one gateway every request goes to.
    pub(crate) gateway: Uri,
    /// Where key config requests go, if the relay serves them.
    pub(crate) key_config_upstream: Option<Uri>,
    /// The path clients POST encapsulated requests to.
    pub(crate) relay_path: String,
    /// The path clients GET the gateway's key config from.
    pub(crate) key_config_path: String,
    /// How long to wait for the gateway's response headers. Bodies, which for chunked
    /// exchanges last as long as a Meetup, are not limited.
    pub(crate) response_timeout: Duration,
}

/// Why a configuration was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConfigError {
    /// Not valid TOML, or a field is missing, unknown or of the wrong type.
    Syntax(String),
    /// A gateway URL is not an absolute `http` URL.
    GatewayUrl(&'static str),
    /// A path does not start with `/`.
    Path(&'static str),
    /// The two routes collide.
    SamePaths,
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Syntax(message) => write!(formatter, "configuration: {message}"),
            Self::GatewayUrl(field) => {
                write!(
                    formatter,
                    "configuration: {field} must be an absolute http:// URL"
                )
            }
            Self::Path(field) => write!(formatter, "configuration: {field} must start with /"),
            Self::SamePaths => write!(
                formatter,
                "configuration: relay_path and key_config_path must differ"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

fn gateway_url(text: &str, field: &'static str) -> Result<Uri, ConfigError> {
    let uri: Uri = text.parse().map_err(|_| ConfigError::GatewayUrl(field))?;
    // h2c (prior knowledge) to the gateway; TLS to it is terminated outside the relay.
    if uri.scheme_str() != Some("http") || uri.authority().is_none() {
        return Err(ConfigError::GatewayUrl(field));
    }
    Ok(uri)
}

fn path(value: Option<String>, default: &str, field: &'static str) -> Result<String, ConfigError> {
    let path = value.unwrap_or_else(|| default.to_owned());
    if path.starts_with('/') {
        Ok(path)
    } else {
        Err(ConfigError::Path(field))
    }
}

impl Config {
    /// Parses the configuration file's text.
    pub(crate) fn parse(text: &str) -> Result<Self, ConfigError> {
        let file: File = toml::from_str(text)
            .map_err(|error| ConfigError::Syntax(error.message().to_owned()))?;
        let config = Self {
            listen: file.listen,
            gateway: gateway_url(&file.gateway, "gateway")?,
            key_config_upstream: file
                .gateway_key_config
                .as_deref()
                .map(|url| gateway_url(url, "gateway_key_config"))
                .transpose()?,
            relay_path: path(file.relay_path, "/ohttp", "relay_path")?,
            key_config_path: path(file.key_config_path, "/v1/ohttp/config", "key_config_path")?,
            response_timeout: Duration::from_secs(file.response_timeout_seconds.unwrap_or(30)),
        };
        if config.relay_path == config.key_config_path {
            return Err(ConfigError::SamePaths);
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::{Config, ConfigError};

    #[test]
    fn minimal_file_gets_defaults() -> Result<(), ConfigError> {
        let config = Config::parse(
            "listen = \"127.0.0.1:8443\"\ngateway = \"http://gateway.internal:8080/ohttp\"\n",
        )?;
        assert_eq!(config.relay_path, "/ohttp");
        assert_eq!(config.key_config_path, "/v1/ohttp/config");
        assert_eq!(config.response_timeout.as_secs(), 30);
        assert!(config.key_config_upstream.is_none());
        assert_eq!(config.gateway.path(), "/ohttp");
        Ok(())
    }

    #[test]
    fn every_field_is_read() -> Result<(), ConfigError> {
        let config = Config::parse(
            "listen = \"[::1]:9000\"\n\
             gateway = \"http://10.0.0.2:8080/g\"\n\
             gateway_key_config = \"http://10.0.0.2:8080/keys\"\n\
             relay_path = \"/r\"\n\
             key_config_path = \"/k\"\n\
             response_timeout_seconds = 5\n",
        )?;
        assert_eq!(config.listen.port(), 9000);
        assert_eq!(config.relay_path, "/r");
        assert_eq!(config.key_config_path, "/k");
        assert_eq!(config.response_timeout.as_secs(), 5);
        assert_eq!(
            config.key_config_upstream.map(|uri| uri.path().to_owned()),
            Some("/keys".to_owned())
        );
        Ok(())
    }

    #[test]
    fn bad_files_are_refused() {
        let base = "listen = \"127.0.0.1:1\"\n";
        let cases = [
            (format!("{base}gateway = \"https://g/ohttp\"\n"), "https"),
            (format!("{base}gateway = \"/ohttp\"\n"), "relative"),
            (
                format!("{base}gateway = \"http://g/\"\nrelay_path = \"x\"\n"),
                "path",
            ),
            (
                format!(
                    "{base}gateway = \"http://g/\"\nrelay_path = \"/a\"\nkey_config_path = \"/a\"\n"
                ),
                "same paths",
            ),
            (
                format!("{base}gateway = \"http://g/\"\nlog = true\n"),
                "unknown field",
            ),
            ("gateway = \"http://g/\"\n".to_owned(), "missing listen"),
        ];
        for (text, why) in cases {
            assert!(Config::parse(&text).is_err(), "{why}");
        }
        assert_eq!(
            Config::parse(&format!(
                "{base}gateway = \"http://g/\"\ngateway_key_config = \"ftp://g/\"\n"
            ))
            .err(),
            Some(ConfigError::GatewayUrl("gateway_key_config"))
        );
    }
}
