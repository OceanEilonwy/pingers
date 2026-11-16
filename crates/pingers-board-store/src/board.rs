// SPDX-License-Identifier: AGPL-3.0-only
//! The blind board: one append-only segment per start bucket, held in memory only
//! (design B.4.3, C.1).

use std::collections::{BTreeMap, HashMap};

use pingers_crypto::{revoke_key_matches, segment_hash};
use pingers_presence::{check_lease, segment_expired};
use pingers_proto::{
    BoardCursor, BoardPage, BoardSyncRequest, BoardSyncResponse, LeaseId, ReadBoard, Record,
    RevokeCommitment, SecretBytes, SegmentHash, SegmentInfo,
};
use pingers_time::{Bucket, BucketCount, Clock, UnixSeconds};
use rand_core::CryptoRng;

use crate::BoardError;
use crate::cursor::{CursorKey, ReadPosition};

/// A segment accepts appends while its start bucket is the current or the previous bucket
/// (pingers-presence `check_lease`); from the second bucket after its start it is closed.
const CLOSED_AFTER: BucketCount = BucketCount::new(2);

/// The length of a cursor's random nonce, its first bytes (pingers-crypto's board-cursor seal).
const CURSOR_NONCE_LENGTH: usize = 24;

/// The most used cursors remembered at once. Each cursor needs a credential-gated board sync,
/// and is remembered only for [`crate::CURSOR_LIFETIME`]; past this, pages are refused until
/// some expire.
const MAX_SPENT_CURSORS: usize = 262_144;

/// How many records a page carries: between 1 and [`BoardPage::MAX_RECORDS`]
/// (`Params.board_page_records`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageSize(usize);

impl PageSize {
    /// The largest page the wire allows: 2,730 records (256 KiB).
    pub const MAX: Self = Self(BoardPage::MAX_RECORDS);

    /// Checks the bound.
    ///
    /// # Errors
    ///
    /// [`BoardError::PageSize`] for zero or more than [`BoardPage::MAX_RECORDS`].
    pub const fn new(records: usize) -> Result<Self, BoardError> {
        if records == 0 || records > BoardPage::MAX_RECORDS {
            Err(BoardError::PageSize)
        } else {
            Ok(Self(records))
        }
    }

    /// The number of records.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// One start bucket's records, in arrival order.
#[derive(Default)]
struct Segment {
    records: Vec<Record>,
    /// Parallel to `records`: revoked records stay in place so positions never move.
    revoked: Vec<bool>,
    /// Computed once the segment is closed; closed segments never change their records.
    closed_hash: Option<SegmentHash>,
}

impl Segment {
    /// The segment's hash if it is closed at `now`, computing it on first use.
    ///
    /// The hash covers every record ever appended, revoked or not, in arrival order: it is the
    /// server's commitment to the segment, and later revocations must not change it.
    fn closed_hash(&mut self, start: Bucket, now: Bucket) -> Option<SegmentHash> {
        if now.saturating_buckets_since(start) < CLOSED_AFTER {
            return None;
        }
        Some(
            *self
                .closed_hash
                .get_or_insert_with(|| segment_hash(&self.records)),
        )
    }
}

/// A stored lease: who may revoke it, and where its four records are.
struct LeaseEntry {
    commitment: RevokeCommitment,
    start: Bucket,
    positions: [usize; 4],
}

/// The in-memory blind board.
///
/// Never written to disk or logs (design C.1). It holds no index by value and counts no
/// matches: the server matches nothing. Wrap it in a mutex to share it between requests.
pub struct Board<C> {
    clock: C,
    cursor_key: CursorKey,
    page_size: PageSize,
    segments: BTreeMap<Bucket, Segment>,
    leases: HashMap<LeaseId, LeaseEntry>,
    /// Cursors already used, by their nonce, with when they stop being current: each cursor
    /// is good for one page (an answer cannot be fetched twice to amplify a read).
    spent: HashMap<[u8; CURSOR_NONCE_LENGTH], UnixSeconds>,
}

impl<C: Clock> Board<C> {
    /// An empty board that seals cursors with `cursor_key` and serves pages of `page_size`.
    #[must_use]
    pub fn new(clock: C, cursor_key: CursorKey, page_size: PageSize) -> Self {
        Self {
            clock,
            cursor_key,
            page_size,
            segments: BTreeMap::new(),
            leases: HashMap::new(),
            spent: HashMap::new(),
        }
    }

