// SPDX-License-Identifier: AGPL-3.0-only
//! Design G.3 to G.7 at the protocol level: devices build leases, the board stores them, readers
//! trial-match, knock, and decide. The network, MLS and pushes are left out; their place is
//! taken by [`TestBoard`], [`TestMailboxes`] and the decisions this crate returns.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_crypto::RevokeKey;
use pingers_geo::PresenceState;
use pingers_proto::{
    Epoch, GroupId, KeyPackageBytes, MailboxId, MeetupHint, RecordRandomness, SizeClass,
};
use pingers_testkit::{TestRng, random_array, seeded_rng};
use pingers_time::{Bucket, UnixSeconds};

use crate::test_support::{Member, TestBoard, TestError, TestMailboxes, TestParty, TestRoster};
use crate::{
    ActiveSpot, Admission, BoardMatch, BuiltLease, FreshFix, KnockContext, KnockDecision,
    KnockFilter, KnockInputs, KnockSituation, LeaseInputs, LivenessEvidence, OwnRecords, RealSlot,
    Sharing, SlotContent, TrialMatcher, UploadPlan, UploadTrigger, build_knock, build_lease,
    knock_targets, knock_unanswered, open_knock, plan_upload, verify_knock,
};

const EPOCH: Epoch = Epoch::new(3);

/// One phone's protocol state.
struct Device {
    member: Member,
    rng: TestRng,
    /// The Spot's keys, held only while Inside.
    spot: Option<ActiveSpot>,
    lease: Option<BuiltLease>,
    /// The knock mailbox of the current real record, if any.
    knock_mailbox: Option<MailboxId>,
    meetup: Option<GroupId>,
    knocked_mailboxes: Vec<MailboxId>,
    knocked_records: Vec<RecordRandomness>,
}

impl Device {
    fn new(leaf: u32, seed: u64) -> Self {
        let mut rng = seeded_rng(seed);
        Self {
            member: Member::new(leaf, &mut rng),
            rng,
            spot: None,
            lease: None,
            knock_mailbox: None,
            meetup: None,
            knocked_mailboxes: Vec::new(),
            knocked_records: Vec::new(),
        }
    }

    fn enter(&mut self, party: &TestParty) -> Result<(), TestError> {
        self.spot = Some(party.inside(EPOCH)?);
        Ok(())
    }

    /// The Spot leaves `Inside`: its keys are dropped.
    fn leave(&mut self) {
        self.spot = None;
    }

    /// `POST /board/sync`: replace the live lease, then read the board (post, then read).
    fn sync(&mut self, board: &mut TestBoard, now: Bucket) -> Result<Vec<BoardMatch>, TestError> {
        let revoke_key = RevokeKey::generate(&mut self.rng);
        let mailbox = MailboxId::from_bytes(random_array(&mut self.rng));
        let hint = self.meetup.as_ref().map(MeetupHint::for_group);
        let real = self
            .spot
            .iter()
            .map(|spot| RealSlot {
                spot,
                knock_mailbox: mailbox,
                meetup_hint: hint,
            })
            .collect();
        let built = build_lease(
            &LeaseInputs {
                start: now,
                real,
                revoke_key: &revoke_key,
            },
            &mut self.rng,
        )?;
        self.knock_mailbox = self.spot.as_ref().map(|_| mailbox);
        if let Some(old) = self.lease.replace(built) {
            board.revoke(&old.lease);
        }
        let lease = self.lease.as_ref().ok_or(TestError::Malformed("lease"))?;
        board.post(&lease.lease);
        self.read(board, now)
    }

    /// Reads and trial-matches the board without posting (a later page of the same sync).
    fn read(&self, board: &TestBoard, now: Bucket) -> Result<Vec<BoardMatch>, TestError> {
        let own = OwnRecords::new(
            self.lease
                .iter()
                .flat_map(|lease| lease.slots.iter().map(|slot| slot.randomness))
                .collect(),
        );
        Ok(TrialMatcher::new(self.spot.as_slice(), now).match_page(&board.page()?, &own)?)
    }

    /// The record randomness of this device's real record, if it has one.
    fn own_record(&self) -> Option<RecordRandomness> {
        self.lease.as_ref().and_then(|lease| {
            lease
                .slots
                .iter()
                .find(|slot| matches!(slot.content, SlotContent::Real { .. }))
                .map(|slot| slot.randomness)
        })
    }

