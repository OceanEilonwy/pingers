// SPDX-License-Identifier: AGPL-3.0-only
//! The configuration file (`--config <FILE>`, TOML).
//!
//! ```toml
//! # Omit [server] to run against the in-process harness (pingers-testkit).
//! [server]
//! url = "https://pingers.example.org"
//! ohttp_relay = "https://relay.example.net/ohttp"   # required with a server
//! push_relay = "https://push.example.com"           # optional
//!
//! seed = 7   # overrides each scenario's seed and seeds load generation
//! ```
//!
//! URLs must be `https`, except on loopback (`localhost`, `127.0.0.1`, `::1`) for local servers.

use serde::Deserialize;
use url::Url;

/// Why the configuration was rejected.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    /// Not valid TOML or not the documented shape.
    #[error("config is not valid: {0}")]
    Parse(#[from] toml::de::Error),
    /// A URL did not parse.
    #[error("`{field}` is not a URL: {source}")]
    Url {
        /// The key.
        field: &'static str,
        /// Why.
        source: url::ParseError,
    },
    /// A URL to a remote host is not `https`.
    #[error("`{field}` must use https (plain http is allowed only on loopback)")]
    NotHttps {
        /// The key.
        field: &'static str,
    },
}

/// Where the clients send their traffic.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "one value per process; boxing would only add noise"
)]
pub(crate) enum Target {
    /// The in-process server, OHTTP relay and push relay from pingers-testkit.
    InProcess,
    /// A running deployment. Anonymous traffic always goes through the OHTTP relay (there is
    /// no direct mode, design B.12), so a relay is required.
    Remote {
        /// The server.
        server: Url,
        /// The OHTTP relay.
        ohttp_relay: Url,
        /// The push relay, if pushes should be sent.
        push_relay: Option<Url>,
    },
}

/// The validated configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    /// Where to connect.
    pub(crate) target: Target,
    /// A seed overriding scenario seeds, if given.
    pub(crate) seed: Option<u64>,
}

impl Default for Config {
    /// The in-process harness and each scenario's own seed.
    fn default() -> Self {
        Self {
            target: Target::InProcess,
            seed: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    server: Option<ServerTable>,
    seed: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerTable {
    url: String,
    ohttp_relay: String,
    push_relay: Option<String>,
}

/// Parses and validates a configuration file.
///
/// # Errors
///
/// [`ConfigError`] naming the first problem.
pub(crate) fn parse(text: &str) -> Result<Config, ConfigError> {
    let file: ConfigFile = toml::from_str(text)?;
    let target = match file.server {
        None => Target::InProcess,
        Some(server) => Target::Remote {
            server: url("server.url", &server.url)?,
            ohttp_relay: url("server.ohttp_relay", &server.ohttp_relay)?,
            push_relay: server
                .push_relay
                .map(|push| url("server.push_relay", &push))
                .transpose()?,
        },
    };
    Ok(Config {
        target,
        seed: file.seed,
    })
}

fn url(field: &'static str, text: &str) -> Result<Url, ConfigError> {
    let parsed = Url::parse(text).map_err(|source| ConfigError::Url { field, source })?;
    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if loopback => Ok(parsed),
        _ => Err(ConfigError::NotHttps { field }),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn an_empty_file_means_the_in_process_harness() -> Result<(), ConfigError> {
        assert_eq!(parse("")?, Config::default());
        Ok(())
    }

    #[test]
    fn a_remote_server_needs_its_relay() -> Result<(), ConfigError> {
        let config = parse(
            r#"
            seed = 9
            [server]
            url = "https://pingers.example.org"
            ohttp_relay = "https://relay.example.net/ohttp"
            "#,
        )?;
        assert_eq!(config.seed, Some(9));
        assert!(matches!(
            config.target,
            Target::Remote { push_relay: None, ref server, .. } if server.as_str() == "https://pingers.example.org/"
        ));
        assert!(matches!(
            parse("[server]\nurl = \"https://a.example\""),
            Err(ConfigError::Parse(_))
        ));
        Ok(())
    }

    #[test]
    fn plain_http_is_allowed_only_on_loopback() -> Result<(), ConfigError> {
        let local = parse(
            "[server]\nurl = \"http://127.0.0.1:8080\"\nohttp_relay = \"http://localhost:8081\"\npush_relay = \"http://[::1]:8082\"",
        )?;
        assert!(matches!(
            local.target,
            Target::Remote {
                push_relay: Some(_),
                ..
            }
        ));
        assert!(matches!(
            parse("[server]\nurl = \"https://a.example\"\nohttp_relay = \"http://relay.example\""),
            Err(ConfigError::NotHttps {
                field: "server.ohttp_relay"
            })
        ));
        assert!(matches!(
            parse("[server]\nurl = \"ftp://127.0.0.1\"\nohttp_relay = \"https://r.example\""),
            Err(ConfigError::NotHttps {
                field: "server.url"
            })
        ));
        Ok(())
    }

    #[test]
    fn a_malformed_url_names_its_field() {
        assert!(matches!(
            parse(
                "[server]\nurl = \"https://a.example\"\nohttp_relay = \"https://r.example\"\npush_relay = \"not a url\""
            ),
            Err(ConfigError::Url {
                field: "server.push_relay",
                ..
            })
        ));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(matches!(parse("sed = 1"), Err(ConfigError::Parse(_))));
    }
}
