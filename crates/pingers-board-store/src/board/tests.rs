// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;

use pingers_crypto::{RevokeKey, revoke_commitment, segment_hash};
use pingers_proto::{
    BoardCursor, BoardPage, BoardSyncRequest, Lease, LeaseId, LeaseReplacement, ReadBoard, Record,
    RecordRandomness, RecordTag, SealedBeacon, SecretBytes,
};
use pingers_testkit::{ManualClock, TEST_START, TestRng, seeded_rng, test_clock};
use pingers_time::{Bucket, BucketCount};
use proptest::prelude::*;
use zeroize::Zeroizing;

use super::*;
use crate::{BoardError, CursorKey};

/// Why a test failed outside an assertion.
#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Board(#[from] BoardError),
    #[error("bucket out of range")]
    Bucket,
    #[error("expected a page")]
    NoPage,
    #[error("expected a cursor")]
    NoCursor,
}

struct Fixture {
    clock: ManualClock,
    board: Board<ManualClock>,
    rng: TestRng,
}

impl Fixture {
    fn new(page_records: usize) -> Result<Self, TestError> {
        let clock = test_clock();
        let mut rng = seeded_rng(1);
        let board = Board::new(
            clock.clone(),
            CursorKey::generate(&mut rng),
            PageSize::new(page_records)?,
        );
        Ok(Self { clock, board, rng })
    }

    fn now(&self) -> Bucket {
        self.clock.now().bucket()
    }

    fn advance_buckets(&self, count: u32) {
        self.clock.advance(Bucket::LENGTH.saturating_mul(count));
    }

    fn sync(&mut self, lease: &Lease, read: ReadBoard) -> Result<BoardSyncResponse, BoardError> {
        let request = BoardSyncRequest {
            lease: *lease,
            replaces: None,
            read,
        };
        self.board.sync(&request, &mut self.rng)
    }

    fn post(&mut self, lease: &Lease) -> Result<LeaseId, TestError> {
        Ok(self.sync(lease, ReadBoard::Skip)?.lease_id)
    }

    /// Every record a full read returns, page by page.
    fn read_all(&mut self, lease: &Lease) -> Result<Vec<BoardPage>, TestError> {
        let first = self
            .sync(lease, ReadBoard::Read)?
            .page
            .ok_or(TestError::NoPage)?;
        self.follow(first)
    }

    fn follow(&mut self, first: BoardPage) -> Result<Vec<BoardPage>, TestError> {
        let mut pages = vec![first];
        while let Some(cursor) = pages.last().and_then(|page| page.next.clone()) {
            pages.push(self.board.page(&cursor, &mut self.rng)?);
        }
        Ok(pages)
    }
}

fn record(fill: u8) -> Record {
    Record {
        randomness: RecordRandomness::from_bytes([fill; 16]),
        tag: RecordTag::from_bytes([fill; 16]),
        sealed: SealedBeacon::from_bytes([fill; 64]),
    }
}

/// A lease whose records are `4 × id .. 4 × id + 3` and whose revoke key is `[id; 32]`.
fn lease(id: u8, start: Bucket) -> Lease {
    let base = id.wrapping_mul(4);
    Lease {
        start,
        records: [0, 1, 2, 3].map(|slot| record(base.wrapping_add(slot))),
        revoke_commitment: revoke_commitment(&revoke_key(id)),
    }
}

fn revoke_key(id: u8) -> RevokeKey {
    RevokeKey::from_bytes(Zeroizing::new([id; 32]))
}

fn revealed(id: u8) -> SecretBytes<32> {
    SecretBytes::new([id; 32])
}

fn records_of(pages: &[BoardPage]) -> Vec<Record> {
    pages.iter().flat_map(|page| page.records.clone()).collect()
}

fn back(bucket: Bucket, count: u64) -> Result<Bucket, TestError> {
    bucket
        .checked_sub(BucketCount::new(count))
        .ok_or(TestError::Bucket)
}

#[test]
fn page_size_must_be_between_one_and_the_wire_maximum() {
    assert!(matches!(PageSize::new(0), Err(BoardError::PageSize)));
    assert_eq!(PageSize::new(1).ok().map(PageSize::get), Some(1));
    assert_eq!(PageSize::new(2_730).ok(), Some(PageSize::MAX));
    assert!(matches!(PageSize::new(2_731), Err(BoardError::PageSize)));
}