    /// Knocks the chosen matches; returns how many knocks were posted.
    fn knock(
        &mut self,
        matches: &[BoardMatch],
        mailboxes: &mut TestMailboxes,
    ) -> Result<usize, TestError> {
        let spot = self
            .spot
            .as_ref()
            .ok_or(TestError::Malformed("not inside"))?;
        let own_meetups: Vec<GroupId> = self.meetup.iter().copied().collect();
        let targets = knock_targets(
            matches,
            &KnockFilter {
                already_knocked: &self.knocked_mailboxes,
                own_meetups: &own_meetups,
            },
        );
        let own_record = self.own_record();
        for target in &targets {
            let reply = MailboxId::from_bytes(random_array(&mut self.rng));
            let knock = build_knock(
                &KnockInputs {
                    matched: target,
                    spot,
                    own_record,
                    own_leaf: self.member.leaf,
                    key_package: KeyPackageBytes::from_bytes(vec![0x4b; 200]),
                    reply_mailbox: reply,
                    meetup_subscription: None,
                    signing_key: &self.member.signing_key,
                    class: SizeClass::Bytes1024,
                },
                &mut self.rng,
            )?;
            mailboxes.post(&knock);
            self.knocked_mailboxes.push(target.beacon.knock_mailbox);
            self.knocked_records.push(target.record.randomness);
        }
        Ok(targets.len())
    }

    /// Woken for its knock mailbox: open, verify and decide on every knock in it.
    fn answer(
        &self,
        mailboxes: &TestMailboxes,
        roster: &TestRoster,
        evidence: LivenessEvidence,
    ) -> Result<Vec<KnockDecision>, TestError> {
        let (Some(spot), Some(mailbox), Some(own_record), Some(lease)) = (
            self.spot.as_ref(),
            self.knock_mailbox,
            self.own_record(),
            self.lease.as_ref(),
        ) else {
            // Keys dropped on leaving: nothing can be opened, so nothing is answered.
            return Ok(Vec::new());
        };
        let context = KnockContext {
            spot,
            start: lease.lease.start,
            own_record,
        };
        mailboxes
            .read(&mailbox)
            .iter()
            .map(|post| {
                let verified = verify_knock(&context, open_knock(&context, post)?, roster)?;
                Ok(crate::decide_knock(
                    &verified,
                    &evidence,
                    &KnockSituation {
                        own_record: &own_record,
                        knocked_records: &self.knocked_records,
                        current_meetup: self.meetup,
                    },
                ))
            })
            .collect()
    }
}

const LIVE: LivenessEvidence = LivenessEvidence {
    spot_state: PresenceState::Inside,
    fresh_fix: FreshFix::InsideSpot,
    sharing: Sharing::Allowed,
};

const GONE: LivenessEvidence = LivenessEvidence {
    spot_state: PresenceState::Outside,
    fresh_fix: FreshFix::OutsideSpot,
    sharing: Sharing::Allowed,
};

struct World {
    party: TestParty,
    board: TestBoard,
    mailboxes: TestMailboxes,
    roster: TestRoster,
    alice: Device,
    bob: Device,
    carol: Device,
    group: GroupId,
}

impl World {
    fn new() -> Self {
        let mut rng = seeded_rng(60);
        let party = TestParty::new(&mut rng);
        let alice = Device::new(0, 61);
        let bob = Device::new(1, 62);
        let carol = Device::new(2, 63);
        let roster = TestRoster {
            current: EPOCH,
            members: vec![
                alice.member.entry(Epoch::new(0), None),
                bob.member.entry(Epoch::new(0), None),
                carol.member.entry(Epoch::new(0), None),
            ],
        };
        Self {
            party,
            board: TestBoard::default(),
            mailboxes: TestMailboxes::default(),
            roster,
            alice,
            bob,
            carol,
            group: GroupId::from_bytes(random_array(&mut rng)),
        }
    }
}

fn bucket(offset: u64) -> Result<Bucket, TestError> {
    Ok(Bucket::from_index(1_980_000_u64.saturating_add(offset))?)
}

