// SPDX-License-Identifier: AGPL-3.0-only
//! The configuration file: defaults, every field, and each refusal.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use pingers_ohttp::GatewayKey;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;

use super::{
    AddressList, ConfigError, Logging, OhttpKeys, RelayAllowList, ServedKey, ServerConfig,
    TrustedProxies,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The release signing certificate's fingerprint.
const RELEASE_CERT: &str = "AA:44:E4:54:B0:F8:FA:A8:9B:8F:69:EF:72:90:6D:AA:2A:B8:C3:F2:D6:BE:87:B1:23:C2:D3:30:C9:BD:18:16";

#[test]
fn app_links_are_optional_and_read_with_upper_case_fingerprints() -> TestResult {
    assert_eq!(
        ServerConfig::parse(&minimal(&key(1, SECRET_A)))?.app_links,
        None
    );
    let text = format!(
        "{}[app_links]\npackages = [\"org.pingers.app\", \"org.pingers.app.foss\"]\n\
         certificates = [\"{}\"]\n",
        minimal(&key(1, SECRET_A)),
        RELEASE_CERT.to_ascii_lowercase()
    );
    let links = ServerConfig::parse(&text)?
        .app_links
        .ok_or("app links were read")?;
    let packages: Vec<&str> = links
        .packages
        .iter()
        .map(super::AndroidPackage::as_str)
        .collect();
    assert_eq!(packages, ["org.pingers.app", "org.pingers.app.foss"]);
    let certificates: Vec<&str> = links
        .certificates
        .iter()
        .map(super::CertificateFingerprint::as_str)
        .collect();
    assert_eq!(certificates, [RELEASE_CERT]);
    Ok(())
}

#[test]
fn malformed_app_links_are_refused() {
    let base = minimal(&key(1, SECRET_A));
    let short_cert = RELEASE_CERT.get(..92).unwrap_or_default();
    let cases = [
        format!("[app_links]\npackages = []\ncertificates = [\"{RELEASE_CERT}\"]\n"),
        "[app_links]\npackages = [\"org.pingers.app\"]\ncertificates = []\n".to_owned(),
        format!("[app_links]\npackages = [\"pingers\"]\ncertificates = [\"{RELEASE_CERT}\"]\n"),
        format!(
            "[app_links]\npackages = [\"org.1pingers\"]\ncertificates = [\"{RELEASE_CERT}\"]\n"
        ),
        format!(
            "[app_links]\npackages = [\"org.pin\\\"gers\"]\ncertificates = [\"{RELEASE_CERT}\"]\n"
        ),
        format!(
            "[app_links]\npackages = [\"org.pingers.app\"]\ncertificates = [\"{short_cert}\"]\n"
        ),
        "[app_links]\npackages = [\"org.pingers.app\"]\ncertificates = [\"zz\"]\n".to_owned(),
    ];
    for case in cases {
        let refused = ServerConfig::parse(&format!("{base}{case}")).err();
        assert!(
            matches!(refused, Some(ConfigError::AppLinks)),
            "{case}\n{refused:?}"
        );
    }
}
type Expected = fn(&ConfigError) -> bool;

const SECRET_A: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const SECRET_B: &str = "0202020202020202020202020202020202020202020202020202020202020202";

fn minimal(keys: &str) -> String {
    format!(
        "listen = \"127.0.0.1:8080\"\nserver_id = \"https://pingers.test\"\n\
         [ohttp]\nlisten = \"127.0.0.1:9471\"\nrelays = [\"10.0.0.5\"]\n{keys}"
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
    assert_eq!(config.membership_proofs, super::MembershipProofs::On);
    Ok(())
}

#[test]
fn membership_proofs_can_be_switched_off_and_nothing_else_is_accepted() -> TestResult {
    let with_key = minimal(&key(1, SECRET_A));
    let off = ServerConfig::parse(&format!("membership_proofs = \"off\"\n{with_key}"))?;
    assert_eq!(off.membership_proofs, super::MembershipProofs::Off);
    let on = ServerConfig::parse(&format!("membership_proofs = \"on\"\n{with_key}"))?;
    assert_eq!(on.membership_proofs, super::MembershipProofs::On);
    assert!(matches!(
        ServerConfig::parse(&format!("membership_proofs = \"maybe\"\n{with_key}")),
        Err(ConfigError::Syntax { .. })
    ));
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
         [ohttp]\nlisten = \"[::1]:9001\"\nrelays = [\"fd00::5\"]\ntrusted_proxies = [\"::1\", \"127.0.0.1\"]\nserved_key_id = 3\n{}{}",
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
    assert_eq!(config.gateway_listen, "[::1]:9001".parse()?);
    assert_eq!(
        config.trusted_proxies,
        TrustedProxies::new(vec![
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        ])
    );
    // served_key_id picks the second key listed; the first still opens requests.
    assert_eq!(config.ohttp_keys.served().key_id(), 3);
    assert_eq!(
        config
            .ohttp_keys
            .configs()
            .iter()
            .map(pingers_ohttp::KeyConfig::key_id)
            .collect::<Vec<_>>(),
        [7, 3]
    );
    assert_eq!(config.ohttp_keys.into_keys().len(), 2);
    Ok(())
}

#[test]
fn without_served_key_id_the_first_key_is_served_and_no_proxy_is_trusted() -> TestResult {
    let config = ServerConfig::parse(&minimal(&format!(
        "{}{}",
        key(7, SECRET_A),
        key(3, SECRET_B)
    )))?;
    assert_eq!(config.ohttp_keys.served().key_id(), 7);
    assert_eq!(config.trusted_proxies, TrustedProxies::default());
    assert_eq!(config.gateway_listen, "127.0.0.1:9471".parse()?);
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
    let cases: [(String, Expected); 11] = [
        (minimal(""), |error| {
            matches!(error, ConfigError::NoOhttpKey)
        }),
        (minimal("served_key_id = 1\n"), |error| {
            matches!(error, ConfigError::NoOhttpKey)
        }),
        (
            minimal(&format!("served_key_id = 2\n{}", key(1, SECRET_A))),
            |error| matches!(error, ConfigError::UnknownServedKey { key_id: 2 }),
        ),
        // The gateway's own listener is required: there is no way to put it on the public one.
        (
            with_key.replace("listen = \"127.0.0.1:9471\"\n", ""),
            |error| matches!(error, ConfigError::Syntax { .. }),
        ),
        (
            minimal(&format!("{}{}", key(1, SECRET_A), key(1, SECRET_B))),
            |error| matches!(error, ConfigError::DuplicateKeyId { key_id: 1 }),
        ),
        (minimal(&key(1, "zz")), |error| {
            matches!(error, ConfigError::BadHex)
        }),
        (
            format!(
                "listen = \"127.0.0.1:1\"\nserver_id = \"https://p.test\"\n[ohttp]\nlisten = \"127.0.0.1:2\"\nrelays = []\n{}",
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
                "server_id = \"https://p.test\"\nlisten = \"127.0.0.1:1\"\n\n[ohttp]\nrelays = [\"not an address\"]\nlisten = \"127.0.0.1:2\"\n{}",
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
    // A duplicate is refused even when the served id names a key that exists.
    assert!(matches!(
        OhttpKeys::serving(
            vec![
                GatewayKey::generate(4, &mut rng)?,
                GatewayKey::generate(5, &mut rng)?,
                GatewayKey::generate(4, &mut rng)?
            ],
            ServedKey::Id(5)
        ),
        Err(ConfigError::DuplicateKeyId { key_id: 4 })
    ));
    let second = OhttpKeys::serving(
        vec![
            GatewayKey::generate(4, &mut rng)?,
            GatewayKey::generate(5, &mut rng)?,
        ],
        ServedKey::Id(5),
    )?;
    assert_eq!(second.served().key_id(), 5);
    assert!(matches!(
        OhttpKeys::serving(Vec::new(), ServedKey::Id(5)),
        Err(ConfigError::NoOhttpKey)
    ));
    Ok(())
}

fn relay(relay_id: u16, url: &str) -> String {
    relay_at(relay_id, url, "https://public.example")
}

/// A push relay called at `url` that devices reach at `public_url`.
fn relay_at(relay_id: u16, url: &str, public_url: &str) -> String {
    format!(
        "[[push_relays]]\nrelay_id = {relay_id}\nurl = \"{url}\"\npublic_url = \"{public_url}\"\n\
         credential = \"c\"\n"
    )
}

#[test]
fn wiring_fields_are_read_with_defaults() -> TestResult {
    use pingers_mailbox::RelayDirectory as _;
    use pingers_proto::{RelayId, RelayRef};

    let defaults = ServerConfig::parse(&minimal(&key(1, SECRET_A)))?;
    assert_eq!(defaults.mailbox_store, super::Database::Memory);
    assert_eq!(defaults.mailbox_posts_per_bucket.get(), 60);
    assert_eq!(defaults.signups_per_hour.get().get(), 60);
    assert_eq!(defaults.idle_timeout, Duration::from_secs(120));
    assert_eq!(defaults.max_connections.get(), 4096);
    assert_eq!(defaults.gateway_max_connections.get(), 256);
    assert_eq!(defaults.push_timeout, Duration::from_secs(10));
    assert_eq!(defaults.expire_interval, Duration::from_secs(60));
    assert_eq!(
        defaults
            .push_relays
            .resolve(&RelayRef::Listed(RelayId::new(1))),
        None
    );

    let config = ServerConfig::parse(&format!(
        "mailbox_db = \"/tmp/m.sqlite\"\nmailbox_posts_per_bucket = 5\nsignups_per_hour = 7\npush_timeout_s = 3\n\
         expire_interval_s = 9\nidle_timeout_s = 11\nmax_connections = 12\n{}{}",
        minimal(&key(1, SECRET_A)).replace("[ohttp]\n", "[ohttp]\nmax_connections = 13\n"),
        relay(7, "https://push.example")
    ))?;
    assert_eq!(
        config.mailbox_store,
        super::Database::File("/tmp/m.sqlite".into())
    );
    assert_eq!(config.mailbox_posts_per_bucket.get(), 5);
    assert_eq!(config.signups_per_hour.get().get(), 7);
    assert_eq!(config.idle_timeout, Duration::from_secs(11));
    assert_eq!(config.max_connections.get(), 12);
    assert_eq!(config.gateway_max_connections.get(), 13);
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
    let cases: [(String, Expected); 9] = [
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
        (format!("idle_timeout_s = 0\n{with_key}"), |error| {
            matches!(
                error,
                ConfigError::OutOfRange {
                    field: "idle_timeout_s"
                }
            )
        }),
        (format!("max_connections = 0\n{with_key}"), |error| {
            matches!(
                error,
                ConfigError::OutOfRange {
                    field: "max_connections"
                }
            )
        }),
        (
            with_key.replace("[ohttp]\n", "[ohttp]\nmax_connections = 0\n"),
            |error| {
                matches!(
                    error,
                    ConfigError::OutOfRange {
                        field: "ohttp.max_connections"
                    }
                )
            },
        ),
        (format!("signups_per_hour = 0\n{with_key}"), |error| {
            matches!(
                error,
                ConfigError::OutOfRange {
                    field: "signups_per_hour"
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
fn a_push_relay_on_this_machine_is_called_over_loopback_and_advertised_by_its_public_url()
-> TestResult {
    use pingers_mailbox::RelayDirectory as _;
    use pingers_proto::{RelayId, RelayRef};

    let advertised = format!(
        "key_config_hash = \"{}\"\noperator = \"Me\"\n",
        pingers_proto::KeyConfigHash::from_bytes([0xcd; 32]).to_pin()
    );
    let config = ServerConfig::parse(&format!(
        "{}{}{advertised}",
        minimal(&key(1, SECRET_A)),
        relay_at(2, "http://127.0.0.1:9472", "https://push.example"),
    ))?;
    let target = config
        .push_relays
        .resolve(&RelayRef::Listed(RelayId::new(2)))
        .ok_or("relay 2 not listed")?;
    assert_eq!(target.url.as_str(), "http://127.0.0.1:9472");
    let listed = config.advertised_push_relays.first().ok_or("advertised")?;
    assert_eq!(listed.url.as_str(), "https://push.example");

    let with_key = minimal(&key(1, SECRET_A));
    let cases: [(String, Expected); 3] = [
        // Devices cannot be told a loopback URL: every relay names its public one.
        (
            format!(
                "{with_key}[[push_relays]]\nrelay_id = 2\nurl = \"http://127.0.0.1:9472\"\n\
                 credential = \"c\"\n{advertised}"
            ),
            |error| matches!(error, ConfigError::Syntax { .. }),
        ),
        (
            format!(
                "{with_key}{}{advertised}",
                relay_at(2, "https://push.example", "http://push.example")
            ),
            |error| matches!(error, ConfigError::PublicRelayUrl),
        ),
        (
            format!("{with_key}{}", relay(2, "http://192.168.1.10:9472")),
            |error| matches!(error, ConfigError::RelayUrl),
        ),
    ];
    for (text, expected) in cases {
        let refused = ServerConfig::parse(&text).err();
        assert!(
            refused.as_ref().is_some_and(expected),
            "{text}\n{refused:?}"
        );
    }
    Ok(())
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
    assert_eq!(defaults.map_host, None);

    let config = ServerConfig::parse(&format!(
        "server_operator = \"Example Ltd\"\nregistration_codes = [\"0123456789abcdef0123456789abcdef\", \"friends-of-the-climbing-crew-2026\"]\n\
         map_host = \"https://maps.example\"\n\
         ohttp_relay_urls = [\"https://relay.example/ohttp\"]\n\
         delivery_db = \"/tmp/g.sqlite\"\nquota_db = \"/tmp/q.sqlite\"\n\
         push_relay_ca = \"/tmp/ca.pem\"\n{}{}key_config_hash = \"{}\"\noperator = \"Relay Ltd\"\n{}",
        minimal(&key(1, SECRET_A)),
        relay(3, "https://push.example"),
        pingers_proto::KeyConfigHash::from_bytes([0xab; 32]).to_pin(),
        relay(4, "https://other.example"),
    ))?;
    assert_eq!(config.server_operator, "Example Ltd");
    assert_eq!(
        config
            .registration_codes
            .iter()
            .map(pingers_proto::SecretText::expose)
            .collect::<Vec<_>>(),
        [
            "0123456789abcdef0123456789abcdef",
            "friends-of-the-climbing-crew-2026"
        ]
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
    // Devices are told the public URL, not the one this server calls.
    assert_eq!(advertised.url.as_str(), "https://public.example");
    assert_eq!(advertised.key_config_hash.as_bytes(), &[0xab; 32]);
    assert_eq!(advertised.operator, "Relay Ltd");
    assert_eq!(
        config
            .map_host
            .as_ref()
            .map(pingers_proto::MapHostUrl::as_str),
        Some("https://maps.example")
    );
    // Without map_host_gateway, map downloads through the relay go to the map host itself.
    assert_eq!(
        config
            .map_host_gateway
            .as_ref()
            .map(super::MapHostGateway::origin),
        Some("https://maps.example")
    );
    assert_eq!(defaults.map_host_gateway, None);
    Ok(())
}

#[test]
fn the_map_host_gateway_is_the_map_host_unless_named() -> Result<(), ConfigError> {
    use super::MapHostGateway;
    let host = Some("https://maps.example/");
    assert_eq!(MapHostGateway::from_settings(None, None)?, None);
    assert_eq!(
        MapHostGateway::from_settings(host, None)?
            .as_ref()
            .map(MapHostGateway::origin),
        Some("https://maps.example")
    );
    // A map host on this machine is reached over loopback, never by host name.
    for gateway in [
        "http://127.0.0.1:9474",
        "http://[::1]:9474/",
        "https://gw.example",
    ] {
        assert_eq!(
            MapHostGateway::from_settings(host, Some(gateway.to_owned()))?
                .as_ref()
                .map(MapHostGateway::origin),
            Some(gateway.trim_end_matches('/')),
            "{gateway}"
        );
    }
    for gateway in [
        "http://localhost:9474",
        "http://10.0.0.2",
        "ftp://maps.example",
    ] {
        assert!(
            matches!(
                MapHostGateway::from_settings(host, Some(gateway.to_owned())),
                Err(ConfigError::MapHostGateway)
            ),
            "{gateway}"
        );
    }
    // A gateway with no map host to advertise would forward for a host no device knows.
    assert!(matches!(
        MapHostGateway::from_settings(None, Some("https://gw.example".to_owned())),
        Err(ConfigError::MapHostGateway)
    ));
    Ok(())
}

#[test]
fn bad_identity_fields_are_refused() {
    let with_key = minimal(&key(1, SECRET_A));
    let http_server = with_key.replace("https://pingers.test", "http://pingers.test");
    let cases: [(String, Expected); 9] = [
        (http_server, |error| matches!(error, ConfigError::ServerId)),
        (
            format!("map_host = \"http://maps.example\"\n{with_key}"),
            |error| matches!(error, ConfigError::MapHostUrl),
        ),
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
        // Learned from the relay, or given: not both, and either way with an operator.
        (
            format!(
                "{with_key}{}key_config_hash = \"{}\"\nkey_config_from_relay = true\noperator = \"x\"\n",
                relay(1, "https://a"),
                pingers_proto::KeyConfigHash::from_bytes([1; 32]).to_pin()
            ),
            |error| matches!(error, ConfigError::RelayInfo),
        ),
        (
            format!(
                "{with_key}{}key_config_from_relay = true\n",
                relay(1, "https://a")
            ),
            |error| matches!(error, ConfigError::RelayInfo),
        ),
        // The hash as hex is not a pin: there is one text form.
        (
            format!(
                "{with_key}{}key_config_hash = \"{}\"\noperator = \"x\"\n",
                relay(1, "https://a"),
                "ab".repeat(32)
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

#[test]
fn a_relay_whose_key_is_learned_is_listed_apart() -> TestResult {
    let config = ServerConfig::parse(&format!(
        "{}{}key_config_from_relay = true\noperator = \"Relay Ltd\"\n",
        minimal(&key(1, SECRET_A)),
        relay(3, "http://127.0.0.1:9472"),
    ))?;
    assert!(config.advertised_push_relays.is_empty());
    assert_eq!(
        config.learned_push_relays,
        [super::LearnedPushRelay {
            relay_id: pingers_proto::RelayId::new(3),
            url: pingers_proto::RelayUrl::new("https://public.example".to_owned())?,
            operator: "Relay Ltd".to_owned(),
        }]
    );
    // Off is the same as leaving it out.
    let off = ServerConfig::parse(&format!(
        "{}{}key_config_from_relay = false\n",
        minimal(&key(1, SECRET_A)),
        relay(3, "http://127.0.0.1:9472"),
    ))?;
    assert!(off.learned_push_relays.is_empty() && off.advertised_push_relays.is_empty());
    Ok(())
}

/// A registration code needs 128 bits of entropy: 32 hex digits, or 22 characters that use
/// the rest of `A-Z a-z 0-9 - _`; nothing shorter, longer than 128, or with other characters.
#[test]
fn registration_codes_need_sixteen_bytes_of_entropy() -> Result<(), ConfigError> {
    let with_code = |code: &str| {
        ServerConfig::parse(&format!(
            "registration_codes = [\"{code}\"]\n{}",
            minimal(&key(1, SECRET_A))
        ))
    };
    for strong in [
        "0123456789abcdef0123456789abcdef".to_owned(),
        "friends-of-the-crew-2026".to_owned(),
        "a".repeat(128),
    ] {
        with_code(&strong)?;
    }
    for weak in [
        "dev-registration-code".to_owned(),
        "0123456789abcdef0123456789abcde".to_owned(),
        "friends".to_owned(),
        "a".repeat(129),
        "spaces are not allowed in a code".to_owned(),
    ] {
        assert!(
            matches!(with_code(&weak), Err(ConfigError::WeakRegistrationCode)),
            "{weak}"
        );
    }
    Ok(())
}
