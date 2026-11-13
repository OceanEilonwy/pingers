// SPDX-License-Identifier: AGPL-3.0-only
//! Lease building: shape, sealing, shuffling and slot choice (design B.4.2, B.4.5).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_crypto::{RevokeKey, revoke_commitment};
use pingers_proto::{Epoch, GroupId, Lease, MailboxId, MeetupHint};
use pingers_testkit::{TestRng, random_array, seeded_rng};
use pingers_time::{Bucket, UnixSeconds};
use proptest::prelude::*;

use crate::test_support::{FixedRng, TestBoard, TestError, TestParty};
use crate::{
    ActiveSpot, BuiltLease, LeaseInputs, OwnRecords, PresenceError, RealSlot, SlotCandidate,
    SlotContent, TrialMatcher, build_lease, choose_slots,
};

const EPOCH: Epoch = Epoch::new(4);

fn bucket() -> Result<Bucket, TestError> {
    Ok(Bucket::from_index(1_966_201)?)
}

/// `count` Spots of one Party, each Inside.
fn spots(count: usize, rng: &mut TestRng) -> Result<Vec<ActiveSpot>, TestError> {
    let party = TestParty::new(rng);
    (0..count)
        .map(|_| Ok(party.other_spot(rng).inside(EPOCH)?))
        .collect()
}

struct Built {
    built: BuiltLease,
    mailboxes: Vec<MailboxId>,
}

fn lease_for(
    spots: &[ActiveSpot],
    hint: Option<MeetupHint>,
    rng: &mut TestRng,
) -> Result<Built, TestError> {
    let mailboxes: Vec<MailboxId> = spots
        .iter()
        .map(|_| MailboxId::from_bytes(random_array(rng)))
        .collect();
    let real = spots
        .iter()
        .zip(&mailboxes)
        .map(|(spot, mailbox)| RealSlot {
            spot,
            knock_mailbox: *mailbox,
            meetup_hint: hint,
        })
        .collect();
    let revoke_key = RevokeKey::generate(rng);
    let built = build_lease(
        &LeaseInputs {
            start: bucket()?,
            real,
            revoke_key: &revoke_key,
        },
        rng,
    )?;
    Ok(Built { built, mailboxes })
}

fn fail(error: &TestError) -> TestCaseError {
    TestCaseError::fail(error.to_string())
}

proptest! {
    #[test]
    fn every_lease_is_four_records_whatever_the_real_count(
        real_count in 0_usize..=4,
        seed in any::<u64>(),
    ) {
        let mut rng = seeded_rng(seed);
        let spots = spots(real_count, &mut rng).map_err(|error| fail(&error))?;
        let Built { built, mailboxes } =
            lease_for(&spots, None, &mut rng).map_err(|error| fail(&error))?;
        prop_assert_eq!(built.lease.to_bytes().len(), Lease::SIZE);
        for (slot, record) in built.slots.iter().zip(&built.lease.records) {
            prop_assert_eq!(slot.randomness, record.randomness);
        }
        let mut real: Vec<[u8; 16]> = built
            .slots
            .iter()
            .filter_map(|slot| match slot.content {
                SlotContent::Real { knock_mailbox, .. } => Some(*knock_mailbox.as_bytes()),
                SlotContent::Dummy => None,
            })
            .collect();
        let mut expected: Vec<[u8; 16]> =
            mailboxes.iter().map(|mailbox| *mailbox.as_bytes()).collect();
        real.sort_unstable();
        expected.sort_unstable();
        prop_assert_eq!(real, expected);
    }

    #[test]
    fn records_never_share_randomness(seed in any::<u64>(), real_count in 0_usize..=4) {
        let mut rng = seeded_rng(seed);
        let spots = spots(real_count, &mut rng).map_err(|error| fail(&error))?;
        let Built { built, .. } =
            lease_for(&spots, None, &mut rng).map_err(|error| fail(&error))?;
        for (index, record) in built.lease.records.iter().enumerate() {
            for other in built.lease.records.iter().skip(index.saturating_add(1)) {
                prop_assert_ne!(record.randomness, other.randomness);
            }
        }
    }
}

