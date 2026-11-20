// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use super::*;
use crate::error::ServiceAccountError;
use pingers_proto::PushRelayKeyId;

const PRIVATE: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
/// The public key of [`PRIVATE`] (RFC 7748 §6.1), which the relay derives.
const PUBLIC: &str = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";
const HASH_KEY: &str = "4242424242424242424242424242424242424242424242424242424242424242";
/// `credential_hash` of `0123456789abcdef0123456789abcdef` under [`HASH_KEY`].
const H0: &str = "11e91a598f0cdd95086a51f58961207c05755742f65f5e2a4885bbe8721c3985";

fn minimal() -> String {
    format!(
        r#"
listen = "127.0.0.1:8080"
operator = "Test Relay Ltd"
hash_key = "4242424242424242424242424242424242424242424242424242424242424242"

[[keys]]
key_id = 1
private_key = "{PRIVATE}"
activated_at = 1000

[[servers]]
server_id = "pingers.example"
credential_hash = "11e91a598f0cdd95086a51f58961207c05755742f65f5e2a4885bbe8721c3985"
pushes_per_minute = 600

[unifiedpush]
"#
    )
}

fn with(extra: &str) -> String {
    // Top-level keys must come before the first table.
    format!("{extra}\n{}", minimal())
}

fn error_of(text: &str) -> Option<ConfigError> {
    RelayConfig::parse(text).err()
}

#[test]
fn a_minimal_file_gets_private_defaults() -> Result<(), ConfigError> {
    let config = RelayConfig::parse(&minimal())?;
    assert_eq!(config.logging, Logging::Off);
    assert_eq!(config.upstream_timeout, Duration::from_secs(10));
    assert_eq!(config.max_ttl.duration(), Duration::from_hours(30 * 24));
    assert_eq!(config.token_limit, rate("t", 30)?);
    assert_eq!(config.unifiedpush, Some(EndpointPolicy::PublicHttpsOnly));
    assert!(config.fcm.is_none());
    assert_eq!(config.operator, "Test Relay Ltd");
    // Servers on other hosts may push through the TLS front in front of a relay.
    assert_eq!(config.push_senders, crate::http::PushSenders::Anywhere);
    let key = config
        .keys
        .published(UnixSeconds::new(1000))
        .ok_or(ConfigError::NoKeys)?;
    assert_eq!(key.key_id(), PushRelayKeyId::new(1));
    assert_eq!(hex::encode(key.public_key().public_key()), PUBLIC);
    assert_eq!(hex::encode(key.private_key().expose_bytes()), PRIVATE);
    assert!(
        config
            .servers
            .authenticate(b"0123456789abcdef0123456789abcdef")
            .is_ok_and(|server| server.is_some())
    );
    Ok(())
}

#[test]
fn push_direct_only_refuses_forwarded_pushes() -> Result<(), ConfigError> {
    let config = RelayConfig::parse(&with("push_direct_only = true"))?;
    assert_eq!(config.push_senders, crate::http::PushSenders::DirectOnly);
    let config = RelayConfig::parse(&with("push_direct_only = false"))?;
    assert_eq!(config.push_senders, crate::http::PushSenders::Anywhere);
    Ok(())
}

#[test]
fn counts_logging_defaults_to_hourly() -> Result<(), ConfigError> {
    let config = RelayConfig::parse(&with("logging = \"counts\""))?;
    assert_eq!(
        config.logging,
        Logging::Counts {
            interval: Duration::from_hours(1)
        }
    );
    Ok(())
}

#[test]
fn every_setting_is_read() -> Result<(), ConfigError> {
    let text = format!(
        "logging = \"counts\"\ncounts_interval_s = 60\nupstream_timeout_ms = 1500\n\
         token_pushes_per_minute = 5\nmax_ttl_s = 3600\n{}\
         allow_insecure_local_endpoints = true\n\n[fcm]\nproject_id = \"p-1\"\n\
         service_account_file = \"/run/sa.json\"\nendpoint = \"http://127.0.0.1:9/\"\n",
        minimal()
    );
    let config = RelayConfig::parse(&text)?;
    assert_eq!(
        config.logging,
        Logging::Counts {
            interval: Duration::from_secs(60)
        }
    );
    assert_eq!(config.upstream_timeout, Duration::from_millis(1500));
    assert_eq!(config.token_limit, rate("t", 5)?);
    assert_eq!(config.max_ttl.duration(), Duration::from_secs(3600));
    assert_eq!(config.unifiedpush, Some(EndpointPolicy::AllowInsecureLocal));
    let fcm = config.fcm.ok_or(ConfigError::NoPlatform)?;
    assert_eq!(fcm.project_id, "p-1");
    assert_eq!(
        fcm.endpoint,
        Url::parse("http://127.0.0.1:9/").map_err(|_| ConfigError::BadFcmEndpoint)?
    );
    assert!(matches!(
        fcm.service_account,
        ServiceAccountSource::File(path) if path.as_os_str() == "/run/sa.json"
    ));
    Ok(())
}