/// G.3, then G.4: Alice enters alone; Bob enters, matches her record, knocks; she creates the
/// Meetup. Returns the world with both in it.
fn alice_and_bob_meet() -> Result<World, TestError> {
    let mut world = World::new();
    world.alice.enter(&world.party)?;
    let alone = world.alice.sync(&mut world.board, bucket(0)?)?;
    assert!(
        alone.is_empty(),
        "G.3: Alice is alone; her own record is skipped"
    );

    world.bob.enter(&world.party)?;
    let found = world.bob.sync(&mut world.board, bucket(0)?)?;
    assert_eq!(found.len(), 1, "G.4: Bob matches exactly Alice's record");
    let alice_record = found.first().ok_or(TestError::Malformed("match"))?;
    assert_eq!(
        Some(alice_record.beacon.knock_mailbox),
        world.alice.knock_mailbox
    );
    assert_eq!(alice_record.beacon.meetup_hint, None);
    assert_eq!(world.bob.knock(&found, &mut world.mailboxes)?, 1);

    let decisions = world.alice.answer(&world.mailboxes, &world.roster, LIVE)?;
    assert_eq!(
        decisions,
        vec![KnockDecision::Accept(Admission::CreateMeetup)]
    );
    world.alice.meetup = Some(world.group);
    world.bob.meetup = Some(world.group);
    Ok(world)
}

#[test]
fn g3_alone_in_a_spot_uploads_a_lease_that_matches_nothing() -> Result<(), TestError> {
    let mut world = World::new();
    world.alice.enter(&world.party)?;
    assert_eq!(
        plan_upload(
            UploadTrigger::ConfirmedEntry,
            Some(UnixSeconds::new(1)),
            UnixSeconds::new(2)
        ),
        UploadPlan::UploadNow
    );
    assert!(world.alice.sync(&mut world.board, bucket(0)?)?.is_empty());
    let page = world.board.page()?;
    assert_eq!(page.records.len(), 4, "one real record and three dummies");
    // Nobody else Inside: a reader Inside another Spot of the Party sees nothing either.
    let elsewhere = world.party.other_spot(&mut seeded_rng(64)).inside(EPOCH)?;
    let matches = TrialMatcher::new(core::slice::from_ref(&elsewhere), bucket(0)?)
        .match_page(&page, &OwnRecords::default())?;
    assert!(matches.is_empty());
    Ok(())
}

#[test]
fn g4_second_person_enters_and_a_meetup_is_created() -> Result<(), TestError> {
    let world = alice_and_bob_meet()?;
    assert_eq!(world.alice.meetup, Some(world.group));
    Ok(())
}

#[test]
fn g4_a_stale_poster_declines_and_the_knock_goes_unanswered() -> Result<(), TestError> {
    let mut world = World::new();
    world.alice.enter(&world.party)?;
    world.alice.sync(&mut world.board, bucket(0)?)?;
    world.bob.enter(&world.party)?;
    let found = world.bob.sync(&mut world.board, bucket(1)?)?;
    world.bob.knock(&found, &mut world.mailboxes)?;
    // Alice's lease is still live, but her fresh fix is outside: silence.
    let decisions = world.alice.answer(&world.mailboxes, &world.roster, GONE)?;
    assert_eq!(decisions, vec![KnockDecision::Decline]);
    let sent = bucket(1)?.start();
    let an_hour = sent
        .checked_add(crate::LEASE_DURATION)
        .ok_or(TestError::Malformed("time"))?;
    assert!(knock_unanswered(sent, an_hour));
    Ok(())
}

#[test]
fn g4_both_knock_at_once_and_exactly_one_creates_the_meetup() -> Result<(), TestError> {
    let mut world = World::new();
    world.alice.enter(&world.party)?;
    world.bob.enter(&world.party)?;
    world.alice.sync(&mut world.board, bucket(0)?)?;
    let bob_found = world.bob.sync(&mut world.board, bucket(0)?)?;
    // Alice reads the next page, which now holds Bob's record too.
    let alice_found = world.alice.read(&world.board, bucket(0)?)?;
    assert_eq!((alice_found.len(), bob_found.len()), (1, 1));
    world.alice.knock(&alice_found, &mut world.mailboxes)?;
    world.bob.knock(&bob_found, &mut world.mailboxes)?;
    let alice = world.alice.answer(&world.mailboxes, &world.roster, LIVE)?;
    let bob = world.bob.answer(&world.mailboxes, &world.roster, LIVE)?;
    let creators = alice
        .iter()
        .chain(&bob)
        .filter(|decision| **decision == KnockDecision::Accept(Admission::CreateMeetup))
        .count();
    let waiting = alice
        .iter()
        .chain(&bob)
        .filter(|decision| **decision == KnockDecision::WaitForPeer)
        .count();
    assert_eq!((creators, waiting), (1, 1));
    Ok(())
}

