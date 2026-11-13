// SPDX-License-Identifier: AGPL-3.0-only
//! Chunked OHTTP (draft-ietf-ohai-chunked-ohttp-08) for the Meetup live channel.
//!
//! Sans-IO: encoders return framed bytes to write; decoders are fed bytes as they arrive (in
//! any split) and return whole opened chunks. Each chunk carries one padded live frame.
//!
//! Inner content: the first request chunk is the known-length Binary HTTP request head
//! ([`BinaryRequest`]), and every later chunk in either direction is one opaque frame. The
//! outer format is exactly draft-08's, so any relay that streams works.
//!
//! The HPKE suite is AES-128-GCM only (the gateway refuses any other), and a non-final chunk
//! that opens to nothing is [`OhttpError::EmptyNonFinalChunk`] (draft-08 §6).

use std::cell::Cell;
use std::collections::VecDeque;

use rand_core::CryptoRng;

use crate::config::KeyConfig;
use crate::framing::{ChunkReader, MAX_CHUNK_PLAINTEXT, frame};
use crate::request::ENCAPSULATION_LENGTH;
use crate::{BinaryRequest, Gateway, OhttpError};

/// Whether a chunk ends its stream. The final chunk protects against truncation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChunkPosition {
    /// More chunks follow. Must not be empty.
    NonFinal,
    /// The last chunk of the stream. May be empty.
    Final,
}

impl ChunkPosition {
    const fn is_final(self) -> bool {
        matches!(self, Self::Final)
    }
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

/// Maps an error from opening a chunk.
fn open_error(error: &ohttp::Error) -> OhttpError {
    if matches!(error, ohttp::Error::EmptyChunk) {
        OhttpError::EmptyNonFinalChunk
    } else {
        OhttpError::OpenFailed
    }
}

/// Maps an error from sealing a chunk. Once the inputs are checked, the only way sealing fails
/// is the HPKE or AEAD sequence number running out.
fn seal_error(error: &ohttp::Error) -> OhttpError {
    if matches!(error, ohttp::Error::Hpke(_) | ohttp::Error::Internal) {
        OhttpError::TooManyChunks
    } else {
        OhttpError::Internal
    }
}

/// Checks a chunk before sealing: not after the final one, not empty unless final, not too
/// large.
fn check_outgoing(
    payload: &[u8],
    position: ChunkPosition,
    finished: bool,
) -> Result<(), OhttpError> {
    if finished {
        return Err(OhttpError::DataAfterFinalChunk);
    }
    if payload.is_empty() && !position.is_final() {
        return Err(OhttpError::EmptyNonFinalChunk);
    }
    if payload.len() > MAX_CHUNK_PLAINTEXT {
        return Err(OhttpError::ChunkTooLarge);
    }
    Ok(())
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
    inner: ohttp::ChunkedClientRequest,
    finished: bool,
}

impl ChunkedClient {
    /// Starts an exchange carrying `request` (its body is the stream of chunks that follow).
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] if the request head cannot be encoded or is longer than one
    /// chunk; [`OhttpError::Internal`] if HPKE fails.
    pub fn start(
        config: &KeyConfig,
        request: &BinaryRequest,
        rng: &mut impl CryptoRng,
    ) -> Result<ChunkedClientStart, OhttpError> {
        let head = request.encode()?;
        if head.len() > MAX_CHUNK_PLAINTEXT {
            return Err(OhttpError::BadMessage);
        }
        let mut inner = config
            .client()?
            .encapsulate_chunked(rng)
            .map_err(|_| OhttpError::Internal)?;
        let sealed = inner.seal(&head, false).map_err(|_| OhttpError::Internal)?;
        let mut preamble = inner.header().to_vec();
        preamble.extend(frame(&sealed, false)?);
        Ok(ChunkedClientStart {
            preamble,
            client: Self {
                inner,
                finished: false,
            },
        })
    }

    /// Seals one outgoing chunk and returns its framed bytes.
    ///
    /// # Errors
    ///
    /// [`OhttpError::EmptyNonFinalChunk`] for an empty non-final chunk;
    /// [`OhttpError::DataAfterFinalChunk`] after the final chunk;
    /// [`OhttpError::ChunkTooLarge`] above 16 KiB;
    /// [`OhttpError::TooManyChunks`] if the counter would overflow.
    pub fn seal_chunk(
        &mut self,
        payload: &[u8],
        position: ChunkPosition,
    ) -> Result<Vec<u8>, OhttpError> {
        check_outgoing(payload, position, self.finished)?;
        let sealed = self
            .inner
            .seal(payload, position.is_final())
            .map_err(|error| seal_error(&error))?;
        self.finished = position.is_final();
        frame(&sealed, position.is_final())
    }

