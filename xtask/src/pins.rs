// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask pins <server.toml> <push-relay.toml>`: the two build variables the app pins
//! its keys with, from the deployed configurations:
//!
//! ```text
//! PINGERS_GATEWAY_KEY_PINS=<pin>,<pin>
//! PINGERS_PUSH_RELAY_KEY_PINS=<pin>,<pin>
//! ```
//!
//! A pin is `KeyConfigHash::to_pin` from pingers-proto: unpadded base64url of the SHA-256 of
//! the key's RFC 9458 key config, the one text form `pingers-push-relay pins` (and
//! `pingers-server pins`) print one per line. Gateway pins follow `[[ohttp.keys]]` in file
//! order; push relay pins follow `[[keys]]` by `activated_at`, as `pingers-push-relay pins`
//! does. Only the key fields are read, everything else in the files is ignored, and only
//! public values are printed. Errors name the file and field, never a value.
//!
//! The private keys are turned into key configs with the same library calls the server and
//! the relay use (pingers-ohttp's `GatewayKey`, pingers-crypto's `PushRelayPrivateKey`), so
//! this needs neither binary built. The tests check the push relay half against
//! `pingers_push_relay::command::pin_set`.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};
use pingers_crypto::PushRelayPrivateKey;
use pingers_ohttp::GatewayKey;
use pingers_proto::{KeyConfigHash, PushRelayKeyConfig, PushRelayKeyId};
use toml::{Table, Value};
use zeroize::{Zeroize as _, Zeroizing};

/// The app build's variable for the OHTTP gateway's key pins.
pub(crate) const GATEWAY_VARIABLE: &str = "PINGERS_GATEWAY_KEY_PINS";
/// The app build's variable for the push relay's key pins.
pub(crate) const PUSH_RELAY_VARIABLE: &str = "PINGERS_PUSH_RELAY_KEY_PINS";

/// Reads both files and prints the two variable lines.
pub(crate) fn run(server: &Path, push_relay: &Path) -> Result<String> {
    let gateway = gateway_pins(&read(server)?).with_context(|| server.display().to_string())?;
    let relay =
        push_relay_pins(&read(push_relay)?).with_context(|| push_relay.display().to_string())?;
    Ok(variables(&gateway, &relay))
}

fn read(path: &Path) -> Result<Zeroizing<String>> {
    std::fs::read_to_string(path)
        .map(Zeroizing::new)
        .with_context(|| format!("cannot read {}", path.display()))
}

/// The output: one `NAME=pin,pin` line per variable.
pub(crate) fn variables(gateway: &[KeyConfigHash], push_relay: &[KeyConfigHash]) -> String {
    let mut output = String::new();
    for (name, pins) in [
        (GATEWAY_VARIABLE, gateway),
        (PUSH_RELAY_VARIABLE, push_relay),
    ] {
        let joined: Vec<String> = pins.iter().map(KeyConfigHash::to_pin).collect();
        // Writing to a String cannot fail.
        let _infallible = writeln!(output, "{name}={}", joined.join(","));
    }
    output
}