fn is_real(content: SlotContent) -> bool {
    matches!(content, SlotContent::Real { .. })
}

#[test]
fn real_records_are_spread_evenly_over_the_four_positions() -> Result<(), TestError> {
    // With one real record per lease, its position is what an observer who could tell real
    // from dummy would see first; it must carry no pattern.
    let mut counts = [0_u32; 4];
    let mut rng = seeded_rng(11);
    let spots = spots(1, &mut rng)?;
    for _ in 0..800 {
        let Built { built, .. } = lease_for(&spots, None, &mut rng)?;
        for (count, slot) in counts.iter_mut().zip(&built.slots) {
            if is_real(slot.content) {
                *count = count.saturating_add(1);
            }
        }
    }
    // Each position expects 200 (standard deviation about 12); the seed is fixed.
    for count in counts {
        assert!((160..=240).contains(&count), "{counts:?}");
    }
    Ok(())
}

/// The mean byte value of each record field, for dummies and for real records.
#[derive(Default)]
struct FieldMeans {
    sums: [u64; 3],
    lengths: [u64; 3],
}

impl FieldMeans {
    fn add(&mut self, fields: [&[u8]; 3]) {
        for ((sum, length), bytes) in self.sums.iter_mut().zip(&mut self.lengths).zip(fields) {
            *sum = bytes
                .iter()
                .fold(*sum, |total, byte| total.saturating_add(u64::from(*byte)));
            *length = length.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        }
    }

    fn means(&self) -> Vec<u64> {
        self.sums
            .iter()
            .zip(self.lengths)
            .map(|(sum, length)| sum.checked_div(length).unwrap_or(0))
            .collect()
    }
}

#[test]
fn dummies_and_real_records_both_look_random() -> Result<(), TestError> {
    // A crude indistinguishability check: the mean byte of every field is near 127.5 for both
    // kinds. A zero-filled dummy, or a real record with a structured field, fails it.
    let mut rng = seeded_rng(12);
    let spots = spots(2, &mut rng)?;
    let mut dummy = FieldMeans::default();
    let mut real = FieldMeans::default();
    for _ in 0..100 {
        let Built { built, .. } = lease_for(&spots, None, &mut rng)?;
        for (slot, record) in built.slots.iter().zip(&built.lease.records) {
            let fields: [&[u8]; 3] = [
                record.randomness.as_bytes(),
                record.tag.as_bytes(),
                record.sealed.as_bytes(),
            ];
            if is_real(slot.content) {
                real.add(fields);
            } else {
                dummy.add(fields);
            }
        }
    }
    for mean in dummy.means().into_iter().chain(real.means()) {
        assert!((120..=135).contains(&mean), "mean byte {mean}");
    }
    Ok(())
}

#[test]
fn real_record_opens_to_its_beacon_under_its_spot() -> Result<(), TestError> {
    let mut rng = seeded_rng(13);
    let spots = spots(1, &mut rng)?;
    let hint = MeetupHint::for_group(&GroupId::from_bytes(random_array(&mut rng)));
    let Built { built, mailboxes } = lease_for(&spots, Some(hint), &mut rng)?;
    let mut board = TestBoard::default();
    board.post(&built.lease);
    let matches =
        TrialMatcher::new(&spots, bucket()?).match_page(&board.page()?, &OwnRecords::default())?;
    assert_eq!(matches.len(), 1);
    let found = matches.first().ok_or(TestError::Malformed("no match"))?;
    assert_eq!(Some(found.beacon.knock_mailbox), mailboxes.first().copied());
    assert_eq!(found.beacon.meetup_hint, Some(hint));
    assert_eq!(found.start, bucket()?);
    Ok(())
}

