// SPDX-License-Identifier: AGPL-3.0-only
//! The binary's commands besides serving: `keygen ohttp` makes a gateway key, and `pins`
//! prints the pins of a configuration's gateway keys for the app build
//! (`PINGERS_GATEWAY_KEY_PINS`). Neither starts the server or opens a database.

use std::ffi::OsString;
use std::path::PathBuf;

use pingers_ohttp::{GatewayKey, KeyConfig};
use pingers_proto::KeyConfigHash;
use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRng, SeedableRng as _};
use zeroize::Zeroizing;

use crate::{OhttpKeys, ServerError};

/// What the binary was asked to do. Exhaustive on purpose: the binary must handle every
/// command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `pingers-server <config.toml>`: run the server.
    Serve {
        /// The configuration file.
        config: PathBuf,
    },
    /// `pingers-server pins <config.toml>`: print the pin set of the configured gateway keys.
    Pins {
        /// The configuration file.
        config: PathBuf,
    },
    /// `pingers-server keygen ohttp`: print a new gateway key secret.
    KeygenOhttp,
}

impl Command {
    /// Reads the arguments after the program name.
    ///
    /// # Errors
    ///
    /// [`ServerError::Usage`] if they are not one of the commands.
    pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, ServerError> {
        let mut arguments = arguments.into_iter();
        let first = arguments.next();
        let second = arguments.next();
        if arguments.next().is_some() {
            return Err(ServerError::Usage);
        }
        match (first, second) {
            (Some(command), Some(kind)) if command == "keygen" && kind == "ohttp" => {
                Ok(Self::KeygenOhttp)
            }
            (Some(command), Some(config)) if command == "pins" => Ok(Self::Pins {
                config: PathBuf::from(config),
            }),
            (Some(config), None) if config != "pins" && config != "keygen" => Ok(Self::Serve {
                config: PathBuf::from(config),
            }),
            _ => Err(ServerError::Usage),
        }
    }
}

/// A new gateway key's secret as 64 lowercase hex digits, the form `[[ohttp.keys]] secret`
/// takes. No `Debug` or `Display`: it is printed once, on purpose, by the binary.
pub struct OhttpSecret {
    hex: Zeroizing<String>,
}

impl OhttpSecret {
    /// The 64 hex digits.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.hex
    }
}

/// A new gateway key secret from `rng`.
///
/// # Errors
///
/// [`ServerError::OhttpKey`] if the bytes drawn are not a usable X25519 key (they always are:
/// every 32 bytes are, once clamped; this checks it rather than assuming it).
pub fn new_ohttp_secret(rng: &mut impl CryptoRng) -> Result<OhttpSecret, ServerError> {
    let mut secret = Zeroizing::new([0_u8; 32]);
    rng.fill_bytes(secret.as_mut_slice());
    let hex = Zeroizing::new(hex::encode(secret.as_slice()));
    // The id does not matter: the key is only loaded to check it, then dropped.
    GatewayKey::from_bytes(1, secret).map_err(ServerError::OhttpKey)?;
    Ok(OhttpSecret { hex })
}

/// A new gateway key secret from the operating system's random number generator.
///
/// # Errors
///
/// [`ServerError::Entropy`] if the generator cannot be read; otherwise as
/// [`new_ohttp_secret`].
pub fn os_ohttp_secret() -> Result<OhttpSecret, ServerError> {
    let seed = crate::os_seed()?;
    new_ohttp_secret(&mut ChaCha20Rng::from_seed(*seed.bytes))
}

/// The pin of every configured gateway key, in the order configured: the served key and the
/// other rotation slot, so that switching `served_key_id` never shows the app an unpinned key.
/// Each is the SHA-256 of the key's RFC 9458 key config (`KeyConfig::hash`).
#[must_use]
pub fn pin_set(keys: &OhttpKeys) -> Vec<KeyConfigHash> {
    keys.configs().iter().map(KeyConfig::hash).collect()
}

/// What `pingers-server pins` prints: [`pin_set`], one [`KeyConfigHash::to_pin`] per line and
/// nothing else. Joined with commas, the lines are the value of the app build's
/// `PINGERS_GATEWAY_KEY_PINS`, as `cargo xtask pins` prints it. Nothing secret is printed.
#[must_use]
pub fn pins_output(keys: &OhttpKeys) -> String {
    pin_set(keys).iter().fold(String::new(), |mut output, pin| {
        output.push_str(&pin.to_pin());
        output.push('\n');
        output
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::{ServedKey, ServerConfig};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn arguments(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn arguments_name_one_command() {
        assert_eq!(
            Command::parse(arguments(&["/etc/server.toml"])).ok(),
            Some(Command::Serve {
                config: PathBuf::from("/etc/server.toml")
            })
        );
        assert_eq!(
            Command::parse(arguments(&["pins", "/etc/server.toml"])).ok(),
            Some(Command::Pins {
                config: PathBuf::from("/etc/server.toml")
            })
        );
        assert_eq!(
            Command::parse(arguments(&["keygen", "ohttp"])).ok(),
            Some(Command::KeygenOhttp)
        );
        for wrong in [
            &[][..],
            &["pins"],
            &["keygen"],
            &["keygen", "push"],
            &["keygen", "ohttp", "extra"],
            &["pins", "a.toml", "b.toml"],
            &["a.toml", "b.toml"],
        ] {
            assert!(
                matches!(Command::parse(arguments(wrong)), Err(ServerError::Usage)),
                "{wrong:?}"
            );
        }
    }

    #[test]
    fn a_new_secret_is_a_fresh_key_the_configuration_accepts() -> TestResult {
        let mut rng = ChaCha20Rng::seed_from_u64(5);
        let first = new_ohttp_secret(&mut rng)?;
        let second = new_ohttp_secret(&mut rng)?;
        assert_eq!(first.expose().len(), 64);
        assert!(
            first
                .expose()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_ne!(first.expose(), second.expose());
        let config = ServerConfig::parse(&format!(
            "listen = \"127.0.0.1:1\"\nserver_id = \"https://pingers.test\"\n\
             [ohttp]\nlisten = \"127.0.0.1:2\"\nrelays = [\"127.0.0.1\"]\n\
             [[ohttp.keys]]\nkey_id = 1\nsecret = \"{}\"\n",
            first.expose()
        ))?;
        assert_eq!(config.ohttp_keys.served().key_id(), 1);
        Ok(())
    }

    #[test]
    fn pins_are_every_key_in_order_one_per_line_whichever_is_served() -> TestResult {
        let mut rng = ChaCha20Rng::seed_from_u64(6);
        let first = GatewayKey::generate(1, &mut rng)?;
        let second = GatewayKey::generate(2, &mut rng)?;
        let first_pin = first.config().hash();
        let second_pin = second.config().hash();
        let keys = OhttpKeys::serving(vec![first, second], ServedKey::Id(2))?;
        assert_eq!(pin_set(&keys), [first_pin, second_pin]);
        assert_eq!(
            pins_output(&keys),
            format!("{}\n{}\n", first_pin.to_pin(), second_pin.to_pin())
        );
        // Each line reads back as the hash it came from.
        for (line, pin) in pins_output(&keys).lines().zip([first_pin, second_pin]) {
            assert_eq!(KeyConfigHash::from_pin(line)?, pin);
        }
        Ok(())
    }
}