/// The pins of a server configuration's `[[ohttp.keys]]`, in file order.
pub(crate) fn gateway_pins(text: &str) -> Result<Vec<KeyConfigHash>> {
    let mut table = parse(text)?;
    let keys = table
        .get_mut("ohttp")
        .and_then(Value::as_table_mut)
        .and_then(|ohttp| ohttp.get_mut("keys"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("no [[ohttp.keys]]"))?;
    let mut pins = Vec::with_capacity(keys.len());
    for key in keys {
        let key = key
            .as_table_mut()
            .context("ohttp.keys entry is not a table")?;
        let key_id = key_id(key, "ohttp.keys.key_id")?;
        let secret = take_hex_32(key, "secret", "ohttp.keys.secret")?;
        let gateway = GatewayKey::from_bytes(key_id, secret)
            .map_err(|_error| anyhow!("ohttp key {key_id} is not a valid X25519 key"))?;
        pins.push(gateway.config().hash());
    }
    if pins.is_empty() {
        bail!("no [[ohttp.keys]]");
    }
    Ok(pins)
}

/// The pins of a push relay configuration's `[[keys]]`, earliest `activated_at` first.
pub(crate) fn push_relay_pins(text: &str) -> Result<Vec<KeyConfigHash>> {
    struct Scheduled {
        activated_at: i64,
        pin: KeyConfigHash,
    }
    let mut table = parse(text)?;
    let keys = table
        .get_mut("keys")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("no [[keys]]"))?;
    let mut scheduled = Vec::with_capacity(keys.len());
    for key in keys {
        let key = key.as_table_mut().context("keys entry is not a table")?;
        let key_id = key_id(key, "keys.key_id")?;
        let activated_at = key
            .get("activated_at")
            .and_then(Value::as_integer)
            .context("keys.activated_at is missing or not an integer")?;
        let private_key =
            PushRelayPrivateKey::from_bytes(take_hex_32(key, "private_key", "keys.private_key")?);
        let public_key = private_key
            .public_key(key_id)
            .map_err(|_error| anyhow!("relay key {key_id} is not a valid X25519 key"))?;
        let config = PushRelayKeyConfig {
            key_id: PushRelayKeyId::new(key_id),
            public_key: *public_key.public_key(),
        };
        scheduled.push(Scheduled {
            activated_at,
            pin: pingers_crypto::key_config_hash(&config.to_bytes()),
        });
    }
    if scheduled.is_empty() {
        bail!("no [[keys]]");
    }
    scheduled.sort_by_key(|key| key.activated_at);
    Ok(scheduled.into_iter().map(|key| key.pin).collect())
}

/// Parses TOML without keeping the parser's message, which can quote a line holding a key.
fn parse(text: &str) -> Result<Table> {
    toml::from_str(text).map_err(|error| {
        let line = error.span().map_or(0, |span| {
            text.bytes()
                .take(span.start)
                .filter(|byte| *byte == b'\n')
                .count()
                .saturating_add(1)
        });
        anyhow!("not valid TOML (line {line})")
    })
}

fn key_id(key: &Table, field: &str) -> Result<u8> {
    key.get("key_id")
        .and_then(Value::as_integer)
        .and_then(|id| u8::try_from(id).ok())
        .with_context(|| format!("{field} is missing or not 0 to 255"))
}

