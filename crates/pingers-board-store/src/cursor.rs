// SPDX-License-Identifier: AGPL-3.0-only
//! Board cursors: the reader's position in each segment, sealed so only this board can read or
//! change it (design B.4.3, D: "opaque, server-MACed").
//!
//! The position is sealed with pingers-crypto's board-cursor seal (XChaCha20-Poly1305 with the
//! `pingers/v1/board-cursor` label as associated data) under a key that exists only for
//! cursors. Authenticated encryption gives the MAC the design asks for and also keeps segment
//! positions opaque to the client.

use core::time::Duration;
use std::collections::BTreeMap;

use pingers_crypto::{BoardCursorKey, open_board_cursor, seal_board_cursor};
use pingers_proto::BoardCursor;
use pingers_time::{Bucket, UnixSeconds};
use rand_core::CryptoRng;

use crate::BoardError;

/// How long a cursor stays valid after the first page of its read (design B.4.3).
pub const CURSOR_LIFETIME: Duration = Duration::from_mins(10);

/// The layout version inside the seal.
const VERSION: u8 = 1;

/// The key that seals board cursors. Generated at start-up and never stored: the board lives
/// in memory, so cursors die with it.
pub struct CursorKey(BoardCursorKey);

impl CursorKey {
    /// A fresh key.
    #[must_use]
    pub fn generate(rng: &mut impl CryptoRng) -> Self {
        Self(BoardCursorKey::generate(rng))
    }
}

/// Where a read stands: when it began, and the next raw position in each segment visited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReadPosition {
    /// When the first page of this read was served.
    pub(crate) issued: UnixSeconds,
    /// Per segment start bucket, the index of the next record to consider.
    pub(crate) next: BTreeMap<Bucket, usize>,
}

impl ReadPosition {
    /// A read starting now, at the beginning of every segment.
    pub(crate) const fn start(issued: UnixSeconds) -> Self {
        Self {
            issued,
            next: BTreeMap::new(),
        }
    }

    /// When the read stops being current: [`CURSOR_LIFETIME`] after it began.
    pub(crate) fn expires_at(&self) -> Option<UnixSeconds> {
        self.issued.checked_add(CURSOR_LIFETIME)
    }

    /// Whether the read is still within [`CURSOR_LIFETIME`] at `now`.
    pub(crate) fn is_current(&self, now: UnixSeconds) -> bool {
        now.saturating_duration_since(self.issued) < CURSOR_LIFETIME
    }

