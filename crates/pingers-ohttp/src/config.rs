// SPDX-License-Identifier: AGPL-3.0-only
//! Key configs and pinning (RFC 9458 §3, design B.12).

use pingers_proto::KeyConfigHash;

use crate::OhttpError;

/// DHKEM(X25519, HKDF-SHA256).
pub(crate) const KEM_X25519: u16 = 0x0020;
/// HKDF-SHA256.
pub(crate) const KDF_HKDF_SHA256: u16 = 0x0001;
/// AES-128-GCM, the only AEAD Pingers uses (design B.4.9).
pub(crate) const AEAD_AES_128_GCM: u16 = 0x0001;
/// The length of an X25519 public key and of the encapsulated key in a request.
pub(crate) const X25519_KEY_LENGTH: usize = 32;
/// Key id, KEM, KDF and AEAD: the header that starts every encapsulated request.
pub(crate) const REQUEST_HEADER_LENGTH: usize = 7;

/// A gateway's parsed key config, as served at `GET /v1/ohttp/config` and fetched through the
/// relay. Only configs offering X25519 / HKDF-SHA256 / AES-128-GCM are accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyConfig {
    encoded: Vec<u8>,
    key_id: u8,
    public_key: [u8; X25519_KEY_LENGTH],
}

/// Reads big-endian fields from a byte slice without panicking.
struct Reader<'bytes> {
    rest: &'bytes [u8],
}

impl<'bytes> Reader<'bytes> {
    fn take(&mut self, length: usize) -> Result<&'bytes [u8], OhttpError> {
        let (taken, rest) = self
            .rest
            .split_at_checked(length)
            .ok_or(OhttpError::BadKeyConfig)?;
        self.rest = rest;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, OhttpError> {
        let [byte] = self.take(1)? else {
            return Err(OhttpError::BadKeyConfig);
        };
        Ok(*byte)
    }

    fn u16(&mut self) -> Result<u16, OhttpError> {
        let [high, low] = self.take(2)? else {
            return Err(OhttpError::BadKeyConfig);
        };
        Ok(u16::from_be_bytes([*high, *low]))
    }
}

impl KeyConfig {
    /// Parses an encoded key config.
    ///
    /// The config may list other symmetric suites besides HKDF-SHA256 / AES-128-GCM; they are
    /// ignored, and requests always use AES-128-GCM. The encoding is kept exactly as given, so
    /// [`KeyConfig::hash`] matches what the gateway serves.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadKeyConfig`] if it is malformed or offers no supported suite.
    pub fn decode(encoded: &[u8]) -> Result<Self, OhttpError> {
        let mut reader = Reader { rest: encoded };
        let key_id = reader.u8()?;
        if reader.u16()? != KEM_X25519 {
            return Err(OhttpError::BadKeyConfig);
        }
        let public_key = <[u8; X25519_KEY_LENGTH]>::try_from(reader.take(X25519_KEY_LENGTH)?)
            .map_err(|_| OhttpError::BadKeyConfig)?;
        let suites_length = usize::from(reader.u16()?);
        if suites_length == 0 || suites_length % 4 != 0 {
            return Err(OhttpError::BadKeyConfig);
        }
        let mut suites = Reader {
            rest: reader.take(suites_length)?,
        };
        if !reader.rest.is_empty() {
            return Err(OhttpError::BadKeyConfig);
        }
        let mut supported = false;
        while !suites.rest.is_empty() {
            let kdf = suites.u16()?;
            let aead = suites.u16()?;
            supported |= kdf == KDF_HKDF_SHA256 && aead == AEAD_AES_128_GCM;
        }
        if !supported {
            return Err(OhttpError::BadKeyConfig);
        }
        Ok(Self {
            encoded: encoded.to_vec(),
            key_id,
            public_key,
        })
    }

    /// The key id requests must name.
    #[must_use]
    pub fn key_id(&self) -> u8 {
        self.key_id
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

    /// The upstream client for this config, offering only the pinned suite.
    pub(crate) fn client(&self) -> Result<ohttp::ClientRequest, OhttpError> {
        let mut single_suite = Vec::with_capacity(41);
        single_suite.push(self.key_id);
        single_suite.extend_from_slice(&KEM_X25519.to_be_bytes());
        single_suite.extend_from_slice(&self.public_key);
        single_suite.extend_from_slice(&4_u16.to_be_bytes());
        single_suite.extend_from_slice(&KDF_HKDF_SHA256.to_be_bytes());
        single_suite.extend_from_slice(&AEAD_AES_128_GCM.to_be_bytes());
        ohttp::ClientRequest::from_encoded_config(&single_suite).map_err(|_| OhttpError::Internal)
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
    server: ohttp::Server,
    config: KeyConfig,
}

impl GatewayKey {
    /// Loads a key from the server's configuration.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadKeyConfig`] if the key bytes are invalid.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "contract: the caller hands the secret over and it is zeroized when dropped here"
    )]
    pub fn from_bytes(
        key_id: u8,
        secret: zeroize::Zeroizing<[u8; 32]>,
    ) -> Result<Self, OhttpError> {
        let config =
            ohttp::KeyConfig::import(key_id, ohttp::hpke::Kem::X25519Sha256, suite(), &*secret)
                .map_err(|_| OhttpError::BadKeyConfig)?;
        Self::from_upstream(config, OhttpError::BadKeyConfig)
    }

    /// Generates a new key (for first start and rotation).
    ///
    /// # Errors
    ///
    /// [`OhttpError::Internal`] if key generation fails.
    pub fn generate(key_id: u8, rng: &mut impl rand_core::CryptoRng) -> Result<Self, OhttpError> {
        let config = ohttp::KeyConfig::new(key_id, ohttp::hpke::Kem::X25519Sha256, suite(), rng)
            .map_err(|_| OhttpError::Internal)?;
        Self::from_upstream(config, OhttpError::Internal)
    }

    fn from_upstream(config: ohttp::KeyConfig, failure: OhttpError) -> Result<Self, OhttpError> {
        let encoded = config.encode().map_err(|_| OhttpError::Internal)?;
        let server = ohttp::Server::new(config).map_err(|_| failure)?;
        Ok(Self {
            server,
            config: KeyConfig::decode(&encoded)?,
        })
    }

    /// The public config to serve.
    #[must_use]
    pub fn config(&self) -> KeyConfig {
        self.config.clone()
    }

    pub(crate) fn key_id(&self) -> u8 {
        self.config.key_id
    }

    pub(crate) fn server(&self) -> &ohttp::Server {
        &self.server
    }
}

fn suite() -> Vec<ohttp::SymmetricSuite> {
    vec![ohttp::SymmetricSuite::new(
        ohttp::hpke::Kdf::HkdfSha256,
        ohttp::hpke::Aead::Aes128Gcm,
    )]
}
