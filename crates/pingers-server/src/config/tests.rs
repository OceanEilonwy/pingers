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
    format!("listen = \"127.0.0.1:8080\"\n[ohttp]\nrelays = [\"10.0.0.5\"]\n{keys}")
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
                "listen = \"127.0.0.1:1\"\n[ohttp]\nrelays = []\n{}",
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
                "listen = \"127.0.0.1:1\"\n\n[ohttp]\nrelays = [\"not an address\"]\n{}",
                key(1, SECRET_A)
            ),
            |error| matches!(error, ConfigError::Syntax { line: 4 }),
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