    /// A decoder for the response stream of this exchange.
    #[must_use]
    pub fn response_decoder(&self) -> ResponseChunkDecoder {
        // Exporting the response secret cannot fail for a context that sealed the preamble;
        // if it ever did, the decoder reports it on first use.
        let stage = match self.inner.response() {
            Ok(keys) => ResponseStage::Nonce {
                keys,
                nonce: Vec::new(),
            },
            Err(_) => ResponseStage::Failed,
        };
        ResponseChunkDecoder { stage }
    }
}

/// Where a response decoder is.
enum ResponseStage {
    /// Collecting the response nonce.
    Nonce {
        keys: ohttp::ChunkedResponseKeys,
        nonce: Vec<u8>,
    },
    /// Reading chunks.
    Chunks {
        opener: ohttp::ChunkOpener,
        reader: ChunkReader,
    },
    /// An error was returned; the stream cannot continue.
    Failed,
}

/// Opens the gateway's response stream on the client.
pub struct ResponseChunkDecoder {
    stage: ResponseStage,
}

impl ResponseChunkDecoder {
    /// Feeds bytes as they arrive; returns every chunk completed by them.
    ///
    /// After an error, every later call fails with [`OhttpError::OpenFailed`].
    ///
    /// # Errors
    ///
    /// [`OhttpError::OpenFailed`], [`OhttpError::EmptyNonFinalChunk`],
    /// [`OhttpError::ChunkTooLarge`].
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<OpenedChunk>, OhttpError> {
        let result = self.feed_inner(bytes);
        if result.is_err() {
            self.stage = ResponseStage::Failed;
        }
        result
    }

    fn feed_inner(&mut self, mut bytes: &[u8]) -> Result<Vec<OpenedChunk>, OhttpError> {
        if let ResponseStage::Nonce { keys, nonce } = &mut self.stage {
            let wanted = keys.nonce_len().saturating_sub(nonce.len());
            let (taken, rest) = bytes.split_at(wanted.min(bytes.len()));
            nonce.extend_from_slice(taken);
            bytes = rest;
            if nonce.len() < keys.nonce_len() {
                return Ok(Vec::new());
            }
            let opener = keys.opener(nonce).map_err(|_| OhttpError::Internal)?;
            self.stage = ResponseStage::Chunks {
                opener,
                reader: ChunkReader::new(),
            };
        }
        let ResponseStage::Chunks { opener, reader } = &mut self.stage else {
            return Err(OhttpError::OpenFailed);
        };
        reader
            .feed(bytes)?
            .iter()
            .map(|sealed| open_non_final(|sealed, last| opener.open(sealed, last), sealed))
            .collect()
    }

    /// Signals the end of the stream; opens the final chunk, which runs to the end.
    ///
    /// # Errors
    ///
    /// [`OhttpError::Truncated`] if no final chunk arrived; [`OhttpError::OpenFailed`].
    pub fn finish(self) -> Result<Option<OpenedChunk>, OhttpError> {
        match self.stage {
            ResponseStage::Nonce { .. } => Err(OhttpError::Truncated),
            ResponseStage::Chunks { mut opener, reader } => {
                let sealed = reader.finish()?;
                open_final(|sealed, last| opener.open(sealed, last), &sealed)
            }
            ResponseStage::Failed => Err(OhttpError::OpenFailed),
        }
    }
}

fn open_non_final(
    open: impl FnOnce(&[u8], bool) -> Result<Vec<u8>, ohttp::Error>,
    sealed: &[u8],
) -> Result<OpenedChunk, OhttpError> {
    Ok(OpenedChunk {
        payload: open(sealed, false).map_err(|error| open_error(&error))?,
        position: ChunkPosition::NonFinal,
    })
}

fn open_final(
    open: impl FnOnce(&[u8], bool) -> Result<Vec<u8>, ohttp::Error>,
    sealed: &[u8],
) -> Result<Option<OpenedChunk>, OhttpError> {
    Ok(Some(OpenedChunk {
        payload: open(sealed, true).map_err(|error| open_error(&error))?,
        position: ChunkPosition::Final,
    }))
}

/// What the request decoder starts with: the context after the head, and whatever followed
/// the head in the bytes given to [`ChunkedGateway::accept`].
struct RequestStart {
    opener: ohttp::ChunkedServerRequest,
    reader: ChunkReader,
    pending: VecDeque<Vec<u8>>,
}

/// The gateway half of one chunked exchange.
pub struct ChunkedGateway {
    request: BinaryRequest,
    /// Taken by the first [`ChunkedGateway::request_decoder`] call.
    start: Cell<Option<RequestStart>>,
    response: ohttp::ChunkedServerResponse,
    finished: bool,
}

