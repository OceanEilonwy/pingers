// SPDX-License-Identifier: AGPL-3.0-only
//! The outer framing of chunked OHTTP (draft-ietf-ohai-chunked-ohttp-08 §4, §5), without any
//! cryptography: each non-final chunk is `varint(length) || sealed`, length at least one; the
//! final chunk is `varint(0) || sealed` and runs to the end of the stream.

use crate::OhttpError;

/// The largest chunk plaintext: upstream's limit, ample for a padded live frame.
pub(crate) const MAX_CHUNK_PLAINTEXT: usize = ohttp::chunked::MAX_CHUNK_PLAINTEXT;
/// The AES-128-GCM tag that sealing adds to every chunk.
pub(crate) const TAG_LENGTH: usize = 16;
/// The largest sealed chunk a reader accepts.
pub(crate) const MAX_SEALED_CHUNK: usize = MAX_CHUNK_PLAINTEXT + TAG_LENGTH;

/// Appends the QUIC variable-length integer encoding (RFC 9000 §16) of `value`.
///
/// Only lengths up to [`MAX_SEALED_CHUNK`] are framed, so the four-byte form is the largest
/// needed; larger values are an internal error. Always the shortest form.
pub(crate) fn write_varint(value: usize, output: &mut Vec<u8>) -> Result<(), OhttpError> {
    let value = u32::try_from(value).map_err(|_| OhttpError::Internal)?;
    if let Ok(byte) = u8::try_from(value)
        && byte < 0x40
    {
        output.push(byte);
    } else if let Ok(two) = u16::try_from(value)
        && two < 0x4000
    {
        output.extend_from_slice(&(two | 0x4000).to_be_bytes());
    } else if value < 0x4000_0000 {
        output.extend_from_slice(&(value | 0x8000_0000).to_be_bytes());
    } else {
        return Err(OhttpError::Internal);
    }
    Ok(())
}

/// Frames one sealed chunk.
pub(crate) fn frame(sealed: &[u8], last: bool) -> Result<Vec<u8>, OhttpError> {
    let mut framed = Vec::with_capacity(sealed.len().saturating_add(2));
    write_varint(if last { 0 } else { sealed.len() }, &mut framed)?;
    framed.extend_from_slice(sealed);
    Ok(framed)
}

/// What a complete varint at the start of a buffer says.
enum Varint {
    /// More bytes are needed.
    Incomplete,
    /// The value and how many bytes encoded it.
    Complete { value: u64, length: usize },
}

fn read_varint(bytes: &[u8]) -> Varint {
    let Some(first) = bytes.first() else {
        return Varint::Incomplete;
    };
    let length: usize = 1 << (first >> 6);
    let Some(encoded) = bytes.get(..length) else {
        return Varint::Incomplete;
    };
    let value = encoded
        .iter()
        .skip(1)
        .fold(u64::from(first & 0x3f), |value, byte| {
            (value << 8) | u64::from(*byte)
        });
    Varint::Complete { value, length }
}

/// Splits a chunk stream, arriving in any pieces, into sealed chunks.
pub(crate) struct ChunkReader {
    /// Bytes of an incomplete non-final chunk (its length prefix included), or after the final
    /// indicator, the final chunk so far.
    pending: Vec<u8>,
    /// Whether the final indicator has been read.
    in_final: bool,
}

impl ChunkReader {
    pub(crate) const fn new() -> Self {
        Self {
            pending: Vec::new(),
            in_final: false,
        }
    }

    /// Takes more bytes and returns every non-final sealed chunk they complete.
    ///
    /// [`OhttpError::ChunkTooLarge`] if a chunk is longer than [`MAX_SEALED_CHUNK`].
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, OhttpError> {
        self.pending.extend_from_slice(bytes);
        let mut chunks = Vec::new();
        while !self.in_final {
            let Varint::Complete { value, length } = read_varint(&self.pending) else {
                break;
            };
            if value == 0 {
                self.in_final = true;
                self.pending.drain(..length);
                break;
            }
            let chunk_length = usize::try_from(value)
                .ok()
                .filter(|chunk_length| *chunk_length <= MAX_SEALED_CHUNK)
                .ok_or(OhttpError::ChunkTooLarge)?;
            let end = length
                .checked_add(chunk_length)
                .ok_or(OhttpError::ChunkTooLarge)?;
            let Some(sealed) = self.pending.get(length..end) else {
                break;
            };
            chunks.push(sealed.to_vec());
            self.pending.drain(..end);
        }
        if self.in_final && self.pending.len() > MAX_SEALED_CHUNK {
            return Err(OhttpError::ChunkTooLarge);
        }
        Ok(chunks)
    }