/// [`minimal`] with its server entry's `credential_hash` line replaced by `line`.
fn with_server_secret(line: &str) -> String {
    minimal().replace(&format!("credential_hash = \"{H0}\""), line)
}

#[test]
fn a_plain_credential_is_hashed_at_load() -> Result<(), ConfigError> {
    let config = RelayConfig::parse(&with_server_secret(
        "credential = \"0123456789abcdef0123456789abcdef\"",
    ))?;
    assert!(
        config
            .servers
            .authenticate(b"0123456789abcdef0123456789abcdef")
            .is_ok_and(|server| server.is_some())
    );
    assert_eq!(
        hash_credential(
            &KeyedHashKey::from_bytes(hex_32("hash_key", HASH_KEY)?),
            b"0123456789abcdef0123456789abcdef"
        )
        .map(|hash| hex::encode(hash.as_bytes()))?,
        H0
    );
    Ok(())
}

#[test]
fn a_server_needs_exactly_one_of_credential_and_hash() {
    let both = with_server_secret(&format!(
        "credential_hash = \"{H0}\"\ncredential = \"0123456789abcdef0123456789abcdef\""
    ));
    let neither = with_server_secret("");
    for text in [both, neither] {
        assert_eq!(
            error_of(&text),
            Some(ConfigError::CredentialSource {
                server_id: "pingers.example".to_owned()
            })
        );
    }
}

#[test]
fn a_plain_credential_too_short_to_authenticate_is_refused() {
    let error = error_of(&with_server_secret(
        "credential = \"0123456789abcdef0123456789abcde\"",
    ));
    assert_eq!(error, Some(ConfigError::CredentialTooShort));
    let message = error.map(|error| error.to_string()).unwrap_or_default();
    assert!(!message.contains("0123456789abcdef"));
}

#[test]
fn retired_at_zero_means_not_scheduled() -> Result<(), ConfigError> {
    // A real retirement with no successor is refused (OverlapTooShort); 0 must not be one.
    let config = RelayConfig::parse(
        &minimal().replace("activated_at = 1000", "activated_at = 1000\nretired_at = 0"),
    )?;
    assert!(config.keys.published(UnixSeconds::new(u64::MAX)).is_some());
    Ok(())
}

#[test]
fn the_next_key_is_configured_before_it_activates() -> Result<(), ConfigError> {
    let next = format!(
        "[[keys]]\nkey_id = 2\nprivate_key = \"{}\"\nactivated_at = 4102444800\nretired_at = 0\n\n[[servers]]",
        "11".repeat(32)
    );
    let config = RelayConfig::parse(&minimal().replacen("[[servers]]", &next, 1))?;
    let now = UnixSeconds::new(2000);
    assert_eq!(
        config.keys.published(now).map(RelayKey::key_id),
        Some(PushRelayKeyId::new(1))
    );
    assert_eq!(
        config.keys.next(now).map(RelayKey::key_id),
        Some(PushRelayKeyId::new(2))
    );
    Ok(())
}

fn with_fcm(service_account: &str) -> String {
    minimal().replace(
        "[unifiedpush]",
        &format!("[fcm]\nproject_id = \"p\"\n{service_account}"),
    )
}

#[test]
fn an_inline_service_account_is_checked_at_load_and_kept_out_of_debug()
-> Result<(), Box<dyn std::error::Error>> {
    let json = crate::fcm_auth::tests::account_json("https://oauth2.googleapis.com/token")?;
    // A TOML literal string holds the JSON (quotes and its \n escapes) unchanged.
    let config = RelayConfig::parse(&with_fcm(&format!("service_account_json = '{json}'")))?;
    let fcm = config.fcm.ok_or(ConfigError::NoPlatform)?;
    let ServiceAccountSource::Inline(inline) = &fcm.service_account else {
        return Err("not inline".into());
    };
    assert_eq!(inline.expose(), json);
    let debug = format!("{fcm:?}");
    assert!(debug.contains("ServiceAccountJson(redacted)"));
    assert!(!debug.contains("PRIVATE KEY"));
    assert!(!debug.contains(crate::fcm_auth::tests::CLIENT_EMAIL));
    Ok(())
}