    /// `POST /v1/board/sync`: appends the lease, revokes the lease it replaces, and, if asked,
    /// returns the first page of the board taken after the append, so the caller's own records
    /// are on it (design B.4.3, "post, then read").
    ///
    /// Replacing a lease the board no longer holds (expired, or already revoked) is not an
    /// error: the new lease is stored all the same.
    ///
    /// # Errors
    ///
    /// [`BoardError::LeaseNotCurrent`] if the lease does not start in the current or previous
    /// bucket; [`BoardError::RevokeKeyMismatch`] if the replaced lease is live and the key does
    /// not match it. Nothing is stored in either case.
    pub fn sync(
        &mut self,
        request: &BoardSyncRequest,
        rng: &mut impl CryptoRng,
    ) -> Result<BoardSyncResponse, BoardError> {
        let now = self.clock.now();
        self.expire();
        check_lease(&request.lease, now.bucket()).map_err(BoardError::LeaseNotCurrent)?;
        let replaced = match &request.replaces {
            Some(replacement) if self.leases.contains_key(&replacement.lease_id) => {
                self.authorise(&replacement.lease_id, &replacement.revoke_key)?;
                Some(replacement.lease_id)
            }
            Some(_) | None => None,
        };

        let lease_id = self.fresh_lease_id(rng);
        let segment = self.segments.entry(request.lease.start).or_default();
        let first = segment.records.len();
        segment.records.extend_from_slice(&request.lease.records);
        segment.revoked.resize(segment.records.len(), false);
        let positions = [0, 1, 2, 3].map(|slot: usize| first.saturating_add(slot));
        self.leases.insert(
            lease_id,
            LeaseEntry {
                commitment: request.lease.revoke_commitment,
                start: request.lease.start,
                positions,
            },
        );
        if let Some(replaced) = replaced {
            self.remove_lease(&replaced);
        }

        let page = match request.read {
            ReadBoard::Read => Some(self.page_from(ReadPosition::start(now), rng)?),
            ReadBoard::Skip => None,
        };
        Ok(BoardSyncResponse {
            lease_id,
            server_time: now,
            page,
        })
    }

    /// `GET /v1/board/page?c=<cursor>`: the next page of a read begun by [`Board::sync`].
    ///
    /// Segments are append-only and the cursor holds a position per segment, so records
    /// appended since the previous page are picked up, and the last page includes everything
    /// appended since the first. Segments that expired in the meantime are skipped.
    ///
    /// # Errors
    ///
    /// [`BoardError::BadCursor`] if the cursor was not sealed by this board or was altered;
    /// [`BoardError::CursorExpired`] once [`crate::CURSOR_LIFETIME`] has passed since the
    /// read's first page.
    pub fn page(
        &mut self,
        cursor: &BoardCursor,
        rng: &mut impl CryptoRng,
    ) -> Result<BoardPage, BoardError> {
        let position = ReadPosition::open(&self.cursor_key, cursor)?;
        let now = self.clock.now();
        if !position.is_current(now) {
            return Err(BoardError::CursorExpired);
        }
        self.expire();
        let nonce: [u8; CURSOR_NONCE_LENGTH] = cursor
            .as_bytes()
            .first_chunk()
            .copied()
            .ok_or(BoardError::BadCursor)?;
        if self.spent.contains_key(&nonce) || self.spent.len() >= MAX_SPENT_CURSORS {
            return Err(BoardError::CursorSpent);
        }
        let until = position.expires_at().ok_or(BoardError::Internal)?;
        self.spent.insert(nonce, until);
        self.page_from(position, rng)
    }

