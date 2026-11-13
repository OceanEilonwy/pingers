// SPDX-License-Identifier: AGPL-3.0-only
//! The binary's commands besides serving: `keygen ohttp` makes a gateway key, and `pins`
//! prints the public pin set of a configuration's gateway keys for the app build
//! (`PINGERS_GATEWAY_KEY_PINS`). Neither starts the server or opens a database.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;

use pingers_ohttp::GatewayKey;
use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRng, SeedableRng as _};
use zeroize::Zeroizing;

use crate::{OhttpKeys, ServerError};

/// The name of the app build's variable that holds the gateway key pins.
const PINS_VARIABLE: &str = "PINGERS_GATEWAY_KEY_PINS";

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

/// The public pin set of `keys`, as printed by `pingers-server pins`: for each key, in the
/// order configured, its id, whether it is served, its encoded key config and that config's
/// SHA-256 (`KeyConfig::hash`, what the app pins), as `#` comments; then one line
/// `PINGERS_GATEWAY_KEY_PINS=<hash>,<hash>` with every key's hash in lowercase hex. Nothing
/// secret is printed.
#[must_use]
pub fn pins(keys: &OhttpKeys) -> String {
    let served = keys.served().key_id();
    let configs = keys.configs();
    let mut report = String::new();
    let mut hashes = Vec::with_capacity(configs.len());
    for config in &configs {
        let hash = hex::encode(config.hash().as_bytes());
        let role = if config.key_id() == served {
            "served"
        } else {
            "not served"
        };
        // Writing to a String cannot fail.
        let _ = writeln!(
            report,
            "# key_id {} ({role})\n# key_config {}\n# key_config_hash {hash}",
            config.key_id(),
            hex::encode(config.encoded()),
        );
        hashes.push(hash);
    }
    let _ = writeln!(report, "{PINS_VARIABLE}={}", hashes.join(","));
    report
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use rand_chacha::ChaCha20Rng;
    use rand_core::SeedableRng as _;

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
    fn pins_list_every_key_and_mark_the_served_one() -> TestResult {
        let mut rng = ChaCha20Rng::seed_from_u64(6);
        let first = GatewayKey::generate(1, &mut rng)?;
        let second = GatewayKey::generate(2, &mut rng)?;
        let first_config = first.config();
        let second_config = second.config();
        let keys = OhttpKeys::serving(vec![first, second], ServedKey::Id(2))?;
        let report = pins(&keys);
        let first_hash = hex::encode(first_config.hash().as_bytes());
        let second_hash = hex::encode(second_config.hash().as_bytes());
        assert_eq!(
            report,
            format!(
                "# key_id 1 (not served)\n# key_config {}\n# key_config_hash {first_hash}\n\
                 # key_id 2 (served)\n# key_config {}\n# key_config_hash {second_hash}\n\
                 PINGERS_GATEWAY_KEY_PINS={first_hash},{second_hash}\n",
                hex::encode(first_config.encoded()),
                hex::encode(second_config.encoded()),
            )
        );
        Ok(())
    }
}