#[test]
fn an_inline_service_account_survives_toml_string_escaping()
-> Result<(), Box<dyn std::error::Error>> {
    // nodo renders templates by substituting into the parsed TOML tree and writing it back with
    // the toml crate, so the setting arrives in whatever string form that encoder picks.
    let json = crate::fcm_auth::tests::account_json("https://oauth2.googleapis.com/token")?;
    let encoded = toml::Value::String(json.clone()).to_string();
    let config = RelayConfig::parse(&with_fcm(&format!("service_account_json = {encoded}")))?;
    let fcm = config.fcm.ok_or(ConfigError::NoPlatform)?;
    assert!(
        matches!(&fcm.service_account, ServiceAccountSource::Inline(inline) if inline.expose() == json)
    );
    Ok(())
}

#[test]
fn a_bad_inline_service_account_is_refused_by_field() {
    let json = r#"{"client_email":"","private_key":"x","token_uri":"https://oauth2.googleapis.com/token"}"#;
    assert_eq!(
        error_of(&with_fcm(&format!("service_account_json = '{json}'"))),
        Some(ConfigError::ServiceAccount(
            ServiceAccountError::NoClientEmail
        ))
    );
}

#[test]
fn fcm_needs_exactly_one_service_account_source() {
    let both = with_fcm("service_account_file = \"/t\"\nservice_account_json = '{}'");
    for text in [both, with_fcm("")] {
        assert_eq!(error_of(&text), Some(ConfigError::ServiceAccountSource));
    }
}

#[test]
fn fcm_alone_is_enough_and_defaults_to_googles_endpoint() -> Result<(), ConfigError> {
    let text = minimal().replace(
        "[unifiedpush]",
        "[fcm]\nproject_id = \"p\"\nservice_account_file = \"/t\"",
    );
    let config = RelayConfig::parse(&text)?;
    assert_eq!(config.unifiedpush, None);
    assert_eq!(
        config.fcm.map(|fcm| fcm.endpoint.to_string()),
        Some("https://fcm.googleapis.com/".to_owned())
    );
    Ok(())
}

#[test]
fn no_platform_is_refused() {
    assert_eq!(
        error_of(&minimal().replace("[unifiedpush]", "")),
        Some(ConfigError::NoPlatform)
    );
}

#[test]
fn syntax_errors_give_the_line_but_not_the_text() {
    let text = minimal().replace(
        "credential_hash = \"11e91a598f0cdd95086a51f58961207c05755742f65f5e2a4885bbe8721c3985\"",
        "credential_hash = \"11e91a598f0cdd95086a51f58961207c05755742f65f5e2a4885bbe8721c3985",
    );
    let error = error_of(&text);
    assert_eq!(error, Some(ConfigError::Syntax { line: 13 }));
    let message = error.map(|error| error.to_string()).unwrap_or_default();
    assert!(!message.contains("11e91a598f0cdd95"));
}

#[test]
fn unknown_fields_are_refused() {
    assert!(matches!(
        error_of(&with("log_requests = true")),
        Some(ConfigError::Syntax { .. })
    ));
}

#[test]
fn zero_values_are_refused() {
    for (setting, field) in [
        ("upstream_timeout_ms = 0", "upstream_timeout_ms"),
        ("token_pushes_per_minute = 0", "token_pushes_per_minute"),
        ("max_ttl_s = 0", "max_ttl_s"),
        ("max_ttl_s = 4294967296", "max_ttl_s"),
        (
            "logging = \"counts\"\ncounts_interval_s = 0",
            "counts_interval_s",
        ),
    ] {
        assert_eq!(
            error_of(&with(setting)),
            Some(ConfigError::OutOfRange { field }),
            "{setting}"
        );
    }
    assert_eq!(
        error_of(&minimal().replace("pushes_per_minute = 600", "pushes_per_minute = 0")),
        Some(ConfigError::OutOfRange {
            field: "servers.pushes_per_minute"
        })
    );
}

#[test]
fn a_zero_counts_interval_is_ignored_while_logging_is_off() {
    assert!(RelayConfig::parse(&with("counts_interval_s = 0")).is_ok());
}

