// SPDX-License-Identifier: AGPL-3.0-only
//! Key configs and pinning (RFC 9458 §3, design B.12).

use pingers_proto::KeyConfigHash;

use crate::OhttpError;
use crate::stub::{Unimplemented, not_implemented};

/// A gateway's parsed key config, as served at `GET /v1/ohttp/config` and fetched through the
/// relay. Only configs offering X25519 / HKDF-SHA256 / AES-128-GCM are accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyConfig {
    encoded: Vec<u8>,
    unimplemented: Unimplemented,
}

impl KeyConfig {
    /// Parses an encoded key config.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadKeyConfig`] if it is malformed or offers no supported suite.
    pub fn decode(encoded: &[u8]) -> Result<Self, OhttpError> {
        not_implemented(encoded)
    }

    /// The key id requests must name.
    #[must_use]
    pub fn key_id(&self) -> u8 {
        match self.unimplemented {}
    }

    /// The config's canonical encoding.
    #[must_use]
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// SHA-256 of the canonical encoding: the value members pin in Party state.
    #[must_use]
    pub fn hash(&self) -> KeyConfigHash {
        pingers_crypto::key_config_hash(&self.encoded)
    }
}

/// Checks a fetched config against the pinned hash.
///
/// # Errors
///
/// [`OhttpError::KeyMismatch`] if they differ.
pub fn check_pin(config: &KeyConfig, pinned: &KeyConfigHash) -> Result<(), OhttpError> {
    if config.hash() == *pinned {
        Ok(())
    } else {
        Err(OhttpError::KeyMismatch)
    }
}

/// The gateway's HPKE private key and its key id. No `Debug`, `Clone` or `Serialize`.
pub struct GatewayKey {
    unimplemented: Unimplemented,
}

impl GatewayKey {
    /// Loads a key from the server's configuration.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadKeyConfig`] if the key bytes are invalid.
    pub fn from_bytes(
        key_id: u8,
        secret: zeroize::Zeroizing<[u8; 32]>,
    ) -> Result<Self, OhttpError> {
        not_implemented((key_id, secret))
    }

    /// Generates a new key (for first start and rotation).
    ///
    /// # Errors
    ///
    /// [`OhttpError::Internal`] if key generation fails.
    pub fn generate(key_id: u8, rng: &mut impl rand_core::CryptoRng) -> Result<Self, OhttpError> {
        not_implemented((key_id, rng))
    }

    /// The public config to serve.
    #[must_use]
    pub fn config(&self) -> KeyConfig {
        match self.unimplemented {}
    }
}