#[test]
fn first_page_includes_the_callers_own_lease() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let now = fixture.now();
    fixture.post(&lease(1, now))?;
    let pages = fixture.read_all(&lease(2, now))?;
    assert_eq!(pages.len(), 1);
    assert_eq!(
        records_of(&pages),
        [lease(1, now).records, lease(2, now).records].concat()
    );
    Ok(())
}

#[test]
fn upload_without_read_returns_no_page() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let now = fixture.now();
    let response = fixture.sync(&lease(1, now), ReadBoard::Skip)?;
    assert_eq!(response.page, None);
    assert_eq!(response.server_time, TEST_START);
    assert_eq!(fixture.board.live_records(), 4);
    Ok(())
}

#[test]
fn every_lease_gets_its_own_id() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let now = fixture.now();
    let first = fixture.post(&lease(1, now))?;
    let second = fixture.post(&lease(1, now))?;
    assert_ne!(first, second);
    Ok(())
}

#[test]
fn lease_starting_in_the_previous_bucket_is_accepted() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let previous = back(fixture.now(), 1)?;
    fixture.post(&lease(1, previous))?;
    assert_eq!(fixture.board.live_records(), 4);
    Ok(())
}

#[test]
fn lease_from_two_buckets_ago_is_refused_and_not_stored() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let stale = back(fixture.now(), 2)?;
    assert!(matches!(
        fixture.sync(&lease(1, stale), ReadBoard::Skip),
        Err(BoardError::LeaseNotCurrent(_))
    ));
    assert_eq!(fixture.board.live_records(), 0);
    Ok(())
}

#[test]
fn replacing_a_lease_removes_its_records() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let now = fixture.now();
    let old = fixture.post(&lease(1, now))?;
    let request = BoardSyncRequest {
        lease: lease(2, now),
        replaces: Some(LeaseReplacement {
            lease_id: old,
            revoke_key: revealed(1),
        }),
        read: ReadBoard::Read,
    };
    let page = fixture
        .board
        .sync(&request, &mut fixture.rng)?
        .page
        .ok_or(TestError::NoPage)?;
    assert_eq!(page.records, lease(2, now).records.to_vec());
    assert!(matches!(
        fixture.board.revoke(&old, &revealed(1)),
        Err(BoardError::UnknownLease)
    ));
    Ok(())
}

#[test]
fn replacement_with_the_wrong_key_stores_nothing() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let now = fixture.now();
    let old = fixture.post(&lease(1, now))?;
    let request = BoardSyncRequest {
        lease: lease(2, now),
        replaces: Some(LeaseReplacement {
            lease_id: old,
            revoke_key: revealed(9),
        }),
        read: ReadBoard::Skip,
    };
    assert!(matches!(
        fixture.board.sync(&request, &mut fixture.rng),
        Err(BoardError::RevokeKeyMismatch)
    ));
    assert_eq!(fixture.board.live_records(), 4);
    Ok(())
}

#[test]
fn replacing_a_lease_the_board_no_longer_holds_still_stores_the_new_one() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let now = fixture.now();
    let request = BoardSyncRequest {
        lease: lease(2, now),
        replaces: Some(LeaseReplacement {
            lease_id: LeaseId::from_bytes([7; 16]),
            revoke_key: revealed(9),
        }),
        read: ReadBoard::Skip,
    };
    fixture.board.sync(&request, &mut fixture.rng)?;
    assert_eq!(fixture.board.live_records(), 4);
    Ok(())
}

#[test]
fn revoking_needs_the_key_that_matches_the_commitment() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let now = fixture.now();
    let id = fixture.post(&lease(1, now))?;
    assert!(matches!(
        fixture.board.revoke(&id, &revealed(2)),
        Err(BoardError::RevokeKeyMismatch)
    ));
    assert_eq!(fixture.board.live_records(), 4);
    fixture.board.revoke(&id, &revealed(1))?;
    assert_eq!(fixture.board.live_records(), 0);
    Ok(())
}