    /// `version || be64(issued) || count || count × (be64(bucket) || be32(next))`.
    fn encode(&self) -> Result<Vec<u8>, BoardError> {
        let count = u8::try_from(self.next.len()).map_err(|_overflow| BoardError::Internal)?;
        let mut bytes = Vec::new();
        bytes.push(VERSION);
        bytes.extend_from_slice(&self.issued.get().to_be_bytes());
        bytes.push(count);
        for (start, next) in &self.next {
            let next = u32::try_from(*next).map_err(|_overflow| BoardError::Internal)?;
            bytes.extend_from_slice(&start.to_be_bytes());
            bytes.extend_from_slice(&next.to_be_bytes());
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, BoardError> {
        let (&[version], rest) = bytes
            .split_first_chunk::<1>()
            .ok_or(BoardError::BadCursor)?;
        if version != VERSION {
            return Err(BoardError::BadCursor);
        }
        let (issued, rest) = rest.split_first_chunk::<8>().ok_or(BoardError::BadCursor)?;
        let (&[count], mut rest) = rest.split_first_chunk::<1>().ok_or(BoardError::BadCursor)?;
        let mut next = BTreeMap::new();
        for _ in 0..count {
            let (start, after_start) =
                rest.split_first_chunk::<8>().ok_or(BoardError::BadCursor)?;
            let (position, after_position) = after_start
                .split_first_chunk::<4>()
                .ok_or(BoardError::BadCursor)?;
            let start = Bucket::from_index(u64::from_be_bytes(*start))
                .map_err(|_range| BoardError::BadCursor)?;
            let position = usize::try_from(u32::from_be_bytes(*position))
                .map_err(|_overflow| BoardError::BadCursor)?;
            next.insert(start, position);
            rest = after_position;
        }
        if !rest.is_empty() {
            return Err(BoardError::BadCursor);
        }
        Ok(Self {
            issued: UnixSeconds::new(u64::from_be_bytes(*issued)),
            next,
        })
    }

    /// Seals the position into an opaque cursor.
    pub(crate) fn seal(
        &self,
        key: &CursorKey,
        rng: &mut impl CryptoRng,
    ) -> Result<BoardCursor, BoardError> {
        Ok(seal_board_cursor(&key.0, &self.encode()?, rng)?)
    }

    /// Opens a cursor sealed by [`ReadPosition::seal`] under the same key.
    pub(crate) fn open(key: &CursorKey, cursor: &BoardCursor) -> Result<Self, BoardError> {
        let plaintext =
            open_board_cursor(&key.0, cursor).map_err(|_tampered| BoardError::BadCursor)?;
        Self::decode(&plaintext)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_testkit::seeded_rng;

    use super::*;

    fn position() -> Result<ReadPosition, BoardError> {
        let mut next = BTreeMap::new();
        next.insert(
            Bucket::from_index(7).map_err(|_range| BoardError::Internal)?,
            3,
        );
        next.insert(
            Bucket::from_index(8).map_err(|_range| BoardError::Internal)?,
            70_000,
        );
        Ok(ReadPosition {
            issued: UnixSeconds::new(1_000),
            next,
        })
    }

    #[test]
    fn sealed_position_opens_unchanged() -> Result<(), BoardError> {
        let mut rng = seeded_rng(1);
        let key = CursorKey::generate(&mut rng);
        let cursor = position()?.seal(&key, &mut rng)?;
        assert_eq!(ReadPosition::open(&key, &cursor)?, position()?);
        Ok(())
    }

    #[test]
    fn cursor_from_another_key_is_rejected() -> Result<(), BoardError> {
        let mut rng = seeded_rng(2);
        let cursor = position()?.seal(&CursorKey::generate(&mut rng), &mut rng)?;
        let other = CursorKey::generate(&mut rng);
        assert!(matches!(
            ReadPosition::open(&other, &cursor),
            Err(BoardError::BadCursor)
        ));
        Ok(())
    }

    #[test]
    fn trailing_bytes_after_the_segments_are_rejected() -> Result<(), BoardError> {
        let mut bytes = position()?.encode()?;
        assert!(ReadPosition::decode(&bytes).is_ok());
        bytes.push(0);
        assert!(matches!(
            ReadPosition::decode(&bytes),
            Err(BoardError::BadCursor)
        ));
        Ok(())
    }

    #[test]
    fn missing_segment_entries_are_rejected() -> Result<(), BoardError> {
        let bytes = position()?.encode()?;
        let truncated = bytes.get(..bytes.len().saturating_sub(1)).unwrap_or(&[]);
        assert!(matches!(
            ReadPosition::decode(truncated),
            Err(BoardError::BadCursor)
        ));
        Ok(())
    }

    #[test]
    fn unknown_version_is_rejected() -> Result<(), BoardError> {
        let mut bytes = position()?.encode()?;
        if let Some(version) = bytes.first_mut() {
            *version = 2;
        }
        assert!(matches!(
            ReadPosition::decode(&bytes),
            Err(BoardError::BadCursor)
        ));
        Ok(())
    }

    #[test]
    fn read_is_current_for_just_under_ten_minutes() {
        let read = ReadPosition::start(UnixSeconds::new(1_000));
        assert!(read.is_current(UnixSeconds::new(1_599)));
        assert!(!read.is_current(UnixSeconds::new(1_600)));
    }
}
