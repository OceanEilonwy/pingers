// Vendored from ohttp 0.8.0 and patched for Pingers; see ../README.md for every change.
// Pingers patch: upstream's `deny(warnings, clippy::pedantic)` is dropped. Clippy picks up the
// Pingers workspace's clippy.toml here, and vendored code keeps upstream's style.
#![allow(clippy::all, clippy::pedantic)]
#![forbid(unsafe_code)]

pub mod chunked;
mod config;
mod crypto;
mod err;
pub mod hpke;
mod rh;

use std::{
    cmp::max,
    convert::TryFrom,
    io::{Cursor, Read},
    mem::size_of,
};

use byteorder::{NetworkEndian, WriteBytesExt};
use crypto::{Decrypt, Encrypt};
use log::trace;
use rand_core::CryptoRng;

pub use crate::{
    chunked::{
        ChunkOpener, ChunkedClientRequest, ChunkedResponseKeys, ChunkedServerRequest,
        ChunkedServerResponse,
    },
    config::{KeyConfig, SymmetricSuite},
    err::Error,
};
use crate::{
    err::Res,
    hpke::Aead as AeadId,
    rh::{
        SymKey,
        aead::{Aead, Mode, NONCE_LEN},
        hkdf::{Hkdf, KeyMechanism},
        hpke::{Config as HpkeConfig, Exporter, HpkeR, HpkeS, PublicKey},
    },
};

/// The request header is a `KeyId` and 2 each for KEM, KDF, and AEAD identifiers
pub const REQUEST_HEADER_LEN: usize = size_of::<KeyId>() + 6;
const INFO_REQUEST: &[u8] = b"message/bhttp request";
const LABEL_RESPONSE: &[u8] = b"message/bhttp response";
const INFO_KEY: &[u8] = b"key";
const INFO_NONCE: &[u8] = b"nonce";

/// The type of a key identifier.
pub type KeyId = u8;

/// Construct the info parameter we use to initialize an `HpkeS` instance.
fn build_info(label: &[u8], key_id: KeyId, config: HpkeConfig) -> Res<Vec<u8>> {
    let mut info = Vec::with_capacity(label.len() + 1 + REQUEST_HEADER_LEN);
    info.extend_from_slice(label);
    info.push(0);
    info.write_u8(key_id)?;
    info.write_u16::<NetworkEndian>(u16::from(config.kem()))?;
    info.write_u16::<NetworkEndian>(u16::from(config.kdf()))?;
    info.write_u16::<NetworkEndian>(u16::from(config.aead()))?;
    trace!("HPKE info: {}", hex::encode(&info));
    Ok(info)
}

/// This is the sort of information we expect to receive from the receiver.
/// This might not be necessary if we agree on a format.
pub struct ClientRequest {
    key_id: KeyId,
    config: HpkeConfig,
    pk: PublicKey,
}

impl ClientRequest {
    /// Construct a `ClientRequest` from a specific `KeyConfig` instance.
    pub fn from_config(config: &mut KeyConfig) -> Res<Self> {
        // TODO(mt) choose the best config, not just the first.
        let first = *config.symmetric.first().ok_or(Error::Unsupported)?;
        let selected = config.select(first)?;
        Ok(Self {
            key_id: config.key_id,
            config: selected,
            pk: config.pk.clone(),
        })
    }

    /// Reads an encoded configuration and constructs a single use client sender.
    /// See `KeyConfig::decode` for the structure details.
    pub fn from_encoded_config(encoded_config: &[u8]) -> Res<Self> {
        let mut config = KeyConfig::decode(encoded_config)?;
        Self::from_config(&mut config)
    }

    /// Encapsulate a request.  This consumes this object.
    /// This produces a response handler and the bytes of an encapsulated request.
    ///
    /// Pingers patch: the caller supplies the randomness for the HPKE ephemeral key.
    pub fn encapsulate(
        self,
        request: &[u8],
        rng: &mut impl CryptoRng,
    ) -> Res<(Vec<u8>, ClientResponse)> {
        // Build the info, which contains the message header.
        let info = build_info(INFO_REQUEST, self.key_id, self.config)?;
        let mut hpke = HpkeS::new(self.config, &self.pk, &info, rng)?;

        let header = info
            .get(INFO_REQUEST.len() + 1..)
            .ok_or(Error::Internal)?
            .to_vec();

        let mut enc_request = header;
        let enc = hpke.enc()?;
        enc_request.extend_from_slice(&enc);

        let mut ct = hpke.seal(&[], request)?;
        enc_request.append(&mut ct);

        Ok((enc_request, ClientResponse::new(hpke, enc)))
    }

    /// Pingers patch: start a chunked request (draft-ietf-ohai-chunked-ohttp-08) without
    /// any I/O; see [`ChunkedClientRequest`].
    pub fn encapsulate_chunked(self, rng: &mut impl CryptoRng) -> Res<ChunkedClientRequest> {
        ChunkedClientRequest::new(self.config, self.key_id, &self.pk, rng)
    }
}

/// A server can handle multiple requests.
/// It holds a single key pair and can generate a configuration.
/// (A more complex server would have multiple key pairs. This is simple.)
#[derive(Debug, Clone)]
pub struct Server {
    config: KeyConfig,
}

impl Server {
    /// Create a new server configuration.
    ///
    /// Pingers patch: a configuration without a private key is an error, not a panic.
    pub fn new(config: KeyConfig) -> Res<Self> {
        if config.sk.is_none() {
            return Err(Error::InvalidKeyType);
        }
        Ok(Self { config })
    }

    /// Get the configuration that this server uses.
    #[must_use]
    pub fn config(&self) -> &KeyConfig {
        &self.config
    }