#[test]
fn revoking_an_unknown_lease_is_reported() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    assert!(matches!(
        fixture
            .board
            .revoke(&LeaseId::from_bytes([1; 16]), &revealed(1)),
        Err(BoardError::UnknownLease)
    ));
    Ok(())
}

#[test]
fn pages_split_the_board_without_gaps_or_repeats() -> Result<(), TestError> {
    let mut fixture = Fixture::new(3)?;
    let now = fixture.now();
    let previous = back(now, 1)?;
    fixture.post(&lease(1, previous))?;
    fixture.post(&lease(2, now))?;
    fixture.post(&lease(3, previous))?;
    let pages = fixture.read_all(&lease(4, now))?;

    let expected = [
        lease(1, previous).records,
        lease(3, previous).records,
        lease(2, now).records,
        lease(4, now).records,
    ]
    .concat();
    assert_eq!(records_of(&pages), expected);
    assert_eq!(pages.len(), 6);
    for page in &pages {
        assert!(page.records.len() <= 3);
        // A page names only the segments it carries records of.
        assert!(page.segments.iter().all(|segment| segment.record_count > 0));
        let counted: u32 = page
            .segments
            .iter()
            .map(|segment| segment.record_count)
            .sum();
        assert_eq!(usize::try_from(counted).ok(), Some(page.records.len()));
    }
    // The page that crosses the segment boundary names both segments, oldest first.
    let crossing = pages.get(2).ok_or(TestError::NoPage)?;
    let starts: Vec<Bucket> = crossing
        .segments
        .iter()
        .map(|segment| segment.start)
        .collect();
    assert_eq!(starts, vec![previous, now]);
    assert!(pages.iter().rev().skip(1).all(|page| page.next.is_some()));
    assert_eq!(pages.last().and_then(|page| page.next.clone()), None);
    Ok(())
}

#[test]
fn a_page_that_ends_exactly_at_the_last_record_is_final() -> Result<(), TestError> {
    let mut fixture = Fixture::new(4)?;
    let now = fixture.now();
    let pages = fixture.read_all(&lease(1, now))?;
    assert_eq!(pages.len(), 1);
    assert_eq!(pages.first().map(|page| page.records.len()), Some(4));
    Ok(())
}

#[test]
fn records_appended_during_a_read_reach_the_reader() -> Result<(), TestError> {
    let mut fixture = Fixture::new(4)?;
    let now = fixture.now();
    let previous = back(now, 1)?;
    fixture.post(&lease(1, previous))?;
    let first = fixture
        .sync(&lease(2, now), ReadBoard::Read)?
        .page
        .ok_or(TestError::NoPage)?;
    assert_eq!(first.records, lease(1, previous).records.to_vec());

    // One lease lands in the segment the reader has already passed, one in the current one.
    fixture.post(&lease(3, previous))?;
    fixture.post(&lease(4, now))?;
    let pages = fixture.follow(first)?;
    let mut seen = records_of(&pages);
    let mut expected = [
        lease(1, previous).records,
        lease(2, now).records,
        lease(3, previous).records,
        lease(4, now).records,
    ]
    .concat();
    seen.sort_by_key(Record::to_bytes);
    expected.sort_by_key(Record::to_bytes);
    assert_eq!(seen, expected);
    Ok(())
}

#[test]
fn records_revoked_during_a_read_are_not_served() -> Result<(), TestError> {
    let mut fixture = Fixture::new(4)?;
    let now = fixture.now();
    fixture.post(&lease(1, now))?;
    let second = fixture.post(&lease(2, now))?;
    let first = fixture
        .sync(&lease(3, now), ReadBoard::Read)?
        .page
        .ok_or(TestError::NoPage)?;
    fixture.board.revoke(&second, &revealed(2))?;
    let pages = fixture.follow(first)?;
    assert_eq!(
        records_of(&pages),
        [lease(1, now).records, lease(3, now).records].concat()
    );
    Ok(())
}