    /// `DELETE /v1/board/lease/{id}`: removes a lease's records from the live board.
    ///
    /// # Errors
    ///
    /// [`BoardError::UnknownLease`] if no live lease has this id;
    /// [`BoardError::RevokeKeyMismatch`] if the key does not hash to its commitment.
    pub fn revoke(
        &mut self,
        lease_id: &LeaseId,
        revoke_key: &SecretBytes<32>,
    ) -> Result<(), BoardError> {
        self.expire();
        self.authorise(lease_id, revoke_key)?;
        self.remove_lease(lease_id);
        Ok(())
    }

    /// Drops every segment whose lease horizon has passed, and the leases in it: a segment
    /// goes when bucket `b0 + 4` starts. Every operation calls this first; the server may also
    /// call it on a timer to free memory sooner.
    pub fn expire(&mut self) {
        let instant = self.clock.now();
        self.spent.retain(|_nonce, until| *until > instant);
        let now = instant.bucket();
        self.segments
            .retain(|start, _segment| !segment_expired(*start, now));
        let segments = &self.segments;
        self.leases
            .retain(|_id, lease| segments.contains_key(&lease.start));
    }

    /// Live records on the board, for metrics (a count, never contents).
    #[must_use]
    pub fn live_records(&self) -> usize {
        self.segments
            .values()
            .map(|segment| segment.revoked.iter().filter(|revoked| !**revoked).count())
            .sum()
    }

    fn authorise(
        &self,
        lease_id: &LeaseId,
        revoke_key: &SecretBytes<32>,
    ) -> Result<(), BoardError> {
        let lease = self.leases.get(lease_id).ok_or(BoardError::UnknownLease)?;
        if revoke_key_matches(revoke_key, &lease.commitment) {
            Ok(())
        } else {
            Err(BoardError::RevokeKeyMismatch)
        }
    }

    fn remove_lease(&mut self, lease_id: &LeaseId) {
        let Some(lease) = self.leases.remove(lease_id) else {
            return;
        };
        if let Some(segment) = self.segments.get_mut(&lease.start) {
            for position in lease.positions {
                if let Some(revoked) = segment.revoked.get_mut(position) {
                    *revoked = true;
                }
            }
        }
    }

    fn fresh_lease_id(&self, rng: &mut impl CryptoRng) -> LeaseId {
        loop {
            let mut bytes = [0; 16];
            rng.fill_bytes(&mut bytes);
            let id = LeaseId::from_bytes(bytes);
            if !self.leases.contains_key(&id) {
                return id;
            }
        }
    }

    /// Up to one page of live records from `position` on, oldest segment first, and a cursor
    /// if live records remain.
    fn page_from(
        &mut self,
        mut position: ReadPosition,
        rng: &mut impl CryptoRng,
    ) -> Result<BoardPage, BoardError> {
        let now = self.clock.now().bucket();
        let limit = self.page_size.get();
        let mut records = Vec::new();
        let mut segments = Vec::new();
        let mut more = false;
        for (start, segment) in &mut self.segments {
            let mut next = position.next.get(start).copied().unwrap_or(0);
            let mut count: u32 = 0;
            while let Some(record) = segment.records.get(next) {
                if segment.revoked.get(next).copied().unwrap_or(true) {
                    next = next.saturating_add(1);
                    continue;
                }
                if records.len() >= limit {
                    more = true;
                    break;
                }
                records.push(*record);
                count = count.checked_add(1).ok_or(BoardError::Internal)?;
                next = next.saturating_add(1);
            }
            if count > 0 {
                segments.push(SegmentInfo {
                    start: *start,
                    record_count: count,
                    closed_hash: segment.closed_hash(*start, now),
                });
            }
            position.next.insert(*start, next);
            if more {
                break;
            }
        }
        let next = if more {
            let live = &self.segments;
            position
                .next
                .retain(|start, _next| live.contains_key(start));
            Some(position.seal(&self.cursor_key, rng)?)
        } else {
            None
        };
        Ok(BoardPage {
            segments,
            records,
            next,
        })
    }
}

#[cfg(test)]
mod tests;
