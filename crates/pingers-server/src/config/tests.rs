// SPDX-License-Identifier: AGPL-3.0-only
//! The configuration file: defaults, every field, and each refusal.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use pingers_ohttp::GatewayKey;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;

use super::{AddressList, ConfigError, Logging, OhttpKeys, RelayAllowList, ServerConfig};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Expected = fn(&ConfigError) -> bool;

const SECRET_A: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const SECRET_B: &str = "0202020202020202020202020202020202020202020202020202020202020202";

fn minimal(keys: &str) -> String {
    format!(
        "listen = \"127.0.0.1:8080\"\nserver_id = \"https://pingers.test\"\n\
         [ohttp]\nrelays = [\"10.0.0.5\"]\n{keys}"
    )
}

fn key(key_id: u8, secret: &str) -> String {
    format!("[[ohttp.keys]]\nkey_id = {key_id}\nsecret = \"{secret}\"\n")
}

#[test]
fn minimal_file_gets_the_quiet_defaults() -> TestResult {
    let config = ServerConfig::parse(&minimal(&key(1, SECRET_A)))?;
    assert_eq!(config.logging, Logging::Off);
    assert_eq!(config.request_timeout, Duration::from_secs(30));
    assert_eq!(config.metrics, AddressList::default());
    assert!(config.relays.allows(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
    assert!(!config.relays.allows(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 6))));
    assert_eq!(config.ohttp_keys.served().key_id(), 1);
    assert_eq!(config.listen.port(), 8080);
    Ok(())
}