/// Takes a 64-hex-digit field out of `key`, wiping the text.
fn take_hex_32(key: &mut Table, name: &str, field: &str) -> Result<Zeroizing<[u8; 32]>> {
    let Some(Value::String(mut text)) = key.remove(name) else {
        bail!("{field} is missing or not a string");
    };
    let mut bytes = Zeroizing::new([0_u8; 32]);
    let decoded = hex::decode_to_slice(&text, bytes.as_mut_slice());
    text.zeroize();
    decoded.map_err(|_error| anyhow!("{field} must be 64 hex digits"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    /// RFC 7748 §6.1 Alice's private key.
    const ALICE: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
    const OTHER: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn relay_file(first_activation: u64, second_activation: u64) -> String {
        format!(
            "listen = \"127.0.0.1:0\"\noperator = \"Test\"\nhash_key = \"{OTHER}\"\n\
             [[keys]]\nkey_id = 1\nprivate_key = \"{ALICE}\"\nactivated_at = {first_activation}\n\
             retired_at = 0\n\
             [[keys]]\nkey_id = 2\nprivate_key = \"{OTHER}\"\nactivated_at = {second_activation}\n\
             [[servers]]\nserver_id = \"local\"\ncredential = \"{OTHER}\"\npushes_per_minute = 1\n\
             [unifiedpush]\n"
        )
    }

    #[test]
    fn gateway_pins_match_the_servers_own_pins_subcommand() -> Result<()> {
        // Two slots, the second served: every key is pinned, in file order, either way.
        let text = format!(
            "listen = \"127.0.0.1:0\"\nserver_id = \"https://pingers.test\"\n\
             [ohttp]\nlisten = \"127.0.0.1:0\"\nrelays = [\"127.0.0.1\"]\nserved_key_id = 1\n\
             [[ohttp.keys]]\nkey_id = 2\nsecret = \"{OTHER}\"\n\
             [[ohttp.keys]]\nkey_id = 1\nsecret = \"{ALICE}\"\n"
        );
        let config = pingers_server::ServerConfig::parse(&text)?;
        let expected = pingers_server::pin_set(&config.ohttp_keys);
        assert_eq!(gateway_pins(&text)?, expected);
        assert_eq!(expected.len(), 2);
        // `pingers-server pins` lines, joined with commas, are the variable's value.
        let server_lines: Vec<String> = pingers_server::pins_output(&config.ohttp_keys)
            .lines()
            .map(str::to_owned)
            .collect();
        let output = variables(&gateway_pins(&text)?, &[]);
        let gateway_line = output.lines().next().unwrap_or_default();
        assert_eq!(
            gateway_line,
            format!("{GATEWAY_VARIABLE}={}", server_lines.join(","))
        );
        Ok(())
    }

    #[test]
    fn push_relay_pins_match_the_relays_own_pins_subcommand() -> Result<()> {
        // Listed out of activation order: both must sort by activated_at.
        let text = relay_file(4_102_444_800, 1000);
        let config = pingers_push_relay::config::RelayConfig::parse(&text)?;
        let expected = pingers_push_relay::command::pin_set(&config);
        assert_eq!(push_relay_pins(&text)?, expected);
        assert_eq!(expected.len(), 2);
        Ok(())
    }

    #[test]
    fn gateway_pins_follow_file_order_and_match_the_gateway_key() -> Result<()> {
        let text = format!(
            "listen = \"0.0.0.0:1\"\nserver_id = \"https://pingers.test\"\n\
             [ohttp]\nrelays = [\"127.0.0.1\"]\n\
             [[ohttp.keys]]\nkey_id = 2\nsecret = \"{OTHER}\"\n\
             [[ohttp.keys]]\nkey_id = 1\nsecret = \"{ALICE}\"\n"
        );
        let expected = [(2, OTHER), (1, ALICE)]
            .into_iter()
            .map(|(id, secret)| {
                let mut bytes = Zeroizing::new([0_u8; 32]);
                hex::decode_to_slice(secret, bytes.as_mut_slice())?;
                Ok(GatewayKey::from_bytes(id, bytes)
                    .map_err(|_error| anyhow!("bad key"))?
                    .config()
                    .hash())
            })
            .collect::<Result<Vec<_>>>()?;
        assert_eq!(gateway_pins(&text)?, expected);
        Ok(())
    }

    #[test]
    fn the_output_is_two_variable_lines_of_comma_separated_pins() {
        let a = KeyConfigHash::from_bytes([0xff; 32]);
        let b = KeyConfigHash::from_bytes([0; 32]);
        assert_eq!(
            variables(&[a, b], &[b]),
            format!(
                "PINGERS_GATEWAY_KEY_PINS={},{}\nPINGERS_PUSH_RELAY_KEY_PINS={}\n",
                a.to_pin(),
                b.to_pin(),
                b.to_pin()
            )
        );
    }

    #[test]
    fn errors_name_the_field_and_never_the_value() {
        let short = relay_file(0, 1).replace(ALICE, "77076d0a");
        let error = push_relay_pins(&short).map(|_pins| ()).err();
        let message = error.map(|error| format!("{error:#}")).unwrap_or_default();
        assert!(message.contains("keys.private_key"), "{message}");
        assert!(!message.contains("77076d0a"), "{message}");
        let broken = format!("hash_key = \"{ALICE}\n");
        let message = push_relay_pins(&broken)
            .err()
            .map(|error| format!("{error:#}"))
            .unwrap_or_default();
        assert!(message.contains("line 1"), "{message}");
        assert!(!message.contains(ALICE), "{message}");
        assert!(gateway_pins("listen = \"x\"\n").is_err());
    }
}
