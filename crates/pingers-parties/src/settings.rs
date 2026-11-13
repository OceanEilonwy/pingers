// SPDX-License-Identifier: AGPL-3.0-only
//! Per-Party settings: server, relays, post-quantum, location-visibility override.

use pingers_mls::CipherSuitePolicy;
use pingers_proto::{RelayUrl, ServerId};
use pingers_store::LocationVisibility;

use crate::PartiesError;
use crate::codec::{Reader, Writer};

/// Whether the Party's MLS group uses the post-quantum ciphersuite (design B.8). Fixed when
/// the Party is created: changing it means a new group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PostQuantum {
    /// Suite 0x0003 (X25519, ChaCha20-Poly1305, Ed25519), the default.
    Classical,
    /// The X-Wing suite, "quantum-resistant Party".
    QuantumResistant,
}

impl PostQuantum {
    pub(crate) const fn suite(self) -> CipherSuitePolicy {
        match self {
            Self::Classical => CipherSuitePolicy::Classical,
            Self::QuantumResistant => CipherSuitePolicy::QuantumResistant,
        }
    }
}

/// Which OHTTP relays this Party's anonymous requests go through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayChoice {
    /// The relays the server lists in its parameters.
    ServerDefault,
    /// Relays this device chose for the Party, in order of preference.
    Custom(Vec<RelayUrl>),
}

/// A Party's settings as this device holds them. The server and the suite are fixed at
/// creation (or by the invite); the relays and the location-visibility override can change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartySettings {
    /// The server that hosts the Party's group log and mailboxes.
    pub server: ServerId,
    /// The relays its anonymous requests use.
    pub relays: RelayChoice,
    /// The Party's ciphersuite.
    pub post_quantum: PostQuantum,
    /// A per-Party override of the device's location visibility; `None` follows the device
    /// (design B.6.1).
    pub location_visibility: Option<LocationVisibility>,
}

/// The parts of [`PartySettings`] kept in this crate's records (the location-visibility
/// override lives in the store's `party` row).
pub(crate) struct StoredSettings {
    pub(crate) server: ServerId,
    pub(crate) relays: RelayChoice,
    pub(crate) post_quantum: PostQuantum,
}

const VERSION: u8 = 1;

impl StoredSettings {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, PartiesError> {
        let mut writer = Writer::new();
        writer.byte(VERSION);
        writer.variable(self.server.as_str().as_bytes())?;
        writer.byte(match self.post_quantum {
            PostQuantum::Classical => 0,
            PostQuantum::QuantumResistant => 1,
        });
        match &self.relays {
            RelayChoice::ServerDefault => writer.byte(0),
            RelayChoice::Custom(relays) => {
                writer.byte(1);
                writer.count(relays.len())?;
                for relay in relays {
                    writer.variable(relay.as_str().as_bytes())?;
                }
            }
        }
        Ok(writer.finish())
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, PartiesError> {
        let mut reader = Reader::new(bytes, "party settings");
        if reader.byte()? != VERSION {
            return Err(reader.malformed());
        }
        let server = ServerId::new(reader.text()?)?;
        let post_quantum = match reader.byte()? {
            0 => PostQuantum::Classical,
            1 => PostQuantum::QuantumResistant,
            _ => return Err(reader.malformed()),
        };
        let relays = match reader.byte()? {
            0 => RelayChoice::ServerDefault,
            1 => {
                let count = reader.count()?;
                let mut relays = Vec::new();
                for _ in 0..count {
                    relays.push(RelayUrl::new(reader.text()?)?);
                }
                RelayChoice::Custom(relays)
            }
            _ => return Err(reader.malformed()),
        };
        reader.end()?;
        Ok(Self {
            server,
            relays,
            post_quantum,
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn stored_settings_round_trip() -> Result<(), TestError> {
        for (relays, post_quantum) in [
            (RelayChoice::ServerDefault, PostQuantum::QuantumResistant),
            (
                RelayChoice::Custom(vec![
                    RelayUrl::new("https://a.example".into())?,
                    RelayUrl::new("https://b.example".into())?,
                ]),
                PostQuantum::Classical,
            ),
        ] {
            let settings = StoredSettings {
                server: ServerId::new("https://pingers.example".into())?,
                relays: relays.clone(),
                post_quantum,
            };
            let decoded = StoredSettings::decode(&settings.encode()?)?;
            assert_eq!(decoded.server, settings.server);
            assert_eq!(decoded.relays, relays);
            assert_eq!(decoded.post_quantum, post_quantum);
        }
        Ok(())
    }

    #[test]
    fn unknown_versions_and_codes_are_refused() -> Result<(), TestError> {
        let settings = StoredSettings {
            server: ServerId::new("https://pingers.example".into())?,
            relays: RelayChoice::ServerDefault,
            post_quantum: PostQuantum::Classical,
        };
        let bytes = settings.encode()?;
        for index in [0, bytes.len() - 2, bytes.len() - 1] {
            let mut changed = bytes.clone();
            if let Some(byte) = changed.get_mut(index) {
                *byte = 9;
            }
            assert!(StoredSettings::decode(&changed).is_err());
        }
        Ok(())
    }
}