#[test]
fn every_field_is_read() -> TestResult {
    let config = ServerConfig::parse(&format!(
        "listen = \"[::1]:9000\"\n\
         server_id = \"https://pingers.test\"\n\
         logging = \"counts\"\n\
         counts_interval_s = 60\n\
         request_timeout_s = 5\n\
         [metrics]\nallow = [\"127.0.0.1\"]\n\
         [ohttp]\nrelays = [\"fd00::5\"]\n{}{}",
        key(7, SECRET_A),
        key(3, SECRET_B)
    ))?;
    assert_eq!(
        config.logging,
        Logging::Counts {
            interval: Duration::from_secs(60)
        }
    );
    assert_eq!(config.request_timeout, Duration::from_secs(5));
    assert!(config.metrics.contains(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(config.relays.allows("fd00::5".parse()?));
    // The first key listed is the one served; the second still opens requests.
    assert_eq!(config.ohttp_keys.served().key_id(), 7);
    assert_eq!(config.ohttp_keys.into_keys().len(), 2);
    Ok(())
}

#[test]
fn the_same_secret_gives_the_same_served_config() -> TestResult {
    let first = ServerConfig::parse(&minimal(&key(1, SECRET_A)))?;
    let again = ServerConfig::parse(&minimal(&key(1, SECRET_A)))?;
    let other = ServerConfig::parse(&minimal(&key(1, SECRET_B)))?;
    assert_eq!(first.ohttp_keys.served(), again.ohttp_keys.served());
    assert_ne!(first.ohttp_keys.served(), other.ohttp_keys.served());
    Ok(())
}

#[test]
fn bad_files_are_refused_with_the_rule_they_break() {
    let with_key = minimal(&key(1, SECRET_A));
    let cases: [(String, Expected); 8] = [
        (minimal(""), |error| {
            matches!(error, ConfigError::NoOhttpKey)
        }),
        (
            minimal(&format!("{}{}", key(1, SECRET_A), key(1, SECRET_B))),
            |error| matches!(error, ConfigError::DuplicateKeyId { key_id: 1 }),
        ),
        (minimal(&key(1, "zz")), |error| {
            matches!(error, ConfigError::BadHex)
        }),
        (
            format!(
                "listen = \"127.0.0.1:1\"\nserver_id = \"https://p.test\"\n[ohttp]\nrelays = []\n{}",
                key(1, SECRET_A)
            ),
            |error| matches!(error, ConfigError::NoRelays),
        ),
        (format!("request_timeout_s = 0\n{with_key}"), |error| {
            matches!(
                error,
                ConfigError::OutOfRange {
                    field: "request_timeout_s"
                }
            )
        }),
        (
            format!("logging = \"counts\"\ncounts_interval_s = 0\n{with_key}"),
            |error| {
                matches!(
                    error,
                    ConfigError::OutOfRange {
                        field: "counts_interval_s"
                    }
                )
            },
        ),
        (format!("log = true\n{with_key}"), |error| {
            matches!(error, ConfigError::Syntax { line: 1 })
        }),
        (
            format!(
                "server_id = \"https://p.test\"\nlisten = \"127.0.0.1:1\"\n\n[ohttp]\nrelays = [\"not an address\"]\n{}",
                key(1, SECRET_A)
            ),
            |error| matches!(error, ConfigError::Syntax { line: 5 }),
        ),
    ];
    for (text, expected) in cases {
        let refused = ServerConfig::parse(&text).err();
        assert!(
            refused.as_ref().is_some_and(expected),
            "{text}\n{refused:?}"
        );
    }
}

#[test]
fn counts_logging_without_an_interval_reports_hourly() -> TestResult {
    let config = ServerConfig::parse(&format!(
        "logging = \"counts\"\n{}",
        minimal(&key(1, SECRET_A))
    ))?;
    assert_eq!(
        config.logging,
        Logging::Counts {
            interval: Duration::from_secs(3600)
        }
    );
    Ok(())
}

#[test]
fn ipv4_mapped_peers_match_their_ipv4_entry() -> TestResult {
    let relays = RelayAllowList::new(vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))])?;
    let mapped = IpAddr::V6(Ipv4Addr::new(10, 0, 0, 5).to_ipv6_mapped());
    assert!(relays.allows(mapped));
    let listed_mapped = AddressList::new(vec![mapped]);
    assert!(listed_mapped.contains(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
    assert!(!listed_mapped.contains(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    Ok(())
}

#[test]
fn keys_built_in_code_follow_the_same_rules() -> TestResult {
    let mut rng = ChaCha20Rng::seed_from_u64(1);
    assert!(matches!(
        OhttpKeys::new(Vec::new()),
        Err(ConfigError::NoOhttpKey)
    ));
    let keys = OhttpKeys::new(vec![
        GatewayKey::generate(4, &mut rng)?,
        GatewayKey::generate(5, &mut rng)?,
    ])?;
    assert_eq!(keys.served().key_id(), 4);
    assert_eq!(
        format!("{keys:?}"),
        "OhttpKeys { count: 2, served_key_id: 4 }"
    );
    assert!(matches!(
        OhttpKeys::new(vec![
            GatewayKey::generate(4, &mut rng)?,
            GatewayKey::generate(4, &mut rng)?
        ]),
        Err(ConfigError::DuplicateKeyId { key_id: 4 })
    ));
    Ok(())
}

fn relay(relay_id: u16, url: &str) -> String {
    format!("[[push_relays]]\nrelay_id = {relay_id}\nurl = \"{url}\"\ncredential = \"c\"\n")
}

#[test]
fn wiring_fields_are_read_with_defaults() -> TestResult {
    use pingers_mailbox::RelayDirectory as _;
    use pingers_proto::{RelayId, RelayRef};

    let defaults = ServerConfig::parse(&minimal(&key(1, SECRET_A)))?;
    assert_eq!(defaults.mailbox_store, super::Database::Memory);
    assert_eq!(defaults.mailbox_posts_per_bucket.get(), 60);
    assert_eq!(defaults.push_timeout, Duration::from_secs(10));
    assert_eq!(defaults.expire_interval, Duration::from_secs(60));
    assert_eq!(
        defaults
            .push_relays
            .resolve(&RelayRef::Listed(RelayId::new(1))),
        None
    );

    let config = ServerConfig::parse(&format!(
        "mailbox_db = \"/tmp/m.sqlite\"\nmailbox_posts_per_bucket = 5\npush_timeout_s = 3\n\
         expire_interval_s = 9\n{}{}",
        minimal(&key(1, SECRET_A)),
        relay(7, "https://push.example")
    ))?;
    assert_eq!(
        config.mailbox_store,
        super::Database::File("/tmp/m.sqlite".into())
    );
    assert_eq!(config.mailbox_posts_per_bucket.get(), 5);
    assert_eq!(config.push_timeout, Duration::from_secs(3));
    assert_eq!(config.expire_interval, Duration::from_secs(9));
    let target = config
        .push_relays
        .resolve(&RelayRef::Listed(RelayId::new(7)))
        .ok_or("relay 7 not listed")?;
    assert_eq!(target.url.as_str(), "https://push.example");
    assert_eq!(target.credential.authorization(), "Bearer c");
    Ok(())
}

#[test]
fn bad_wiring_fields_are_refused() {
    let with_key = minimal(&key(1, SECRET_A));
    let cases: [(String, Expected); 5] = [
        (
            format!(
                "{with_key}{}{}",
                relay(1, "https://a"),
                relay(1, "https://b")
            ),
            |error| matches!(error, ConfigError::DuplicateRelayId { relay_id: 1 }),
        ),
        (format!("{with_key}{}", relay(1, "http://a")), |error| {
            matches!(error, ConfigError::RelayUrl)
        }),
        (
            format!("mailbox_posts_per_bucket = 0\n{with_key}"),
            |error| {
                matches!(
                    error,
                    ConfigError::OutOfRange {
                        field: "mailbox_posts_per_bucket"
                    }
                )
            },
        ),
        (format!("push_timeout_s = 0\n{with_key}"), |error| {
            matches!(
                error,
                ConfigError::OutOfRange {
                    field: "push_timeout_s"
                }
            )
        }),
        (format!("expire_interval_s = 0\n{with_key}"), |error| {
            matches!(
                error,
                ConfigError::OutOfRange {
                    field: "expire_interval_s"
                }
            )
        }),
    ];
    for (text, expected) in cases {
        let refused = ServerConfig::parse(&text).err();
        assert!(
            refused.as_ref().is_some_and(expected),
            "{text}\n{refused:?}"
        );
    }
}

#[test]
fn server_identity_and_published_fields_are_read() -> TestResult {
    let defaults = ServerConfig::parse(&minimal(&key(1, SECRET_A)))?;
    assert_eq!(defaults.server_id.as_str(), "https://pingers.test");
    assert_eq!(defaults.server_operator, "https://pingers.test");
    assert!(defaults.registration_codes.is_empty());
    assert!(defaults.ohttp_relay_urls.is_empty());
    assert!(defaults.advertised_push_relays.is_empty());
    assert_eq!(defaults.push_relay_ca, None);
    assert_eq!(defaults.delivery_store, super::Database::Memory);
    assert_eq!(defaults.quota_store, super::Database::Memory);

    let config = ServerConfig::parse(&format!(
        "server_operator = \"Example Ltd\"\nregistration_codes = [\"a\", \"b\"]\n\
         ohttp_relay_urls = [\"https://relay.example/ohttp\"]\n\
         delivery_db = \"/tmp/g.sqlite\"\nquota_db = \"/tmp/q.sqlite\"\n\
         push_relay_ca = \"/tmp/ca.pem\"\n{}{}key_config_hash = \"{}\"\noperator = \"Relay Ltd\"\n{}",
        minimal(&key(1, SECRET_A)),
        relay(3, "https://push.example"),
        "ab".repeat(32),
        relay(4, "https://other.example"),
    ))?;
    assert_eq!(config.server_operator, "Example Ltd");
    assert_eq!(
        config
            .registration_codes
            .iter()
            .map(pingers_proto::SecretText::expose)
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(
        config
            .ohttp_relay_urls
            .iter()
            .map(pingers_proto::RelayUrl::as_str)
            .collect::<Vec<_>>(),
        ["https://relay.example/ohttp"]
    );
    assert_eq!(
        config.delivery_store,
        super::Database::File("/tmp/g.sqlite".into())
    );
    assert_eq!(
        config.quota_store,
        super::Database::File("/tmp/q.sqlite".into())
    );
    assert_eq!(
        config.push_relay_ca,
        Some(super::CertificateSource::File("/tmp/ca.pem".into()))
    );
    // Only the relay with a key hash and an operator is advertised.
    assert_eq!(config.advertised_push_relays.len(), 1);
    let advertised = config.advertised_push_relays.first().ok_or("advertised")?;
    assert_eq!(advertised.relay_id, pingers_proto::RelayId::new(3));
    assert_eq!(advertised.url.as_str(), "https://push.example");
    assert_eq!(advertised.key_config_hash.as_bytes(), &[0xab; 32]);
    assert_eq!(advertised.operator, "Relay Ltd");
    Ok(())
}

#[test]
fn bad_identity_fields_are_refused() {
    let with_key = minimal(&key(1, SECRET_A));
    let http_server = with_key.replace("https://pingers.test", "http://pingers.test");
    let cases: [(String, Expected); 5] = [
        (http_server, |error| matches!(error, ConfigError::ServerId)),
        (
            with_key.replace("server_id = \"https://pingers.test\"\n", ""),
            |error| matches!(error, ConfigError::Syntax { .. }),
        ),
        (
            format!("ohttp_relay_urls = [\"http://relay.example\"]\n{with_key}"),
            |error| matches!(error, ConfigError::OhttpRelayUrl),
        ),
        (
            format!("{with_key}{}operator = \"x\"\n", relay(1, "https://a")),
            |error| matches!(error, ConfigError::RelayInfo),
        ),
        (
            format!(
                "{with_key}{}key_config_hash = \"zz\"\noperator = \"x\"\n",
                relay(1, "https://a")
            ),
            |error| matches!(error, ConfigError::RelayInfo),
        ),
    ];
    for (text, expected) in cases {
        let refused = ServerConfig::parse(&text).err();
        assert!(
            refused.as_ref().is_some_and(expected),
            "{text}\n{refused:?}"
        );
    }
}
