// SPDX-License-Identifier: AGPL-3.0-only
//! Trial matching: what matches, what never does, and which matches are knocked (B.4.3).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_crypto::{RevokeKey, record_tag};
use pingers_geo::PresenceState;
use pingers_proto::{
    BoardBeacon, Epoch, GroupId, MailboxId, MeetupHint, ProtoError, Record, RecordRandomness,
    SealedBeacon,
};
use pingers_testkit::{TestRng, random_array, seeded_rng};
use pingers_time::{Bucket, BucketCount};
use proptest::prelude::*;

use crate::test_support::{TestBoard, TestError, TestParty};
use crate::{
    ActiveSpot, BoardMatch, BuiltLease, EpochState, EpochTransition, KnockFilter, LeaseInputs,
    MAX_KNOCKS_PER_ARRIVAL, OwnRecords, PresenceError, RealSlot, SpotStatus, TrialMatcher,
    build_lease, knock_targets, matching_epochs,
};

const EPOCH: Epoch = Epoch::new(9);

fn now() -> Result<Bucket, TestError> {
    Ok(Bucket::from_index(2_000_000)?)
}

fn lease(start: Bucket, real: &[&ActiveSpot], rng: &mut TestRng) -> Result<BuiltLease, TestError> {
    let revoke_key = RevokeKey::generate(rng);
    let real = real
        .iter()
        .map(|spot| RealSlot {
            spot,
            knock_mailbox: MailboxId::from_bytes(random_array(rng)),
            meetup_hint: None,
        })
        .collect();
    Ok(build_lease(
        &LeaseInputs {
            start,
            real,
            revoke_key: &revoke_key,
        },
        rng,
    )?)
}

fn matched(
    spots: &[ActiveSpot],
    board: &TestBoard,
    own: &OwnRecords,
) -> Result<Vec<BoardMatch>, TestError> {
    Ok(TrialMatcher::new(spots, now()?).match_page(&board.page()?, own)?)
}

fn real_randomness(built: &BuiltLease) -> Vec<RecordRandomness> {
    built
        .slots
        .iter()
        .filter(|slot| matches!(slot.content, crate::SlotContent::Real { .. }))
        .map(|slot| slot.randomness)
        .collect()
}

