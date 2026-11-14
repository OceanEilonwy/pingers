// SPDX-License-Identifier: AGPL-3.0-only
//! The command line: serving, and the operator subcommands that print keys, credential
//! hashes and pins. Each subcommand prints only to standard output, and only what its name
//! says.
//!
//! ```sh
//! pingers-push-relay /etc/pingers-push-relay/push-relay.toml          # serve
//! pingers-push-relay pins /etc/pingers-push-relay/push-relay.toml     # one pin per line
//! pingers-push-relay keygen                                          # 64 hex, for hash_key,
//!                                                                    # private_key or credential
//! printf '%s\n%s\n' "$hash_key" "$credential" | pingers-push-relay credential-hash
//! ```
//!
//! A pin is [`KeyConfigHash::to_pin`]: unpadded base64url of the SHA-256 of the key's RFC 9458
//! key config, the same text form `pingers-server pins` prints for the OHTTP gateway. The app's
//! `PINGERS_PUSH_RELAY_KEY_PINS` build variable is the lines of `pins` joined with commas
//! (`cargo xtask pins` does this).

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;

use pingers_crypto::{KeyedHashKey, PushRelayPrivateKey};
use pingers_proto::KeyConfigHash;
use zeroize::Zeroizing;

use crate::config::{RelayConfig, hash_credential, hex_32};
use crate::error::RelayError;
use crate::keys::key_config_hash;

/// The command line forms, printed when the arguments match none of them.
pub const USAGE: &str = "usage: pingers-push-relay <config.toml>\n\
     \x20      pingers-push-relay pins <config.toml>\n\
     \x20      pingers-push-relay keygen\n\
     \x20      pingers-push-relay credential-hash   (hash key and credential on stdin, one per line)";

/// The longest standard input `credential-hash` reads: a hash key line and a credential line
/// are far shorter.
pub const MAX_CREDENTIAL_INPUT: u64 = 4096;

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// `pingers-push-relay <config.toml>`: run the relay.
    Serve {
        /// The configuration file.
        config: PathBuf,
    },
    /// `pingers-push-relay pins <config.toml>`: print the pin of every configured key.
    Pins {
        /// The configuration file.
        config: PathBuf,
    },
    /// `pingers-push-relay keygen`: print 32 fresh random bytes as hex.
    Keygen,
    /// `pingers-push-relay credential-hash`: read the hash key and a credential on standard
    /// input and print the credential's `credential_hash`.
    CredentialHash,
}

impl Command {
    /// Reads the arguments after the program name.
    ///
    /// # Errors
    ///
    /// [`RelayError::Usage`] if they match no form in [`USAGE`].
    pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, RelayError> {
        let mut arguments = arguments.into_iter();
        let first = arguments.next().ok_or(RelayError::Usage)?;
        let second = arguments.next();
        if arguments.next().is_some() {
            return Err(RelayError::Usage);
        }
        match (first.to_str(), second) {
            (Some("pins"), Some(config)) => Ok(Self::Pins {
                config: PathBuf::from(config),
            }),
            (Some("keygen"), None) => Ok(Self::Keygen),
            (Some("credential-hash"), None) => Ok(Self::CredentialHash),
            (Some("pins" | "keygen" | "credential-hash"), _) | (_, Some(_)) => {
                Err(RelayError::Usage)
            }
            (_, None) => Ok(Self::Serve {
                config: PathBuf::from(first),
            }),
        }
    }
}

/// The pin of every configured key, earliest activation first: the served key, the next one
/// (announced before it activates), and any still opening during an overlap.
#[must_use]
pub fn pin_set(config: &RelayConfig) -> Vec<KeyConfigHash> {
    config
        .keys
        .all()
        .map(|key| key_config_hash(&key.key_config()))
        .collect()
}

/// What `pins` prints: [`pin_set`], one [`KeyConfigHash::to_pin`] per line.
#[must_use]
pub fn pins_output(config: &RelayConfig) -> String {
    pin_set(config)
        .iter()
        .fold(String::new(), |mut output, pin| {
            output.push_str(&pin.to_pin());
            output.push('\n');
            output
        })
}

/// What `keygen` prints: a fresh relay private key from `rng` as 64 hex digits and a newline.
/// Any 32 random bytes serve equally as an HPKE `private_key`, a `hash_key` or a server
/// `credential`.
#[must_use]
pub fn keygen_output(rng: &mut impl rand_core::CryptoRng) -> Zeroizing<String> {
    let key = PushRelayPrivateKey::generate(rng);
    let mut output = Zeroizing::new(String::with_capacity(65));
    for byte in key.expose_bytes() {
        // Writing to a String cannot fail.
        let _infallible = write!(output, "{byte:02x}");
    }
    output.push('\n');
    output
}

/// What `credential-hash` prints for its standard input: the hash key (64 hex) on the first
/// line and the credential on the second; surrounding whitespace is ignored. Prints the
/// `credential_hash` as 64 hex digits and a newline.
///
/// # Errors
///
/// [`RelayError::CredentialInput`] unless the input is exactly those two lines, or
/// [`RelayError::Config`] for a hash key that is not 64 hex digits or a credential shorter
/// than 32 characters.
pub fn credential_hash_output(input: &str) -> Result<String, RelayError> {
    let mut lines = input.lines().map(str::trim).filter(|line| !line.is_empty());
    let (Some(hash_key), Some(credential), None) = (lines.next(), lines.next(), lines.next())
    else {
        return Err(RelayError::CredentialInput);
    };
    let hash_key =
        KeyedHashKey::from_bytes(hex_32("hash_key", hash_key).map_err(RelayError::Config)?);
    let hash = hash_credential(&hash_key, credential.as_bytes()).map_err(RelayError::Config)?;
    let mut output = hex::encode(hash.as_bytes());
    output.push('\n');
    Ok(output)
}

#[cfg(test)]
mod tests;
