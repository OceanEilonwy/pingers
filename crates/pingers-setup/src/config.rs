// SPDX-License-Identifier: AGPL-3.0-only
//! `setup.toml`, which nodo renders from the `pingers` app's settings and the settings of the
//! four services it references:
//!
//! ```toml
//! listen = "127.0.0.1:9479"                  # loopback only: nodo's helper reads the page
//! domain = "example.org"                     # the owner's domain (pingers-push-relay.domain)
//! server_id = "https://pingers.example.org"  # pingers-server's public addresses
//! ohttp_relay_url = "https://ohttp.example.org/ohttp"
//! push_relay_url = "https://push.example.org"
//! map_host = "https://maps.example.org"
//! registration_code = "<the server's code>"  # a sealed secret, read through a grant
//! gateway_key_config = "http://127.0.0.1:9471/v1/ohttp/config"   # the gateway, on loopback
//! push_relay = "http://127.0.0.1:9472"       # the push relay, on loopback
//! map_host_local = "http://127.0.0.1:9474"   # the map host, on loopback
//! github_repository = "OceanEilonwy/pingers" # where the app builds' variables go
//! release_tag = "v1.0.0-rc.1"                # the tag the checklist suggests
//! ```
//!
//! Every field is required and unknown fields are refused. Errors name fields, never values:
//! the file holds the registration code.

use std::net::SocketAddr;

use pingers_proto::{SecretText, ServerId};
use serde::Deserialize;
use zeroize::Zeroizing;

/// Why `setup.toml` was refused. Never carries a value.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Not TOML, a field missing or unknown, or a value of the wrong type.
    #[error("setup.toml: {0}")]
    Syntax(String),
    /// A field is out of range.
    #[error("setup.toml: {field} {problem}")]
    Field {
        /// The field.
        field: &'static str,
        /// What is wrong with it, in words.
        problem: &'static str,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    listen: SocketAddr,
    domain: String,
    server_id: String,
    ohttp_relay_url: String,
    push_relay_url: String,
    map_host: String,
    registration_code: Zeroizing<String>,
    gateway_key_config: String,
    push_relay: String,
    map_host_local: String,
    github_repository: String,
    release_tag: String,
}

/// The parsed file.
#[derive(Debug)]
pub struct Config {
    /// Where the page is served; a loopback address.
    pub listen: SocketAddr,
    /// The owner's domain.
    pub domain: String,
    /// The server's origin, the setup link's host.
    pub server_id: ServerId,
    /// The OHTTP relay's URL as `/v1/params` advertises it (ending in `/ohttp`).
    pub ohttp_relay_url: String,
    /// The push relay's public origin.
    pub push_relay_url: String,
    /// The map host's public origin.
    pub map_host: String,
    /// The server's registration code.
    pub registration_code: SecretText,
    /// The gateway's `GET /v1/ohttp/config` on loopback; `/next` after it is the other slot.
    pub gateway_key_config: String,
    /// The push relay's loopback origin.
    pub push_relay: String,
    /// The map host's loopback origin.
    pub map_host_local: String,
    /// `owner/name` on GitHub.
    pub github_repository: String,
    /// The release tag the checklist suggests.
    pub release_tag: String,
}