#[test]
fn g5_third_person_knocks_both_and_both_offer_to_add_her() -> Result<(), TestError> {
    let mut world = alice_and_bob_meet()?;
    // Both re-post with the Meetup hint.
    world.alice.sync(&mut world.board, bucket(1)?)?;
    let bob_sees = world.bob.sync(&mut world.board, bucket(1)?)?;
    assert_eq!(
        world.bob.knock(&bob_sees, &mut world.mailboxes)?,
        0,
        "fellow Meetup members do not knock each other"
    );

    world.carol.enter(&world.party)?;
    let found = world.carol.sync(&mut world.board, bucket(1)?)?;
    let hint = MeetupHint::for_group(&world.group);
    assert_eq!(found.len(), 2);
    assert!(
        found
            .iter()
            .all(|found| found.beacon.meetup_hint == Some(hint))
    );
    assert_eq!(world.carol.knock(&found, &mut world.mailboxes)?, 2);

    // Both add her; the delivery service accepts one commit (G.5, 409 for the other).
    let expected = vec![KnockDecision::Accept(Admission::AddToMeetup(world.group))];
    assert_eq!(
        world.alice.answer(&world.mailboxes, &world.roster, LIVE)?,
        expected
    );
    assert_eq!(
        world.bob.answer(&world.mailboxes, &world.roster, LIVE)?,
        expected
    );
    Ok(())
}

#[test]
fn g6_someone_leaves_and_disappears_from_the_board() -> Result<(), TestError> {
    let mut world = alice_and_bob_meet()?;
    world.bob.leave();
    assert_eq!(
        plan_upload(
            UploadTrigger::ExitWithRealRecord,
            Some(UnixSeconds::new(1)),
            UnixSeconds::new(2)
        ),
        UploadPlan::UploadNow
    );
    // Bob's replacement lease is all dummies, and he matches nothing: no keys.
    assert!(world.bob.sync(&mut world.board, bucket(1)?)?.is_empty());
    assert_eq!(world.bob.own_record(), None);

    world.carol.enter(&world.party)?;
    let found = world.carol.sync(&mut world.board, bucket(1)?)?;
    assert_eq!(found.len(), 1, "only Alice's record is left");
    assert_eq!(
        found.first().map(|found| found.beacon.knock_mailbox),
        world.alice.knock_mailbox
    );
    Ok(())
}

#[test]
fn g6_a_knock_reaching_someone_who_left_is_never_answered() -> Result<(), TestError> {
    let mut world = World::new();
    world.alice.enter(&world.party)?;
    world.alice.sync(&mut world.board, bucket(0)?)?;
    world.bob.enter(&world.party)?;
    let found = world.bob.sync(&mut world.board, bucket(0)?)?;
    world.bob.knock(&found, &mut world.mailboxes)?;
    // Alice left before the knock reached her: keys gone, nothing opens.
    world.alice.leave();
    assert!(
        world
            .alice
            .answer(&world.mailboxes, &world.roster, LIVE)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn g7_everyone_leaves_and_a_newcomer_finds_nothing() -> Result<(), TestError> {
    let mut world = alice_and_bob_meet()?;
    world.carol.enter(&world.party)?;
    world.carol.sync(&mut world.board, bucket(1)?)?;
    for device in [&mut world.alice, &mut world.bob, &mut world.carol] {
        device.leave();
        assert!(device.sync(&mut world.board, bucket(2)?)?.is_empty());
    }
    let mut dave = Device::new(3, 65);
    dave.enter(&world.party)?;
    assert!(dave.sync(&mut world.board, bucket(2)?)?.is_empty());
    assert_eq!(
        world.board.page()?.records.len(),
        16,
        "four all-dummy leases remain"
    );
    Ok(())
}

#[test]
fn g7_records_left_behind_expire_after_four_buckets() -> Result<(), TestError> {
    // Alice's phone died while Inside: her lease is never replaced. Dave still matches it for
    // the rest of its four buckets, and not after.
    let mut world = World::new();
    world.alice.enter(&world.party)?;
    world.alice.sync(&mut world.board, bucket(0)?)?;
    let mut dave = Device::new(3, 66);
    dave.enter(&world.party)?;
    assert_eq!(dave.sync(&mut world.board, bucket(3)?)?.len(), 1);
    assert!(dave.sync(&mut world.board, bucket(4)?)?.is_empty());
    Ok(())
}