    /// Ends the stream and returns the sealed final chunk.
    ///
    /// [`OhttpError::Truncated`] if the final indicator never arrived.
    pub(crate) fn finish(self) -> Result<Vec<u8>, OhttpError> {
        if self.in_final {
            Ok(self.pending)
        } else {
            Err(OhttpError::Truncated)
        }
    }

    /// Whether the final indicator has been read.
    pub(crate) const fn in_final(&self) -> bool {
        self.in_final
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use proptest::prelude::*;

    use super::{ChunkReader, MAX_SEALED_CHUNK, frame, write_varint};
    use crate::OhttpError;

    #[test]
    fn varint_uses_the_shortest_form() -> Result<(), OhttpError> {
        let mut encoded = Vec::new();
        for value in [63, 64, 0x3fff, 0x4000, 0x3fff_ffff] {
            write_varint(value, &mut encoded)?;
        }
        assert_eq!(
            encoded,
            [
                0x3f, 0x40, 0x40, 0x7f, 0xff, 0x80, 0x00, 0x40, 0x00, 0xbf, 0xff, 0xff, 0xff
            ]
        );
        Ok(())
    }

    #[test]
    fn varint_refuses_values_needing_eight_bytes() {
        assert!(matches!(
            write_varint(0x4000_0000, &mut Vec::new()),
            Err(OhttpError::Internal)
        ));
    }

    #[test]
    fn reader_accepts_non_minimal_length_encodings() -> Result<(), OhttpError> {
        // draft-08 §6: equivalent varint encodings are not authenticated, so all are valid.
        let mut reader = ChunkReader::new();
        let chunks = reader.feed(&[0x40, 0x02, 0xaa, 0xbb, 0x80, 0, 0, 1, 0xcc])?;
        assert_eq!(chunks, [vec![0xaa, 0xbb], vec![0xcc]]);
        Ok(())
    }

    #[test]
    fn reader_rejects_chunk_longer_than_limit() -> Result<(), OhttpError> {
        let mut too_long = Vec::new();
        write_varint(MAX_SEALED_CHUNK + 1, &mut too_long)?;
        assert!(matches!(
            ChunkReader::new().feed(&too_long),
            Err(OhttpError::ChunkTooLarge)
        ));
        // Eight-byte lengths are rejected before any allocation.
        assert!(matches!(
            ChunkReader::new().feed(&[0xff; 8]),
            Err(OhttpError::ChunkTooLarge)
        ));
        Ok(())
    }

    #[test]
    fn reader_accepts_chunk_at_limit() -> Result<(), OhttpError> {
        let sealed = vec![7; MAX_SEALED_CHUNK];
        let chunks = ChunkReader::new().feed(&frame(&sealed, false)?)?;
        assert_eq!(chunks, [sealed]);
        Ok(())
    }

    #[test]
    fn final_chunk_longer_than_limit_is_rejected() -> Result<(), OhttpError> {
        let mut reader = ChunkReader::new();
        reader.feed(&frame(&vec![1; MAX_SEALED_CHUNK], true)?)?;
        assert!(matches!(reader.feed(&[1]), Err(OhttpError::ChunkTooLarge)));
        Ok(())
    }

    #[test]
    fn stream_without_final_indicator_is_truncated() -> Result<(), OhttpError> {
        let mut reader = ChunkReader::new();
        reader.feed(&frame(&[1, 2, 3], false)?)?;
        assert!(!reader.in_final());
        assert!(matches!(reader.finish(), Err(OhttpError::Truncated)));
        Ok(())
    }

    #[test]
    fn empty_final_chunk_after_indicator_is_returned() -> Result<(), OhttpError> {
        let mut reader = ChunkReader::new();
        assert!(reader.feed(&[0])?.is_empty());
        assert!(reader.in_final());
        assert_eq!(reader.finish()?, Vec::<u8>::new());
        Ok(())
    }

    proptest! {
        /// Framing then reading returns the chunks, however the bytes are split.
        #[test]
        fn reading_framed_chunks_in_any_split_returns_them(
            chunks in proptest::collection::vec(
                proptest::collection::vec(any::<u8>(), 1..200), 0..8),
            last in proptest::collection::vec(any::<u8>(), 0..200),
            splits in proptest::collection::vec(1_usize..64, 0..40),
        ) {
            let mut stream = Vec::new();
            for chunk in &chunks {
                stream.extend(frame(chunk, false)?);
            }
            stream.extend(frame(&last, true)?);

            let mut reader = ChunkReader::new();
            let mut read = Vec::new();
            let mut rest = stream.as_slice();
            for split in splits {
                let (piece, after) = rest.split_at(split.min(rest.len()));
                read.extend(reader.feed(piece)?);
                rest = after;
            }
            read.extend(reader.feed(rest)?);
            prop_assert_eq!(read, chunks);
            prop_assert_eq!(reader.finish()?, last);
        }
    }
}