#[test]
fn cursor_with_any_byte_changed_is_rejected() -> Result<(), TestError> {
    let mut fixture = Fixture::new(1)?;
    let now = fixture.now();
    let cursor = fixture
        .sync(&lease(1, now), ReadBoard::Read)?
        .page
        .and_then(|page| page.next)
        .ok_or(TestError::NoCursor)?;
    for index in 0..cursor.as_bytes().len() {
        let mut bytes = cursor.as_bytes().to_vec();
        if let Some(byte) = bytes.get_mut(index) {
            *byte ^= 0x01;
        }
        assert!(matches!(
            fixture
                .board
                .page(&BoardCursor::from_bytes(bytes), &mut fixture.rng),
            Err(BoardError::BadCursor)
        ));
    }
    let truncated = cursor
        .as_bytes()
        .get(..cursor.as_bytes().len().saturating_sub(1));
    assert!(matches!(
        fixture.board.page(
            &BoardCursor::from_bytes(truncated.unwrap_or(&[]).to_vec()),
            &mut fixture.rng
        ),
        Err(BoardError::BadCursor)
    ));
    assert!(fixture.board.page(&cursor, &mut fixture.rng).is_ok());
    Ok(())
}

#[test]
fn cursor_from_another_board_is_rejected() -> Result<(), TestError> {
    let mut fixture = Fixture::new(1)?;
    let mut other = Fixture::new(1)?;
    other.rng = seeded_rng(2);
    other.board = Board::new(
        other.clock.clone(),
        CursorKey::generate(&mut other.rng),
        PageSize::new(1)?,
    );
    let now = fixture.now();
    let cursor = other
        .sync(&lease(1, now), ReadBoard::Read)?
        .page
        .and_then(|page| page.next)
        .ok_or(TestError::NoCursor)?;
    assert!(matches!(
        fixture.board.page(&cursor, &mut fixture.rng),
        Err(BoardError::BadCursor)
    ));
    Ok(())
}

#[test]
fn cursor_expires_ten_minutes_after_the_first_page() -> Result<(), TestError> {
    let mut fixture = Fixture::new(1)?;
    let now = fixture.now();
    let cursor = fixture
        .sync(&lease(1, now), ReadBoard::Read)?
        .page
        .and_then(|page| page.next)
        .ok_or(TestError::NoCursor)?;
    fixture.clock.advance(Duration::from_secs(599));
    let second = fixture.board.page(&cursor, &mut fixture.rng)?;
    fixture.clock.advance(Duration::from_secs(1));
    assert!(matches!(
        fixture.board.page(&cursor, &mut fixture.rng),
        Err(BoardError::CursorExpired)
    ));
    // A later cursor of the same read carries the first page's time, so it has expired too.
    let later = second.next.ok_or(TestError::NoCursor)?;
    assert!(matches!(
        fixture.board.page(&later, &mut fixture.rng),
        Err(BoardError::CursorExpired)
    ));
    Ok(())
}

#[test]
fn segment_is_live_for_four_buckets_and_dropped_when_the_fifth_starts() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let start = fixture.now();
    let id = fixture.post(&lease(1, start))?;
    fixture.advance_buckets(3);
    let pages = fixture.read_all(&lease(2, fixture.now()))?;
    assert!(records_of(&pages).contains(&record(4)));

    fixture.advance_buckets(1);
    let pages = fixture.read_all(&lease(3, fixture.now()))?;
    assert!(!records_of(&pages).contains(&record(4)));
    assert!(matches!(
        fixture.board.revoke(&id, &revealed(1)),
        Err(BoardError::UnknownLease)
    ));
    Ok(())
}

#[test]
fn expire_frees_segments_without_a_request() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let now = fixture.now();
    fixture.post(&lease(1, now))?;
    fixture.advance_buckets(3);
    fixture.board.expire();
    assert_eq!(fixture.board.live_records(), 4);
    fixture.advance_buckets(1);
    fixture.board.expire();
    assert_eq!(fixture.board.live_records(), 0);
    Ok(())
}