impl Config {
    /// Parses and checks `setup.toml`.
    ///
    /// # Errors
    ///
    /// [`ConfigError`], naming the field.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let file: File = toml::from_str(text).map_err(|error| {
            // toml's message can quote the offending value; keep only its first line, which
            // names the place.
            ConfigError::Syntax(
                error
                    .message()
                    .lines()
                    .next()
                    .unwrap_or("not valid")
                    .to_owned(),
            )
        })?;
        if !file.listen.ip().is_loopback() {
            return Err(ConfigError::Field {
                field: "listen",
                problem: "must be a loopback address",
            });
        }
        if !is_domain(&file.domain) {
            return Err(ConfigError::Field {
                field: "domain",
                problem: "must be a domain name such as example.org",
            });
        }
        for (field, url) in [
            ("ohttp_relay_url", &file.ohttp_relay_url),
            ("push_relay_url", &file.push_relay_url),
            ("map_host", &file.map_host),
        ] {
            if !url.starts_with("https://") {
                return Err(ConfigError::Field {
                    field,
                    problem: "must be an https:// URL",
                });
            }
        }
        for (field, url) in [
            ("gateway_key_config", &file.gateway_key_config),
            ("push_relay", &file.push_relay),
            ("map_host_local", &file.map_host_local),
        ] {
            if !url.starts_with("http://127.") && !url.starts_with("http://[::1]") {
                return Err(ConfigError::Field {
                    field,
                    problem: "must be an http:// URL on loopback",
                });
            }
        }
        let code = file.registration_code;
        if code.is_empty() || code.len() > pingers_proto::MAX_REGISTRATION_CODE_BYTES {
            return Err(ConfigError::Field {
                field: "registration_code",
                problem: "must be 1 to 128 bytes",
            });
        }
        if file.github_repository.split('/').count() != 2 {
            return Err(ConfigError::Field {
                field: "github_repository",
                problem: "must be owner/name",
            });
        }
        Ok(Self {
            listen: file.listen,
            domain: file.domain,
            server_id: ServerId::new(file.server_id).map_err(|_| ConfigError::Field {
                field: "server_id",
                problem: "must be an https:// origin",
            })?,
            ohttp_relay_url: file.ohttp_relay_url,
            push_relay_url: file.push_relay_url,
            map_host: file.map_host,
            registration_code: SecretText::new(code.to_string()),
            gateway_key_config: file.gateway_key_config,
            push_relay: file.push_relay,
            map_host_local: file.map_host_local,
            github_repository: file.github_repository,
            release_tag: file.release_tag,
        })
    }

    /// The four public origins: the server, the OHTTP relay, the push relay and the map host.
    #[must_use]
    pub fn origins(&self) -> [Site<'_>; 4] {
        let relay = self.ohttp_relay_url.trim_end_matches('/');
        let relay = relay.strip_suffix("/ohttp").unwrap_or(relay);
        [
            Site {
                package: "pingers-server",
                site: "api",
                origin: self.server_id.as_str(),
            },
            Site {
                package: "pingers-ohttp-relay",
                site: "relay",
                origin: relay,
            },
            Site {
                package: "pingers-push-relay",
                site: "push",
                origin: &self.push_relay_url,
            },
            Site {
                package: "pingers-map-host",
                site: "maps",
                origin: &self.map_host,
            },
        ]
    }
}

/// One of the four public sites: the nodo app and site name, and its origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site<'config> {
    /// The nodo app.
    pub package: &'static str,
    /// Its `[[web]]` site's name.
    pub site: &'static str,
    /// `https://host`.
    pub origin: &'config str,
}

impl Site<'_> {
    /// The port: the one the origin names, else 443.
    #[must_use]
    pub fn port(&self) -> u16 {
        let rest = self.origin.strip_prefix("https://").unwrap_or(self.origin);
        let authority = rest.split('/').next().unwrap_or(rest);
        authority
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
            .unwrap_or(443)
    }

    /// The host name, without the scheme or a port.
    #[must_use]
    pub fn host(&self) -> &str {
        let rest = self.origin.strip_prefix("https://").unwrap_or(self.origin);
        let rest = rest.split('/').next().unwrap_or(rest);
        rest.split(':').next().unwrap_or(rest)
    }
}