#[test]
fn bad_key_hex_names_the_field() {
    let short_private = minimal().replace(PRIVATE, &PRIVATE.replace("77", ""));
    assert_eq!(
        error_of(&short_private),
        Some(ConfigError::BadHex {
            field: "keys.private_key",
            bytes: 32
        })
    );
    let with_public = minimal().replace(
        "activated_at = 1000",
        &format!("public_key = \"{PUBLIC}\"\nactivated_at = 1000"),
    );
    assert!(matches!(
        error_of(&with_public),
        Some(ConfigError::Syntax { .. })
    ));
}

#[test]
fn key_and_server_rules_apply() {
    assert_eq!(
        error_of(&minimal().replace(
            "activated_at = 1000",
            "activated_at = 1000\nretired_at = 10"
        )),
        Some(ConfigError::RetiredBeforeActivated { key_id: 1 })
    );
    assert_eq!(
        error_of(&minimal().replace(H0, "short")),
        Some(ConfigError::BadHex {
            field: "servers.credential_hash",
            bytes: 32
        })
    );
    assert_eq!(
        error_of(&minimal().replace(HASH_KEY, "42")),
        Some(ConfigError::BadHex {
            field: "hash_key",
            bytes: 32
        })
    );
}

#[test]
fn a_file_without_keys_or_servers_is_refused() {
    let no_keys = format!(
        "listen = \"127.0.0.1:1\"\noperator = \"x\"\nhash_key = \"{HASH_KEY}\"\n[[servers]]\n\
         server_id = \"a\"\ncredential_hash = \"{H0}\"\npushes_per_minute = 1\n[unifiedpush]\n"
    );
    assert_eq!(error_of(&no_keys), Some(ConfigError::NoKeys));
    let no_servers = format!(
        "listen = \"127.0.0.1:1\"\noperator = \"x\"\nhash_key = \"{HASH_KEY}\"\n[[keys]]\nkey_id = 1\n\
         private_key = \"{PRIVATE}\"\nactivated_at = 0\n[unifiedpush]\n"
    );
    assert_eq!(error_of(&no_servers), Some(ConfigError::NoServers));
}

#[test]
fn fcm_settings_are_checked() {
    let fcm = |project: &str, endpoint: &str| {
        minimal().replace(
            "[unifiedpush]",
            &format!(
                "[fcm]\nproject_id = \"{project}\"\nservice_account_file = \"/t\"\nendpoint = \"{endpoint}\""
            ),
        )
    };
    assert_eq!(
        error_of(&fcm("a/b", "https://fcm.googleapis.com/")),
        Some(ConfigError::OutOfRange {
            field: "fcm.project_id"
        })
    );
    assert_eq!(
        error_of(&fcm("p", "not a url")),
        Some(ConfigError::BadFcmEndpoint)
    );
    assert_eq!(
        error_of(&fcm("p", "ftp://fcm.googleapis.com/")),
        Some(ConfigError::BadFcmEndpoint)
    );
}

#[test]
fn an_inline_service_account_names_the_project_when_project_id_is_left_out()
-> Result<(), Box<dyn std::error::Error>> {
    let json = crate::fcm_auth::tests::account_json("https://oauth2.googleapis.com/token")?;
    let without = minimal().replace(
        "[unifiedpush]",
        &format!("[fcm]\nservice_account_json = '{json}'"),
    );
    let fcm = RelayConfig::parse(&without)?
        .fcm
        .ok_or(ConfigError::NoPlatform)?;
    assert_eq!(fcm.project_id, "pingers-test");
    // A project_id given in the file wins over the key file's own.
    let given = RelayConfig::parse(&with_fcm(&format!("service_account_json = '{json}'")))?
        .fcm
        .ok_or(ConfigError::NoPlatform)?;
    assert_eq!(given.project_id, "p");
    Ok(())
}

#[test]
fn a_project_must_come_from_somewhere() -> Result<(), Box<dyn std::error::Error>> {
    // A key file read at start is not opened while loading, so it needs project_id.
    let file = minimal().replace("[unifiedpush]", "[fcm]\nservice_account_file = \"/t\"");
    assert!(matches!(
        RelayConfig::parse(&file),
        Err(ConfigError::NoFcmProject)
    ));
    // An inline key file without its own project_id needs one too.
    let json = crate::fcm_auth::tests::account_json("https://oauth2.googleapis.com/token")?
        .replace("\"project_id\":\"pingers-test\",", "");
    assert!(!json.contains("project_id"));
    let unnamed = minimal().replace(
        "[unifiedpush]",
        &format!("[fcm]\nservice_account_json = '{json}'"),
    );
    assert!(matches!(
        RelayConfig::parse(&unnamed),
        Err(ConfigError::NoFcmProject)
    ));
    Ok(())
}