    fn decode_request_header(&self, r: &mut Cursor<&[u8]>, label: &[u8]) -> Res<(HpkeR, Vec<u8>)> {
        let hpke_config = self.config.decode_hpke_config(r)?;
        let sym = SymmetricSuite::new(hpke_config.kdf(), hpke_config.aead());
        let config = self.config.select(sym)?;
        let info = build_info(label, self.config.key_id, hpke_config)?;

        let mut enc = vec![0; config.kem().n_enc()];
        r.read_exact(&mut enc)?;

        let sk = self.config.sk.as_ref().ok_or(Error::InvalidKeyType)?;
        Ok((
            HpkeR::new(config, &self.config.pk, sk, &enc, &info)?,
            enc,
        ))
    }

    /// Remove encapsulation on a request.
    pub fn decapsulate(&self, enc_request: &[u8]) -> Res<(Vec<u8>, ServerResponse)> {
        if enc_request.len() <= REQUEST_HEADER_LEN {
            return Err(Error::Truncated);
        }
        let mut r = Cursor::new(enc_request);
        let (mut hpke, enc) = self.decode_request_header(&mut r, INFO_REQUEST)?;

        let position = usize::try_from(r.position())?;
        let ct = enc_request.get(position..).ok_or(Error::Truncated)?;
        let request = hpke.open(&[], ct)?;
        Ok((request, ServerResponse::new(&hpke, enc)?))
    }

    /// Pingers patch: accept the header of a chunked request (key id, suite and the
    /// encapsulated key, nothing more); see [`ChunkedServerRequest`].
    pub fn decapsulate_chunked(&self, header: &[u8]) -> Res<ChunkedServerRequest> {
        let mut r = Cursor::new(header);
        let (hpke, enc) = self.decode_request_header(&mut r, chunked::INFO_REQUEST)?;
        if usize::try_from(r.position())? != header.len() {
            return Err(Error::Format);
        }
        Ok(ChunkedServerRequest::new(hpke, enc))
    }
}

/// The length of the response nonce: max(Nn, Nk).
fn entropy(config: HpkeConfig) -> usize {
    max(config.aead().n_n(), config.aead().n_k())
}

fn export_secret<E: Exporter>(exp: &E, label: &[u8], cfg: HpkeConfig) -> Res<SymKey> {
    exp.export(label, entropy(cfg))
}

fn make_aead(mode: Mode, cfg: HpkeConfig, secret: &SymKey, enc: &[u8], nonce: &[u8]) -> Res<Aead> {
    let mut salt = enc.to_vec();
    salt.extend_from_slice(nonce);

    let hkdf = Hkdf::new(cfg.kdf());
    let prk = hkdf.extract(&salt, secret)?;

    let key = hkdf.expand_key(&prk, INFO_KEY, KeyMechanism::Aead(cfg.aead()))?;
    let iv = hkdf.expand_data(&prk, INFO_NONCE, cfg.aead().n_n())?;
    let nonce_base = <[u8; NONCE_LEN]>::try_from(iv).map_err(|_| Error::Internal)?;

    Aead::new(mode, cfg.aead(), &key, nonce_base)
}

/// Pingers patch: a response nonce of the right length for `config`, from the caller's RNG.
fn response_nonce(config: HpkeConfig, rng: &mut impl CryptoRng) -> Vec<u8> {
    let mut nonce = vec![0; entropy(config)];
    rng.fill_bytes(&mut nonce);
    nonce
}

/// An object for encapsulating responses.
/// The only way to obtain one of these is through `Server::decapsulate()`.
///
/// Pingers patch: the response nonce is drawn when the response is encapsulated, from the
/// caller's RNG, so this holds the exported secret rather than a finished AEAD.
pub struct ServerResponse {
    config: HpkeConfig,
    secret: SymKey,
    enc: Vec<u8>,
}

impl ServerResponse {
    fn new(hpke: &HpkeR, enc: Vec<u8>) -> Res<Self> {
        Ok(Self {
            config: hpke.config(),
            secret: export_secret(hpke, LABEL_RESPONSE, hpke.config())?,
            enc,
        })
    }

    /// Consume this object by encapsulating a response.
    pub fn encapsulate(self, response: &[u8], rng: &mut impl CryptoRng) -> Res<Vec<u8>> {
        let response_nonce = response_nonce(self.config, rng);
        let mut aead = make_aead(
            Mode::Encrypt,
            self.config,
            &self.secret,
            &self.enc,
            &response_nonce,
        )?;
        let mut enc_response = response_nonce;
        let mut ct = aead.seal(&[], response)?;
        enc_response.append(&mut ct);
        Ok(enc_response)
    }
}

impl std::fmt::Debug for ServerResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerResponse")
    }
}

/// An object for decapsulating responses.
/// The only way to obtain one of these is through `ClientRequest::encapsulate()`.
pub struct ClientResponse {
    hpke: HpkeS,
    enc: Vec<u8>,
}

impl ClientResponse {
    /// Private method for constructing one of these.
    /// Doesn't do anything because we don't have the nonce yet, so
    /// the work that can be done is limited.
    fn new(hpke: HpkeS, enc: Vec<u8>) -> Self {
        Self { hpke, enc }
    }

    /// Consume this object by decapsulating a response.
    pub fn decapsulate(self, enc_response: &[u8]) -> Res<Vec<u8>> {
        let mid = entropy(self.hpke.config());
        if mid >= enc_response.len() {
            return Err(Error::Truncated);
        }
        let (response_nonce, ct) = enc_response.split_at(mid);
        let mut aead = make_aead(
            Mode::Decrypt,
            self.hpke.config(),
            &export_secret(&self.hpke, LABEL_RESPONSE, self.hpke.config())?,
            &self.enc,
            response_nonce,
        )?;
        aead.open(&[], ct) // 0 is the sequence number
    }
}
