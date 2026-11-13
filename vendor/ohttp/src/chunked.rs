// Pingers addition, replacing upstream's `stream.rs`; see ../README.md.
//
// Chunk encryption for Chunked Oblivious HTTP Messages (draft-ietf-ohai-chunked-ohttp-08),
// without I/O. Callers do the framing: each non-final chunk is written as
// `varint(len(sealed)) || sealed` and the final chunk as `varint(0) || sealed`, running to
// the end of the stream. Keeping I/O out of this module lets the caller inject randomness and
// drive it from any transport.
//
// Differences from upstream's `stream.rs`:
// - the response nonce is `max(Nn, Nk)` bytes for every AEAD (upstream used a fixed 16-byte
//   buffer, which panics with ChaCha20-Poly1305 where the nonce is 32 bytes; ohttp issue #102);
// - a non-final chunk with an empty plaintext is refused when sealing and is an error when
//   opening, as draft-08 section 6 requires (upstream accepted it; ohttp issue #102);
// - randomness comes from the caller.

use rand_core::CryptoRng;

use crate::{
    Error, KeyId, build_info,
    crypto::{Decrypt, Encrypt},
    entropy,
    err::Res,
    export_secret, make_aead, response_nonce,
    rh::{
        SymKey,
        aead::{Aead, Mode},
        hpke::{Config as HpkeConfig, HpkeR, HpkeS, PublicKey},
    },
};

/// The info string for a chunked request.
pub(crate) const INFO_REQUEST: &[u8] = b"message/bhttp chunked request";
/// The exporter label for a chunked response.
const LABEL_RESPONSE: &[u8] = b"message/bhttp chunked response";
/// The largest chunk plaintext upstream permits; callers should enforce it when framing.
pub const MAX_CHUNK_PLAINTEXT: usize = 1 << 14;
const CHUNK_AAD: &[u8] = b"";
const FINAL_CHUNK_AAD: &[u8] = b"final";

fn aad(last: bool) -> &'static [u8] {
    if last { FINAL_CHUNK_AAD } else { CHUNK_AAD }
}

fn seal_chunk(cipher: &mut impl Encrypt, chunk: &[u8], last: bool) -> Res<Vec<u8>> {
    if chunk.is_empty() && !last {
        return Err(Error::EmptyChunk);
    }
    cipher.seal(aad(last), chunk)
}

fn open_chunk(cipher: &mut impl Decrypt, sealed: &[u8], last: bool) -> Res<Vec<u8>> {
    let chunk = cipher.open(aad(last), sealed)?;
    if chunk.is_empty() && !last {
        // draft-08 section 6: "equivalent to a decryption error".
        return Err(Error::EmptyChunk);
    }
    Ok(chunk)
}

/// The client side of one chunked request.
pub struct ChunkedClientRequest {
    hpke: HpkeS,
    header: Vec<u8>,
}

impl ChunkedClientRequest {
    pub(crate) fn new(
        config: HpkeConfig,
        key_id: KeyId,
        pk: &PublicKey,
        rng: &mut impl CryptoRng,
    ) -> Res<Self> {
        let info = build_info(INFO_REQUEST, key_id, config)?;
        let hpke = HpkeS::new(config, pk, &info, rng)?;
        let mut header = info
            .get(INFO_REQUEST.len() + 1..)
            .ok_or(Error::Internal)?
            .to_vec();
        header.extend_from_slice(&hpke.enc()?);
        Ok(Self { hpke, header })
    }

    /// The Chunked Request Header: key id, suite, and the encapsulated key.
    #[must_use]
    pub fn header(&self) -> &[u8] {
        &self.header
    }

    /// Seals one request chunk (without its length prefix).
    pub fn seal(&mut self, chunk: &[u8], last: bool) -> Res<Vec<u8>> {
        seal_chunk(&mut self.hpke, chunk, last)
    }

    /// Test support: seals an empty non-final chunk, which `seal` refuses, so tests can check
    /// that receivers reject it.
    #[cfg(feature = "test-util")]
    pub fn seal_empty_non_final(&mut self) -> Res<Vec<u8>> {
        self.hpke.seal(CHUNK_AAD, &[])
    }

    /// The keys needed to open the response, available before any of it arrives.
    pub fn response(&self) -> Res<ChunkedResponseKeys> {
        let config = self.hpke.config();
        let enc = self.hpke.enc()?;
        Ok(ChunkedResponseKeys {
            config,
            secret: export_secret(&self.hpke, LABEL_RESPONSE, config)?,
            enc,
        })
    }
}

/// What the client needs to open a chunked response once its nonce arrives.
pub struct ChunkedResponseKeys {
    config: HpkeConfig,
    secret: SymKey,
    enc: Vec<u8>,
}

impl ChunkedResponseKeys {
    /// The length of the response nonce that starts the response: max(Nn, Nk).
    #[must_use]
    pub fn nonce_len(&self) -> usize {
        entropy(self.config)
    }

    /// Makes the opener for the response chunks from the response nonce.
    pub fn opener(&self, response_nonce: &[u8]) -> Res<ChunkOpener> {
        if response_nonce.len() != self.nonce_len() {
            return Err(Error::Truncated);
        }
        Ok(ChunkOpener {
            aead: make_aead(
                Mode::Decrypt,
                self.config,
                &self.secret,
                &self.enc,
                response_nonce,
            )?,
        })
    }
}

/// Opens response chunks in order.
pub struct ChunkOpener {
    aead: Aead,
}

impl ChunkOpener {
    /// Opens the next chunk (given without its length prefix).
    pub fn open(&mut self, sealed: &[u8], last: bool) -> Res<Vec<u8>> {
        open_chunk(&mut self.aead, sealed, last)
    }
}

/// The server side of one chunked request.
pub struct ChunkedServerRequest {
    hpke: HpkeR,
    enc: Vec<u8>,
}

impl ChunkedServerRequest {
    pub(crate) fn new(hpke: HpkeR, enc: Vec<u8>) -> Self {
        Self { hpke, enc }
    }

    /// Opens the next request chunk (given without its length prefix).
    pub fn open(&mut self, sealed: &[u8], last: bool) -> Res<Vec<u8>> {
        open_chunk(&mut self.hpke, sealed, last)
    }

    /// Starts the response: draws the response nonce from `rng` and derives the AEAD.
    pub fn response(&self, rng: &mut impl CryptoRng) -> Res<ChunkedServerResponse> {
        let config = self.hpke.config();
        let nonce = response_nonce(config, rng);
        let secret = export_secret(&self.hpke, LABEL_RESPONSE, config)?;
        let aead = make_aead(Mode::Encrypt, config, &secret, &self.enc, &nonce)?;
        Ok(ChunkedServerResponse { nonce, aead })
    }
}

/// Seals response chunks in order.
pub struct ChunkedServerResponse {
    nonce: Vec<u8>,
    aead: Aead,
}

impl ChunkedServerResponse {
    /// The response nonce, which must be sent before any chunk.
    #[must_use]
    pub fn nonce(&self) -> &[u8] {
        &self.nonce
    }

    /// Seals one response chunk (without its length prefix).
    pub fn seal(&mut self, chunk: &[u8], last: bool) -> Res<Vec<u8>> {
        seal_chunk(&mut self.aead, chunk, last)
    }

    /// Test support: seals an empty non-final chunk, which `seal` refuses, so tests can check
    /// that receivers reject it.
    #[cfg(feature = "test-util")]
    pub fn seal_empty_non_final(&mut self) -> Res<Vec<u8>> {
        self.aead.seal(CHUNK_AAD, &[])
    }
}
