// SPDX-License-Identifier: AGPL-3.0-only
//! Chunked OHTTP (draft-ietf-ohai-chunked-ohttp-08) for the Meetup live channel.
//!
//! Sans-IO: encoders return framed bytes to write; decoders are fed bytes as they arrive (in
//! any split) and return whole opened chunks. Each chunk carries one padded live frame.

use rand_core::CryptoRng;

use crate::stub::{Unimplemented, not_implemented};
use crate::{BinaryRequest, Gateway, KeyConfig, OhttpError};

/// Whether a chunk ends its stream. The final chunk protects against truncation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChunkPosition {
    /// More chunks follow. Must not be empty.
    NonFinal,
    /// The last chunk of the stream. May be empty.
    Final,
}

/// One opened chunk.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenedChunk {
    /// The plaintext.
    pub payload: Vec<u8>,
    /// Whether it was the final chunk.
    pub position: ChunkPosition,
}

impl core::fmt::Debug for OpenedChunk {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("OpenedChunk")
            .field("length", &self.payload.len())
            .field("position", &self.position)
            .finish()
    }
}

/// The start of a chunked exchange: the bytes that open the request body (header and the
/// inner request head) and the client that seals the following chunks.
pub struct ChunkedClientStart {
    /// The first bytes of the `message/ohttp-chunked-req` body.
    pub preamble: Vec<u8>,
    /// Seals outgoing chunks and makes the response decoder.
    pub client: ChunkedClient,
}

/// The client half of one chunked exchange. A reconnect starts a new one (fresh HPKE context,
/// design B.4.9).
pub struct ChunkedClient {
    unimplemented: Unimplemented,
}

impl ChunkedClient {
    /// Starts an exchange carrying `request` (its body is the stream of chunks that follow).
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] if the request head cannot be encoded;
    /// [`OhttpError::Internal`] if HPKE fails.
    pub fn start(
        config: &KeyConfig,
        request: &BinaryRequest,
        rng: &mut impl CryptoRng,
    ) -> Result<ChunkedClientStart, OhttpError> {
        not_implemented((config, request, rng))
    }

    /// Seals one outgoing chunk and returns its framed bytes.
    ///
    /// # Errors
    ///
    /// [`OhttpError::EmptyNonFinalChunk`] for an empty non-final chunk;
    /// [`OhttpError::DataAfterFinalChunk`] after the final chunk;
    /// [`OhttpError::TooManyChunks`] if the counter would overflow.
    pub fn seal_chunk(
        &mut self,
        payload: &[u8],
        position: ChunkPosition,
    ) -> Result<Vec<u8>, OhttpError> {
        let _ = (payload, position);
        match self.unimplemented {}
    }

    /// A decoder for the response stream of this exchange.
    #[must_use]
    pub fn response_decoder(&self) -> ResponseChunkDecoder {
        match self.unimplemented {}
    }
}

/// Opens the gateway's response stream on the client.
pub struct ResponseChunkDecoder {
    unimplemented: Unimplemented,
}

impl ResponseChunkDecoder {
    /// Feeds bytes as they arrive; returns every chunk completed by them.
    ///
    /// # Errors
    ///
    /// [`OhttpError::OpenFailed`], [`OhttpError::EmptyNonFinalChunk`],
    /// [`OhttpError::DataAfterFinalChunk`], [`OhttpError::TooManyChunks`].
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<OpenedChunk>, OhttpError> {
        let _ = bytes;
        match self.unimplemented {}
    }

    /// Signals the end of the stream; opens the final chunk, which runs to the end.
    ///
    /// # Errors
    ///
    /// [`OhttpError::Truncated`] if no final chunk arrived; [`OhttpError::OpenFailed`].
    pub fn finish(self) -> Result<Option<OpenedChunk>, OhttpError> {
        match self.unimplemented {}
    }
}

/// The gateway half of one chunked exchange.
pub struct ChunkedGateway {
    unimplemented: Unimplemented,
}

impl ChunkedGateway {
    /// Accepts the start of a chunked request: decapsulates the header and request head.
    /// Returns the gateway half, which decodes the remaining request chunks and seals
    /// response chunks.
    ///
    /// # Errors
    ///
    /// [`OhttpError::UnknownKeyId`], [`OhttpError::UnsupportedSuite`],
    /// [`OhttpError::OpenFailed`], [`OhttpError::BadMessage`].
    pub fn accept(gateway: &Gateway, preamble: &[u8]) -> Result<Self, OhttpError> {
        not_implemented((gateway, preamble))
    }

    /// The inner request head (method, path, headers; the body is the chunk stream).
    #[must_use]
    pub fn request(&self) -> &BinaryRequest {
        match self.unimplemented {}
    }

    /// A decoder for the rest of the request stream.
    #[must_use]
    pub fn request_decoder(&self) -> RequestChunkDecoder {
        match self.unimplemented {}
    }

    /// The bytes that open the response body (the response nonce).
    #[must_use]
    pub fn response_preamble(&self) -> Vec<u8> {
        match self.unimplemented {}
    }

    /// Seals one response chunk and returns its framed bytes.
    ///
    /// # Errors
    ///
    /// As [`ChunkedClient::seal_chunk`].
    pub fn seal_chunk(
        &mut self,
        payload: &[u8],
        position: ChunkPosition,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<u8>, OhttpError> {
        let _ = (payload, position, rng);
        match self.unimplemented {}
    }
}

/// Opens the client's request stream on the gateway.
pub struct RequestChunkDecoder {
    unimplemented: Unimplemented,
}

impl RequestChunkDecoder {
    /// Feeds bytes as they arrive; returns every chunk completed by them.
    ///
    /// # Errors
    ///
    /// As [`ResponseChunkDecoder::feed`].
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<OpenedChunk>, OhttpError> {
        let _ = bytes;
        match self.unimplemented {}
    }

    /// Signals the end of the stream.
    ///
    /// # Errors
    ///
    /// As [`ResponseChunkDecoder::finish`].
    pub fn finish(self) -> Result<Option<OpenedChunk>, OhttpError> {
        match self.unimplemented {}
    }
}