/// Letters, digits and hyphens in dot-separated labels, at least two labels.
fn is_domain(text: &str) -> bool {
    let labels: Vec<&str> = text.split('.').collect();
    text.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

#[cfg(test)]
pub(crate) mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    /// A file as nodo renders it for `example.org`.
    pub(crate) fn file() -> String {
        "listen = \"127.0.0.1:9479\"\ndomain = \"example.org\"\n\
         server_id = \"https://pingers.example.org\"\n\
         ohttp_relay_url = \"https://ohttp.example.org/ohttp\"\n\
         push_relay_url = \"https://push.example.org\"\nmap_host = \"https://maps.example.org\"\n\
         registration_code = \"0123456789abcdef0123456789abcdef\"\n\
         gateway_key_config = \"http://127.0.0.1:9471/v1/ohttp/config\"\n\
         push_relay = \"http://127.0.0.1:9472\"\nmap_host_local = \"http://127.0.0.1:9474\"\n\
         github_repository = \"OceanEilonwy/pingers\"\nrelease_tag = \"v1.0.0-rc.1\"\n"
            .to_owned()
    }

    #[test]
    fn a_rendered_file_parses() -> Result<(), ConfigError> {
        let config = Config::parse(&file())?;
        assert_eq!(config.server_id.as_str(), "https://pingers.example.org");
        assert_eq!(
            config.registration_code.expose(),
            "0123456789abcdef0123456789abcdef"
        );
        let origins = config.origins();
        let hosts: Vec<&str> = origins.iter().map(Site::host).collect();
        assert_eq!(
            hosts,
            [
                "pingers.example.org",
                "ohttp.example.org",
                "push.example.org",
                "maps.example.org"
            ]
        );
        // The relay's origin, without the /ohttp the server advertises.
        assert_eq!(
            config.origins().get(1).map(|site| site.origin),
            Some("https://ohttp.example.org")
        );
        Ok(())
    }

    #[test]
    fn bad_fields_are_named_and_values_never_shown() {
        let cases = [
            ("127.0.0.1:9479", "0.0.0.0:9479", "listen"),
            ("domain = \"example.org\"", "domain = \"example\"", "domain"),
            (
                "https://push.example.org",
                "http://push.example.org",
                "push_relay_url",
            ),
            (
                "http://127.0.0.1:9472",
                "http://10.0.0.1:9472",
                "push_relay",
            ),
            ("OceanEilonwy/pingers", "pingers", "github_repository"),
            ("0123456789abcdef0123456789abcdef", "", "registration_code"),
            (
                "https://pingers.example.org",
                "http://pingers.example.org",
                "server_id",
            ),
        ];
        for (from, to, field) in cases {
            let text = file().replace(from, to);
            let named = match Config::parse(&text) {
                Err(ConfigError::Field { field: named, .. }) => Some(named),
                _ => None,
            };
            assert_eq!(named, Some(field));
        }
        let unknown = format!("{}extra = 1\n", file());
        assert!(matches!(
            Config::parse(&unknown),
            Err(ConfigError::Syntax(_))
        ));
        let quoted = file().replace("listen = \"127.0.0.1:9479\"", "listen = \"secret-value\"");
        let message = Config::parse(&quoted)
            .map(|_| ())
            .err()
            .map(|error| error.to_string());
        assert!(message.is_some_and(|message| !message.contains("0123456789abcdef")));
    }

    #[test]
    fn a_code_may_be_128_bytes_and_no_more() {
        let code =
            |length: usize| file().replace("0123456789abcdef0123456789abcdef", &"c".repeat(length));
        assert!(Config::parse(&code(128)).is_ok());
        assert!(matches!(
            Config::parse(&code(129)),
            Err(ConfigError::Field {
                field: "registration_code",
                ..
            })
        ));
    }

    #[test]
    fn domains_are_checked_label_by_label() {
        for good in ["example.org", "a-b.example.co.uk", "x1.io"] {
            assert!(is_domain(good), "{good}");
        }
        for bad in [
            "example", "-a.org", "a-.org", "a..org", "a_b.org", ".org", "a.org.",
        ] {
            assert!(!is_domain(bad), "{bad}");
        }
    }
}
