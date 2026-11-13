// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use super::*;
use crate::keys::KeyId;

const PRIVATE: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
const PUBLIC: &str = "8520f0098930a754748b7ddcb43ef75c0dbf3a0d26381af4eba4a98eaa9b4e6a";

fn minimal() -> String {
    format!(
        r#"
listen = "127.0.0.1:8080"
operator = "Test Relay Ltd"

[[keys]]
key_id = 1
private_key = "{PRIVATE}"
public_key = "{PUBLIC}"
activated_at = 1000

[[servers]]
server_id = "pingers.example"
credential = "0123456789abcdef0123456789abcdef"
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
    assert_eq!(config.max_ttl, Duration::from_hours(30 * 24));
    assert_eq!(config.token_limit, rate("t", 30)?);
    assert_eq!(config.unifiedpush, Some(EndpointPolicy::PublicHttpsOnly));
    assert_eq!(config.fcm, None);
    assert_eq!(config.operator, "Test Relay Ltd");
    let key = config
        .keys
        .published(UnixSeconds::new(1000))
        .ok_or(ConfigError::NoKeys)?;
    assert_eq!(key.key_id(), KeyId::new(1));
    assert_eq!(hex::encode(key.public_key().public_key()), PUBLIC);
    assert_eq!(hex::encode(key.private_key().expose_bytes()), PRIVATE);
    assert!(
        config
            .servers
            .authenticate(b"0123456789abcdef0123456789abcdef")
            .is_some()
    );
    Ok(())
}

#[test]
fn every_setting_is_read() -> Result<(), ConfigError> {
    let text = format!(
        "logging = \"counts\"\ncounts_interval_s = 60\nupstream_timeout_ms = 1500\n\
         token_pushes_per_minute = 5\nmax_ttl_s = 3600\n{}\
         allow_insecure_local_endpoints = true\n\n[fcm]\nproject_id = \"p-1\"\n\
         access_token_file = \"/run/token\"\nendpoint = \"http://127.0.0.1:9/\"\n",
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
    assert_eq!(config.max_ttl, Duration::from_secs(3600));
    assert_eq!(config.unifiedpush, Some(EndpointPolicy::AllowInsecureLocal));
    assert_eq!(
        config.fcm,
        Some(FcmConfig {
            project_id: "p-1".to_owned(),
            endpoint: Url::parse("http://127.0.0.1:9/").map_err(|_| ConfigError::BadFcmEndpoint)?,
            access_token_file: PathBuf::from("/run/token"),
        })
    );
    Ok(())
}

#[test]
fn fcm_alone_is_enough_and_defaults_to_googles_endpoint() -> Result<(), ConfigError> {
    let text = minimal().replace(
        "[unifiedpush]",
        "[fcm]\nproject_id = \"p\"\naccess_token_file = \"/t\"",
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
        "credential = \"0123456789abcdef0123456789abcdef\"",
        "credential = \"0123456789abcdef0123456789abcdef",
    );
    let error = error_of(&text);
    assert_eq!(error, Some(ConfigError::Syntax { line: 13 }));
    let message = error.map(|error| error.to_string()).unwrap_or_default();
    assert!(!message.contains("0123456789abcdef"));
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
    let bad_public = minimal().replace(PUBLIC, &PUBLIC.replace('8', "g"));
    assert_eq!(
        error_of(&bad_public),
        Some(ConfigError::BadHex {
            field: "keys.public_key",
            bytes: 32
        })
    );
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
        error_of(&minimal().replace(
            "credential = \"0123456789abcdef0123456789abcdef\"",
            "credential = \"short\""
        )),
        Some(ConfigError::WeakCredential {
            server_id: "pingers.example".to_owned()
        })
    );
}

#[test]
fn a_file_without_keys_or_servers_is_refused() {
    let no_keys = "listen = \"127.0.0.1:1\"\noperator = \"x\"\n[[servers]]\nserver_id = \"a\"\n\
         credential = \"0123456789abcdef0123456789abcdef\"\npushes_per_minute = 1\n[unifiedpush]\n";
    assert_eq!(error_of(no_keys), Some(ConfigError::NoKeys));
    let no_servers = format!(
        "listen = \"127.0.0.1:1\"\noperator = \"x\"\n[[keys]]\nkey_id = 1\n\
         private_key = \"{PRIVATE}\"\npublic_key = \"{PUBLIC}\"\nactivated_at = 0\n[unifiedpush]\n"
    );
    assert_eq!(error_of(&no_servers), Some(ConfigError::NoServers));
}

#[test]
fn fcm_settings_are_checked() {
    let fcm = |project: &str, endpoint: &str| {
        minimal().replace(
            "[unifiedpush]",
            &format!(
                "[fcm]\nproject_id = \"{project}\"\naccess_token_file = \"/t\"\nendpoint = \"{endpoint}\""
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