proptest! {
    #[test]
    fn matching_finds_every_real_record_of_an_active_spot_and_nothing_else(
        seed in any::<u64>(),
        // Per device: which of Spots 0, 1, 2 it has a real record for (bit mask).
        devices in proptest::collection::vec(0_u8..8, 1..6),
        own_device in 0_usize..6,
    ) {
        let check = || -> Result<(), TestError> {
            let mut rng = seeded_rng(seed);
            let party = TestParty::new(&mut rng);
            let all: Vec<ActiveSpot> = (0..3)
                .map(|_| Ok(party.other_spot(&mut rng).inside(EPOCH)?))
                .collect::<Result<_, TestError>>()?;
            // The reader is Inside Spots 0 and 1 only.
            let reader = [party_spot(&party, &all, 0)?, party_spot(&party, &all, 1)?];
            let mut board = TestBoard::default();
            let mut expected = Vec::new();
            let mut own = Vec::new();
            for (index, mask) in devices.iter().enumerate() {
                let real: Vec<&ActiveSpot> = all
                    .iter()
                    .enumerate()
                    .filter(|(spot, _)| mask & (1 << spot) != 0)
                    .map(|(_, spot)| spot)
                    .collect();
                let built = lease(now()?, &real, &mut rng)?;
                board.post(&built.lease);
                let active: Vec<RecordRandomness> = built
                    .slots
                    .iter()
                    .filter(|slot| matches!(
                        slot.content,
                        crate::SlotContent::Real { spot, .. }
                            if reader.iter().any(|active| active.spot() == spot)
                    ))
                    .map(|slot| slot.randomness)
                    .collect();
                if index == own_device {
                    own.extend(built.slots.iter().map(|slot| slot.randomness));
                } else {
                    expected.extend(active);
                }
            }
            let found: Vec<RecordRandomness> = matched(&reader, &board, &OwnRecords::new(own))?
                .iter()
                .map(|found| found.record.randomness)
                .collect();
            assert_eq!(found, expected);
            Ok(())
        };
        check().map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}

/// Spot `index` of `all`, derived again as a separate device would.
fn party_spot(
    party: &TestParty,
    all: &[ActiveSpot],
    index: usize,
) -> Result<ActiveSpot, TestError> {
    let spot = all.get(index).ok_or(TestError::Malformed("spot index"))?;
    Ok(ActiveSpot::derive(
        &party.root(EPOCH),
        &SpotStatus {
            spot: spot.spot(),
            epoch: EPOCH,
            state: PresenceState::Inside,
        },
    )?)
}

#[test]
fn keys_exist_only_while_inside() -> Result<(), TestError> {
    let party = TestParty::new(&mut seeded_rng(20));
    for state in [
        PresenceState::Outside,
        PresenceState::Candidate,
        PresenceState::Suppressed,
    ] {
        let derived = ActiveSpot::derive(
            &party.root(EPOCH),
            &SpotStatus {
                spot: party.spot_ref(),
                epoch: EPOCH,
                state,
            },
        );
        assert!(matches!(derived, Err(PresenceError::NotInside)));
    }
    let inside = party.inside(EPOCH)?;
    assert_eq!(inside.spot(), party.spot_ref());
    assert_eq!(inside.epoch(), EPOCH);
    Ok(())
}

#[test]
fn a_cover_read_with_no_active_spot_matches_nothing() -> Result<(), TestError> {
    let mut rng = seeded_rng(21);
    let party = TestParty::new(&mut rng);
    let spot = party.inside(EPOCH)?;
    let mut board = TestBoard::default();
    board.post(&lease(now()?, &[&spot], &mut rng)?.lease);
    assert!(matched(&[], &board, &OwnRecords::default())?.is_empty());
    Ok(())
}

#[test]
fn another_spot_of_the_same_party_does_not_match() -> Result<(), TestError> {
    // The client rule: a member Inside Spot A learns nothing about Spot B's records.
    let mut rng = seeded_rng(22);
    let party = TestParty::new(&mut rng);
    let here = party.inside(EPOCH)?;
    let elsewhere = party.other_spot(&mut rng).inside(EPOCH)?;
    let mut board = TestBoard::default();
    board.post(&lease(now()?, &[&elsewhere], &mut rng)?.lease);
    assert!(matched(&[here], &board, &OwnRecords::default())?.is_empty());
    Ok(())
}

#[test]
fn own_records_are_skipped() -> Result<(), TestError> {
    let mut rng = seeded_rng(23);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let built = lease(now()?, &[&spot], &mut rng)?;
    let mut board = TestBoard::default();
    board.post(&built.lease);
    let own = OwnRecords::new(real_randomness(&built));
    assert!(matched(&[spot], &board, &own)?.is_empty());
    Ok(())
}

#[test]
fn forged_random_record_does_not_match() -> Result<(), TestError> {
    let mut rng = seeded_rng(24);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let mut board = TestBoard::default();
    let forged: Vec<Record> = (0..200)
        .map(|_| Record::from_bytes(&random_array(&mut rng)))
        .collect();
    board.inject(now()?, &forged);
    assert!(matched(&[spot], &board, &OwnRecords::default())?.is_empty());
    Ok(())
}

#[test]
fn record_with_a_valid_tag_but_forged_seal_does_not_match() -> Result<(), TestError> {
    // Only possible for someone holding K_tag, but the seal is checked anyway: the tag alone
    // never yields a knock.
    let mut rng = seeded_rng(25);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let randomness = RecordRandomness::from_bytes(random_array(&mut rng));
    let forged = Record {
        randomness,
        tag: record_tag(spot.tag_key(), now()?, &randomness)?,
        sealed: SealedBeacon::from_bytes(random_array(&mut rng)),
    };
    let mut board = TestBoard::default();
    board.inject(now()?, &[forged]);
    assert!(matched(&[spot], &board, &OwnRecords::default())?.is_empty());
    Ok(())
}

#[test]
fn record_replayed_into_a_later_segment_does_not_match() -> Result<(), TestError> {
    let mut rng = seeded_rng(26);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let earlier = now()?.previous().ok_or(TestError::Malformed("bucket"))?;
    let built = lease(earlier, &[&spot], &mut rng)?;
    let mut board = TestBoard::default();
    board.inject(now()?, &built.lease.records);
    assert!(matched(&[spot], &board, &OwnRecords::default())?.is_empty());
    Ok(())
}

#[test]
fn expired_segment_is_not_matched() -> Result<(), TestError> {
    // A server replaying a whole old segment: the tag binds the bucket, so the reader's own
    // clock decides. Three buckets back is still live, four is not.
    let mut rng = seeded_rng(27);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    for (back, expected) in [(3, 1), (4, 0)] {
        let start = now()?
            .checked_sub(BucketCount::new(back))
            .ok_or(TestError::Malformed("bucket"))?;
        let mut board = TestBoard::default();
        board.post(&lease(start, &[&spot], &mut rng)?.lease);
        let found = matched(core::slice::from_ref(&spot), &board, &OwnRecords::default())?;
        assert_eq!(found.len(), expected, "{back} buckets back");
    }
    Ok(())
}

#[test]
fn segment_more_than_one_bucket_ahead_is_not_matched() -> Result<(), TestError> {
    let mut rng = seeded_rng(28);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    for (ahead, expected) in [(1, 1), (2, 0)] {
        let start = now()?
            .checked_add(BucketCount::new(ahead))
            .ok_or(TestError::Malformed("bucket"))?;
        let mut board = TestBoard::default();
        board.post(&lease(start, &[&spot], &mut rng)?.lease);
        let found = matched(core::slice::from_ref(&spot), &board, &OwnRecords::default())?;
        assert_eq!(found.len(), expected, "{ahead} buckets ahead");
    }
    Ok(())
}

#[test]
fn record_repeated_in_a_page_is_reported_once() -> Result<(), TestError> {
    let mut rng = seeded_rng(29);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let built = lease(now()?, &[&spot], &mut rng)?;
    let mut board = TestBoard::default();
    board.post(&built.lease);
    board.post(&built.lease);
    assert_eq!(matched(&[spot], &board, &OwnRecords::default())?.len(), 1);
    Ok(())
}

#[test]
fn record_tagged_under_the_previous_epoch_needs_the_grace_period() -> Result<(), TestError> {
    let mut rng = seeded_rng(30);
    let party = TestParty::new(&mut rng);
    let previous = Epoch::new(EPOCH.get() - 1);
    let lagging_poster = party.inside(previous)?;
    let mut board = TestBoard::default();
    board.post(&lease(now()?, &[&lagging_poster], &mut rng)?.lease);

    let current_only = [party.inside(EPOCH)?];
    assert!(matched(&current_only, &board, &OwnRecords::default())?.is_empty());

    let with_grace = [party.inside(EPOCH)?, party.inside(previous)?];
    let found = matched(&with_grace, &board, &OwnRecords::default())?;
    assert_eq!(found.len(), 1);
    assert_eq!(found.first().map(|found| found.epoch), Some(previous));
    Ok(())
}

#[test]
fn after_a_removal_the_reader_holds_no_previous_epoch_keys() -> Result<(), TestError> {
    // The keys a reader derives come from `matching_epochs`; after a removal that is only
    // the current epoch, so a record under the removed member's last epoch does not match.
    let mut rng = seeded_rng(31);
    let party = TestParty::new(&mut rng);
    let previous = Epoch::new(EPOCH.get() - 1);
    let mut board = TestBoard::default();
    board.post(&lease(now()?, &[&party.inside(previous)?], &mut rng)?.lease);
    let epochs = matching_epochs(
        &EpochState {
            current: EPOCH,
            entered_at: now()?.start(),
            transition: EpochTransition::Removal,
        },
        now()?.start(),
    );
    let spots: Vec<ActiveSpot> = core::iter::once(epochs.current)
        .chain(epochs.previous)
        .map(|epoch| party.inside(epoch))
        .collect::<Result<_, _>>()?;
    assert!(matched(&spots, &board, &OwnRecords::default())?.is_empty());
    Ok(())
}

#[test]
fn record_of_an_older_epoch_never_matches() -> Result<(), TestError> {
    let mut rng = seeded_rng(32);
    let party = TestParty::new(&mut rng);
    let mut board = TestBoard::default();
    board.post(
        &lease(
            now()?,
            &[&party.inside(Epoch::new(EPOCH.get() - 2))?],
            &mut rng,
        )?
        .lease,
    );
    let with_grace = [
        party.inside(EPOCH)?,
        party.inside(Epoch::new(EPOCH.get() - 1))?,
    ];
    assert!(matched(&with_grace, &board, &OwnRecords::default())?.is_empty());
    Ok(())
}

#[test]
fn segment_counts_must_cover_the_records_exactly() -> Result<(), TestError> {
    let mut rng = seeded_rng(33);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let mut board = TestBoard::default();
    board.post(&lease(now()?, &[&spot], &mut rng)?.lease);
    let matcher = TrialMatcher::new(core::slice::from_ref(&spot), now()?);
    for delta in [-1_i64, 1] {
        let mut page = board.page()?;
        if let Some(segment) = page.segments.first_mut() {
            segment.record_count = u32::try_from(i64::from(segment.record_count) + delta)
                .map_err(|_e| TestError::Malformed("count"))?;
        }
        assert!(matches!(
            matcher.match_page(&page, &OwnRecords::default()),
            Err(PresenceError::Proto(ProtoError::OutOfRange { .. }))
        ));
    }
    Ok(())
}

#[test]
fn matches_keep_board_order_across_segments() -> Result<(), TestError> {
    let mut rng = seeded_rng(34);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let older = now()?.previous().ok_or(TestError::Malformed("bucket"))?;
    let first = lease(older, &[&spot], &mut rng)?;
    let second = lease(now()?, &[&spot], &mut rng)?;
    let mut board = TestBoard::default();
    board.post(&first.lease);
    board.post(&second.lease);
    let found: Vec<(Bucket, RecordRandomness)> = matched(&[spot], &board, &OwnRecords::default())?
        .iter()
        .map(|found| (found.start, found.record.randomness))
        .collect();
    let expected: Vec<(Bucket, RecordRandomness)> = real_randomness(&first)
        .into_iter()
        .map(|randomness| (older, randomness))
        .chain(
            real_randomness(&second)
                .into_iter()
                .map(|randomness| (now().unwrap_or(older), randomness)),
        )
        .collect();
    assert_eq!(found, expected);
    Ok(())
}

fn board_match(
    spot: &ActiveSpot,
    mailbox: u8,
    hint: Option<MeetupHint>,
) -> Result<BoardMatch, TestError> {
    Ok(BoardMatch {
        spot: spot.spot(),
        epoch: EPOCH,
        start: now()?,
        record: Record::from_bytes(&[mailbox; Record::SIZE]),
        beacon: BoardBeacon {
            knock_mailbox: MailboxId::from_bytes([mailbox; 16]),
            meetup_hint: hint,
        },
    })
}

fn mailboxes(targets: &[&BoardMatch]) -> Vec<u8> {
    targets
        .iter()
        .map(|target| target.beacon.knock_mailbox.as_bytes()[0])
        .collect()
}

#[test]
fn knocks_prefer_meetup_hints_and_stop_at_three() -> Result<(), TestError> {
    let mut rng = seeded_rng(35);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let hint = Some(MeetupHint::for_group(&GroupId::from_bytes([7; 32])));
    let matches = [
        board_match(&spot, 1, None)?,
        board_match(&spot, 2, hint)?,
        board_match(&spot, 3, None)?,
        board_match(&spot, 4, hint)?,
        board_match(&spot, 5, None)?,
    ];
    let targets = knock_targets(&matches, &KnockFilter::default());
    assert_eq!(targets.len(), MAX_KNOCKS_PER_ARRIVAL);
    assert_eq!(mailboxes(&targets), vec![2, 4, 1]);
    Ok(())
}

#[test]
fn knocked_mailboxes_and_own_meetups_are_not_knocked_again() -> Result<(), TestError> {
    let mut rng = seeded_rng(36);
    let spot = TestParty::new(&mut rng).inside(EPOCH)?;
    let ours = GroupId::from_bytes([7; 32]);
    let theirs = GroupId::from_bytes([8; 32]);
    let matches = [
        board_match(&spot, 1, None)?,
        board_match(&spot, 2, Some(MeetupHint::for_group(&ours)))?,
        board_match(&spot, 3, Some(MeetupHint::for_group(&theirs)))?,
        board_match(&spot, 4, None)?,
    ];
    let filter = KnockFilter {
        already_knocked: &[MailboxId::from_bytes([1; 16])],
        own_meetups: &[ours],
    };
    assert_eq!(mailboxes(&knock_targets(&matches, &filter)), vec![3, 4]);
    Ok(())
}