#[test]
fn lease_commits_to_its_revoke_key_and_bucket() -> Result<(), TestError> {
    let mut rng = seeded_rng(14);
    let revoke_key = RevokeKey::generate(&mut rng);
    let built = build_lease(
        &LeaseInputs {
            start: bucket()?,
            real: Vec::new(),
            revoke_key: &revoke_key,
        },
        &mut rng,
    )?;
    assert_eq!(
        built.lease.revoke_commitment,
        revoke_commitment(&revoke_key)
    );
    assert_eq!(built.lease.start, bucket()?);
    assert!(
        built
            .slots
            .iter()
            .all(|slot| slot.content == SlotContent::Dummy)
    );
    Ok(())
}

#[test]
fn five_real_slots_are_refused() -> Result<(), TestError> {
    let mut rng = seeded_rng(15);
    let spots = spots(5, &mut rng)?;
    assert!(matches!(
        lease_for(&spots, None, &mut rng),
        Err(TestError::Presence(PresenceError::TooManySlots))
    ));
    Ok(())
}

#[test]
fn randomness_is_consumed_in_the_documented_amounts() -> Result<(), TestError> {
    // 16 bytes per real record, 96 per dummy, then three 4-byte shuffle draws (none rejected
    // when the draws are zero).
    let mut setup = seeded_rng(16);
    let spots = spots(2, &mut setup)?;
    let revoke_key = RevokeKey::generate(&mut setup);
    let mut rng = FixedRng::new(&[0; 2 * 16 + 2 * 96 + 3 * 4]);
    let real = spots
        .iter()
        .map(|spot| RealSlot {
            spot,
            knock_mailbox: MailboxId::from_bytes([1; 16]),
            meetup_hint: None,
        })
        .collect();
    let built = build_lease(
        &LeaseInputs {
            start: bucket()?,
            real,
            revoke_key: &revoke_key,
        },
        &mut rng,
    )?;
    assert!(rng.exhausted_exactly());
    // All-zero draws swap item 3, then 2, then 1 with item 0, so the built order
    // (real 0, real 1, dummy, dummy) becomes (real 1, dummy, dummy, real 0).
    let kinds: Vec<bool> = built
        .slots
        .iter()
        .map(|slot| is_real(slot.content))
        .collect();
    assert_eq!(kinds, vec![true, false, false, true]);
    Ok(())
}

#[test]
fn most_recently_active_spots_win_the_slots() -> Result<(), TestError> {
    let mut rng = seeded_rng(17);
    let spots = spots(6, &mut rng)?;
    let activity = [None, Some(50), None, Some(90), Some(10), Some(70)];
    let candidates = spots
        .iter()
        .zip(activity)
        .map(|(spot, last)| SlotCandidate {
            spot,
            last_peer_activity: last.map(UnixSeconds::new),
        })
        .collect();
    let chosen: Vec<_> = choose_slots(candidates)
        .iter()
        .map(|candidate| candidate.spot.spot())
        .collect();
    let expected: Vec<_> = [3_usize, 5, 1, 4]
        .iter()
        .filter_map(|index| spots.get(*index).map(ActiveSpot::spot))
        .collect();
    assert_eq!(chosen, expected);
    Ok(())
}

#[test]
fn never_active_spots_keep_the_callers_order() -> Result<(), TestError> {
    let mut rng = seeded_rng(18);
    let spots = spots(5, &mut rng)?;
    let candidates = spots
        .iter()
        .map(|spot| SlotCandidate {
            spot,
            last_peer_activity: None,
        })
        .collect();
    let chosen: Vec<_> = choose_slots(candidates)
        .iter()
        .map(|candidate| candidate.spot.spot())
        .collect();
    let expected: Vec<_> = spots.iter().take(4).map(ActiveSpot::spot).collect();
    assert_eq!(chosen, expected);
    Ok(())
}