impl ChunkedGateway {
    /// Accepts the start of a chunked request: decapsulates the header and request head.
    /// Returns the gateway half, which decodes the remaining request chunks and seals
    /// response chunks. The response nonce is drawn from `rng` here.
    ///
    /// `preamble` is the start of the request body: at least the header and the first chunk.
    /// Bytes after the first chunk are kept and decoded by [`ChunkedGateway::request_decoder`].
    ///
    /// # Errors
    ///
    /// [`OhttpError::Truncated`] if `preamble` ends before the first chunk does (call again
    /// with more bytes); [`OhttpError::UnknownKeyId`], [`OhttpError::UnsupportedSuite`],
    /// [`OhttpError::OpenFailed`], [`OhttpError::BadMessage`] (also if the request has no
    /// head chunk), [`OhttpError::ChunkTooLarge`].
    pub fn accept(
        gateway: &Gateway,
        preamble: &[u8],
        rng: &mut impl CryptoRng,
    ) -> Result<Self, OhttpError> {
        let key = gateway.select_key(preamble)?;
        let (header, chunks) = preamble
            .split_at_checked(ENCAPSULATION_LENGTH)
            .ok_or(OhttpError::Truncated)?;
        let mut reader = ChunkReader::new();
        let mut pending: VecDeque<Vec<u8>> = reader.feed(chunks)?.into();
        let Some(head) = pending.pop_front() else {
            return Err(if reader.in_final() {
                OhttpError::BadMessage
            } else {
                OhttpError::Truncated
            });
        };
        let mut opener = key
            .server()
            .decapsulate_chunked(header)
            .map_err(|_| OhttpError::OpenFailed)?;
        let head = opener
            .open(&head, false)
            .map_err(|error| open_error(&error))?;
        let request = BinaryRequest::decode(&head)?;
        let response = opener.response(rng).map_err(|_| OhttpError::Internal)?;
        Ok(Self {
            request,
            start: Cell::new(Some(RequestStart {
                opener,
                reader,
                pending,
            })),
            response,
            finished: false,
        })
    }

    /// The inner request head (method, path, headers; the body is the chunk stream).
    #[must_use]
    pub fn request(&self) -> &BinaryRequest {
        &self.request
    }

    /// A decoder for the rest of the request stream.
    ///
    /// There is one request stream, so only the first call returns a working decoder; a
    /// decoder from any later call fails with [`OhttpError::OpenFailed`].
    #[must_use]
    pub fn request_decoder(&self) -> RequestChunkDecoder {
        RequestChunkDecoder {
            start: self.start.take(),
        }
    }

    /// The bytes that open the response body (the response nonce).
    #[must_use]
    pub fn response_preamble(&self) -> Vec<u8> {
        self.response.nonce().to_vec()
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
    ) -> Result<Vec<u8>, OhttpError> {
        check_outgoing(payload, position, self.finished)?;
        let sealed = self
            .response
            .seal(payload, position.is_final())
            .map_err(|error| seal_error(&error))?;
        self.finished = position.is_final();
        frame(&sealed, position.is_final())
    }
}

/// Opens the client's request stream on the gateway.
pub struct RequestChunkDecoder {
    /// `None` once an error has been returned, or if this decoder was not the first.
    start: Option<RequestStart>,
}

impl RequestChunkDecoder {
    /// Feeds bytes as they arrive; returns every chunk completed by them, starting with any
    /// that arrived with the preamble.
    ///
    /// After an error, every later call fails with [`OhttpError::OpenFailed`].
    ///
    /// # Errors
    ///
    /// As [`ResponseChunkDecoder::feed`]; [`OhttpError::OpenFailed`] for a second decoder.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<OpenedChunk>, OhttpError> {
        let Some(start) = self.start.as_mut() else {
            return Err(OhttpError::OpenFailed);
        };
        let result = Self::feed_inner(start, bytes);
        if result.is_err() {
            self.start = None;
        }
        result
    }

    fn feed_inner(start: &mut RequestStart, bytes: &[u8]) -> Result<Vec<OpenedChunk>, OhttpError> {
        let fresh = start.reader.feed(bytes)?;
        start.pending.extend(fresh);
        start
            .pending
            .drain(..)
            .map(|sealed| open_non_final(|sealed, last| start.opener.open(sealed, last), &sealed))
            .collect()
    }

    /// Signals the end of the stream.
    ///
    /// # Errors
    ///
    /// As [`ResponseChunkDecoder::finish`]; also [`OhttpError::Truncated`] if chunks that
    /// arrived with the preamble were never returned by [`RequestChunkDecoder::feed`] (feed
    /// at least once, even with no bytes).
    pub fn finish(self) -> Result<Option<OpenedChunk>, OhttpError> {
        let Some(mut start) = self.start else {
            return Err(OhttpError::OpenFailed);
        };
        // Chunks that came with the preamble but were never fed out are lost data.
        if !start.pending.is_empty() {
            return Err(OhttpError::Truncated);
        }
        let sealed = start.reader.finish()?;
        open_final(|sealed, last| start.opener.open(sealed, last), &sealed)
    }
}

#[cfg(test)]
mod tests;
