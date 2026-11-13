// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;

use super::*;
use crate::error::ConfigError;

/// RFC 7748 §6.1 Alice: the private key and the public key the relay derives from it.
const PRIVATE: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
const PUBLIC: &str = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";
const HASH_KEY: &str = "4242424242424242424242424242424242424242424242424242424242424242";
const CREDENTIAL: &str = "0123456789abcdef0123456789abcdef";
/// `credential_hash` of [`CREDENTIAL`] under [`HASH_KEY`] (as in the config tests).
const CREDENTIAL_HASH: &str = "11e91a598f0cdd95086a51f58961207c05755742f65f5e2a4885bbe8721c3985";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn arguments(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

#[test]
fn each_form_of_the_command_line_is_recognised() -> Result<(), RelayError> {
    assert_eq!(
        Command::parse(arguments(&["/etc/relay.toml"]))?,
        Command::Serve {
            config: PathBuf::from("/etc/relay.toml")
        }
    );
    assert_eq!(
        Command::parse(arguments(&["pins", "/etc/relay.toml"]))?,
        Command::Pins {
            config: PathBuf::from("/etc/relay.toml")
        }
    );
    assert_eq!(Command::parse(arguments(&["keygen"]))?, Command::Keygen);
    assert_eq!(
        Command::parse(arguments(&["credential-hash"]))?,
        Command::CredentialHash
    );
    Ok(())
}

#[test]
fn malformed_command_lines_are_usage_errors() {
    for words in [
        &[][..],
        &["pins"],
        &["keygen", "extra"],
        &["credential-hash", "secret-on-the-command-line"],
        &["a.toml", "b.toml"],
        &["pins", "a.toml", "b.toml"],
    ] {
        assert!(
            matches!(Command::parse(arguments(words)), Err(RelayError::Usage)),
            "{words:?}"
        );
    }
}

/// A relay file with key 1 (active) and key 2 (scheduled), listed out of order.
fn two_slot_config() -> Result<RelayConfig, ConfigError> {
    RelayConfig::parse(&format!(
        r#"
listen = "127.0.0.1:0"
operator = "Test Relay Ltd"
hash_key = "{HASH_KEY}"

[[keys]]
key_id = 2
private_key = "{}"
activated_at = 4102444800
retired_at = 0

[[keys]]
key_id = 1
private_key = "{PRIVATE}"
activated_at = 1000
retired_at = 0

[[servers]]
server_id = "local"
credential = "{CREDENTIAL}"
pushes_per_minute = 600

[unifiedpush]
"#,
        "11".repeat(32)
    ))
}

#[test]
fn pins_lists_every_key_by_activation_as_base64url_sha256() -> TestResult {
    let config = two_slot_config()?;
    let output = pins_output(&config);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(output.ends_with('\n'));
    // Key 1, recomputed from its RFC 9458 encoding: id, KEM, public key, suites.
    let mut encoded = vec![1, 0x00, 0x20];
    encoded.extend_from_slice(&hex::decode(PUBLIC)?);
    encoded.extend_from_slice(&[0x00, 0x04, 0x00, 0x01, 0x00, 0x01]);
    let expected = pingers_crypto::key_config_hash(&encoded);
    assert_eq!(lines.first().copied(), Some(expected.to_pin().as_str()));
    assert_ne!(lines.first(), lines.get(1));
    assert_eq!(
        pin_set(&config),
        lines
            .iter()
            .map(|line| KeyConfigHash::from_pin(line))
            .collect::<Result<Vec<_>, _>>()?
    );
    Ok(())
}

#[test]
fn keygen_prints_a_usable_private_key() -> TestResult {
    let output = keygen_output(&mut ChaCha20Rng::from_seed([7; 32]));
    let hex_text = output.strip_suffix('\n').ok_or("no newline")?;
    assert_eq!(hex_text.len(), 64);
    // It loads as a relay key, a hash key and a credential.
    let config = RelayConfig::parse(&format!(
        "listen = \"127.0.0.1:0\"\noperator = \"x\"\nhash_key = \"{hex_text}\"\n\
         [[keys]]\nkey_id = 1\nprivate_key = \"{hex_text}\"\nactivated_at = 0\n\
         [[servers]]\nserver_id = \"a\"\ncredential = \"{hex_text}\"\npushes_per_minute = 1\n\
         [unifiedpush]\n"
    ))?;
    assert_eq!(config.keys.all().count(), 1);
    Ok(())
}

#[test]
fn keygen_output_comes_from_the_generator() {
    let first = keygen_output(&mut ChaCha20Rng::from_seed([7; 32]));
    let again = keygen_output(&mut ChaCha20Rng::from_seed([7; 32]));
    let other = keygen_output(&mut ChaCha20Rng::from_seed([8; 32]));
    assert_eq!(*first, *again);
    assert_ne!(*first, *other);
}

#[test]
fn credential_hash_matches_what_the_relay_stores() -> TestResult {
    assert_eq!(
        credential_hash_output(&format!("{HASH_KEY}\n{CREDENTIAL}\n"))?,
        format!("{CREDENTIAL_HASH}\n")
    );
    // Surrounding whitespace and blank lines (a pasted file, CRLF) do not change it.
    assert_eq!(
        credential_hash_output(&format!("\n  {HASH_KEY} \r\n\r\n{CREDENTIAL}\r\n\n"))?,
        format!("{CREDENTIAL_HASH}\n")
    );
    Ok(())
}

#[test]
fn credential_hash_refuses_bad_input_without_echoing_it() {
    let missing = credential_hash_output(HASH_KEY);
    assert!(matches!(missing, Err(RelayError::CredentialInput)));
    let extra = credential_hash_output(&format!("{HASH_KEY}\n{CREDENTIAL}\nmore\n"));
    assert!(matches!(extra, Err(RelayError::CredentialInput)));
    let short = credential_hash_output(&format!("{HASH_KEY}\nshort-credential\n"));
    assert!(matches!(
        short,
        Err(RelayError::Config(ConfigError::CredentialTooShort))
    ));
    let bad_key = credential_hash_output(&format!("not-hex\n{CREDENTIAL}\n"));
    assert!(matches!(
        &bad_key,
        Err(RelayError::Config(ConfigError::BadHex {
            field: "hash_key",
            ..
        }))
    ));
    let message = bad_key
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(!message.contains(CREDENTIAL));
}