#[test]
fn page_skips_a_segment_that_expired_during_the_read() -> Result<(), TestError> {
    let mut fixture = Fixture::new(4)?;
    let start = fixture.now();
    fixture.post(&lease(1, start))?;
    // Read in the last minute of the segment's last live bucket.
    fixture.advance_buckets(3);
    fixture.clock.advance(Duration::from_mins(14));
    let now = fixture.now();
    fixture.post(&lease(2, now))?;
    let first = fixture
        .sync(&lease(3, now), ReadBoard::Read)?
        .page
        .ok_or(TestError::NoPage)?;
    assert_eq!(first.records, lease(1, start).records.to_vec());
    fixture.clock.advance(Duration::from_mins(1));
    let pages = fixture.follow(first)?;
    assert_eq!(
        records_of(pages.get(1..).unwrap_or(&[])),
        [lease(2, now).records, lease(3, now).records].concat()
    );
    Ok(())
}

#[test]
fn closed_segment_hash_appears_two_buckets_after_its_start() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100)?;
    let start = fixture.now();
    let id = fixture.post(&lease(1, start))?;
    fixture.post(&lease(2, start))?;
    let hash_now = |fixture: &mut Fixture, id: u8| -> Result<Option<SegmentHash>, TestError> {
        let pages = fixture.read_all(&lease(id, fixture.now()))?;
        Ok(pages
            .iter()
            .flat_map(|page| page.segments.iter())
            .find(|segment| segment.start == start)
            .and_then(|segment| segment.closed_hash))
    };
    assert_eq!(hash_now(&mut fixture, 3)?, None);
    fixture.advance_buckets(1);
    assert_eq!(hash_now(&mut fixture, 4)?, None);
    fixture.advance_buckets(1);
    let expected = segment_hash(
        &[
            lease(1, start).records,
            lease(2, start).records,
            lease(3, start).records,
        ]
        .concat(),
    );
    assert_eq!(hash_now(&mut fixture, 5)?, Some(expected));

    // Revoking a lease later in its life does not change the published hash.
    fixture.board.revoke(&id, &revealed(1))?;
    assert_eq!(hash_now(&mut fixture, 6)?, Some(expected));
    Ok(())
}

/// Turns any test error into a proptest failure.
fn fail(error: impl core::fmt::Display) -> TestCaseError {
    TestCaseError::fail(error.to_string())
}

proptest! {
    #[test]
    fn a_full_read_returns_each_live_record_once_in_board_order(
        page_records in 1_usize..12,
        leases in proptest::collection::vec((any::<bool>(), any::<bool>()), 1..20),
    ) {
        let mut fixture = Fixture::new(page_records).map_err(fail)?;
        let now = fixture.now();
        let previous = back(now, 1).map_err(fail)?;
        let mut expected_previous = Vec::new();
        let mut expected_now = Vec::new();
        for (index, (in_previous, revoke)) in leases.iter().enumerate() {
            let id = u8::try_from(index).map_err(fail)?;
            let start = if *in_previous { previous } else { now };
            let lease_id = fixture.post(&lease(id, start)).map_err(fail)?;
            if *revoke {
                let key = revealed(id);
                fixture.board.revoke(&lease_id, &key).map_err(fail)?;
            } else if *in_previous {
                expected_previous.extend(lease(id, start).records);
            } else {
                expected_now.extend(lease(id, start).records);
            }
        }
        let reader = lease(200, now);
        expected_now.extend(reader.records);
        let pages = fixture.read_all(&reader).map_err(fail)?;
        prop_assert_eq!(records_of(&pages), [expected_previous, expected_now].concat());
        for page in &pages {
            prop_assert!(page.records.len() <= page_records);
            prop_assert!(!page.records.is_empty());
        }
    }
}

/// Each cursor is good for one page: fetching it again (to make the server read and send the
/// same page twice) is refused, while the next page's cursor still works.
#[test]
fn a_cursor_is_good_for_one_page() -> Result<(), TestError> {
    let mut fixture = Fixture::new(1)?;
    let now = fixture.now();
    let cursor = fixture
        .sync(&lease(1, now), ReadBoard::Read)?
        .page
        .and_then(|page| page.next)
        .ok_or(TestError::NoCursor)?;
    let second = fixture.board.page(&cursor, &mut fixture.rng)?;
    assert!(matches!(
        fixture.board.page(&cursor, &mut fixture.rng),
        Err(BoardError::CursorSpent)
    ));
    if let Some(next) = second.next {
        fixture.board.page(&next, &mut fixture.rng)?;
    }
    Ok(())
}
