// SPDX-License-Identifier: AGPL-3.0-only
//! Design G.3 and G.4 at the wake level, and the knock rules around them.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;

use pingers_geo::{Meters, PresenceState};
use pingers_presence::{
    Admission, EpochState, EpochTransition, KnockDecision, LEASE_DURATION, MIN_UPLOAD_SPACING,
    UploadTrigger,
};
use pingers_proto::{Epoch, ReadBoard};

use super::fakes::Call;
use super::{EPOCH, TestError, Town};
use crate::{
    KnockOutcome, KnockStatus, PushReason, UploadOutcome, WakeEnding, WakeNotice, WakeTrigger,
};

/// G.3: the OS reports the entry, two fixes confirm it, and the lease carries one real record
/// and three dummies. Nobody else is there, so nothing matches.
#[tokio::test]
async fn entering_alone_posts_one_real_record_among_dummies() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    let entry = alice.enter(town.spot("anchor")?, anchor).await?;

    let first = &entry.candidate;
    assert_eq!(first.transitions.len(), 1);
    assert_eq!(
        first.transitions.first().map(|change| change.transition.to),
        Some(PresenceState::Candidate)
    );
    // The first wake of a fresh install is a cover upload: four dummies.
    assert!(matches!(
        first.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::Wake,
            real_records: 0,
            ..
        }
    ));
    assert_eq!(
        first.next_wake,
        pingers_geo::SpotPresence::ENTRY_FIX_SPACING
    );

    let confirmed = &entry.confirmed;
    assert_eq!(
        confirmed.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::ConfirmedEntry,
            real_records: 1,
            read: ReadBoard::Read,
        }
    );
    assert_eq!(alice.real_records(), 1);
    assert!(confirmed.matched.is_empty());
    assert_eq!(confirmed.knocks_sent, 0);
    assert_eq!(confirmed.ending, WakeEnding::Completed);
    assert_eq!(confirmed.next_wake, pingers_presence::TARGET_UPLOAD_SPACING);
    assert_eq!(
        alice.platform.location_mode(),
        pingers_testkit::LocationMode::Dwell
    );
    // Group logs before the board sync (design B.4.7), the pool refilled after.
    let calls = alice.server.calls();
    assert_eq!(
        calls.get(calls.len().saturating_sub(3)),
        Some(&Call::PullLogs)
    );
    assert_eq!(calls.last(), Some(&Call::Refill));
    // The replaced lease left the board: only the newest four records are there.
    assert_eq!(town.world.lock().map_or(0, |world| world.record_count()), 4);
    Ok(())
}

/// G.4: Bob arrives, matches Alice's record and knocks; Alice's push wake opens the knock,
/// takes a fresh fix, and hands Bob to pingers-meetup-session; the Welcome reaches Bob's.
#[tokio::test]
async fn second_arrival_knocks_and_the_poster_admits_after_a_liveness_check()
-> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;

    let entry = bob.enter(spot, anchor).await?;
    // While Bob was only a Candidate he had no keys, so the board he read matched nothing.
    assert!(entry.candidate.matched.is_empty());
    let bob_arrives = entry.confirmed;
    assert_eq!(bob_arrives.matched, vec![anchor]);
    assert_eq!(bob_arrives.knocks_sent, 1);
    let sent = bob.state.snapshot().sent_knocks;
    let knock = sent.first().ok_or(TestError::Missing("sent knock"))?;
    assert_eq!(knock.status, KnockStatus::Waiting);
    assert_eq!(knock.epoch, EPOCH);
    let posted = town
        .world
        .lock()
        .map_or(0, |world| world.posts(&knock.knock_mailbox));
    assert_eq!(posted, 1);

    // Alice's last fix is under a minute old, so the only fix taken is the liveness fix.
    alice.platform.script_fix(spot.fix_inside(alice.now()));
    let answered = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert_eq!(
        answered.knock_decisions,
        vec![KnockOutcome {
            spot: anchor,
            decision: KnockDecision::Accept(Admission::CreateMeetup),
        }]
    );
    let liveness = alice.platform.fix_requests();
    assert_eq!(
        liveness.last().map(|request| request.quality),
        Some(pingers_testkit::FixQuality::High)
    );
    let admitted = alice.meetups.admitted();
    let admission = admitted.first().ok_or(TestError::Missing("admission"))?;
    assert_eq!(admission.spot, anchor);
    assert_eq!(admission.epoch, EPOCH);
    assert_eq!(admission.bucket, alice.now().bucket());
    assert_eq!(admission.knock.knocker, town.party.member("bob")?.leaf);
    // Not an upload wake: the push only made Alice read her mailbox.
    assert!(matches!(answered.upload, UploadOutcome::NotDue(_)));

    // A second read of the same mailbox does not decide the same knock again.
    alice.platform.script_fix(spot.fix_inside(alice.now()));
    let again = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert!(again.knock_decisions.is_empty());

    // pingers-meetup-session posts the Welcome to Bob's reply mailbox; Bob's push wake reads it.
    town.world
        .lock()
        .map_err(|_poisoned| TestError::Missing("world"))?
        .post(admission.knock.knock.reply_mailbox, b"welcome".to_vec());
    let joined = bob.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert_eq!(joined.welcomes, 1);
    let welcomes = bob.meetups.welcomes();
    assert_eq!(welcomes.first().map(|welcome| welcome.spot), Some(anchor));
    assert_eq!(welcomes.first().map(|welcome| welcome.epoch), Some(EPOCH));
    let status = bob
        .state
        .snapshot()
        .sent_knocks
        .first()
        .map(|sent| sent.status);
    assert_eq!(status, Some(KnockStatus::Answered));
    // Answered knocks are not read again.
    let read_again = bob.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert_eq!(read_again.welcomes, 0);
    Ok(())
}

/// A fresh fix outside the fence, though still within the exit buffer, declines (design B.4.4
/// step 4: a stale lease gives a decline, not a false Meetup).
#[tokio::test]
async fn poster_whose_fresh_fix_is_outside_the_spot_declines() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.enter(spot, anchor).await?;

    let drifted = spot.fix_at_distance(Meters::from_whole(170), alice.now())?;
    alice.platform.script_fix(drifted);
    let answered = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert_eq!(
        answered.knock_decisions,
        vec![KnockOutcome {
            spot: anchor,
            decision: KnockDecision::Decline,
        }]
    );
    assert!(alice.meetups.admitted().is_empty());
    Ok(())
}

/// No fix in time for the liveness check: decline.
#[tokio::test]
async fn poster_without_a_fresh_fix_declines() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.enter(spot, anchor).await?;

    let answered = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert_eq!(
        answered
            .knock_decisions
            .first()
            .map(|outcome| outcome.decision),
        Some(KnockDecision::Decline)
    );
    Ok(())
}

/// A poster already in a Meetup at the Spot adds the knocker to it.
#[tokio::test]
async fn poster_in_a_meetup_adds_the_knocker() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    let group = pingers_proto::GroupId::from_bytes([7; 32]);
    alice.meetups.join(anchor, group);
    alice.enter(spot, anchor).await?;
    bob.enter(spot, anchor).await?;

    alice.platform.script_fix(spot.fix_inside(alice.now()));
    let answered = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert_eq!(
        answered
            .knock_decisions
            .first()
            .map(|outcome| outcome.decision),
        Some(KnockDecision::Accept(Admission::AddToMeetup(group)))
    );
    // A Meetup is live, so the wake leaves the location mode to pingers-meetup-session.
    assert!(
        !alice
            .platform
            .location_mode_history()
            .contains(&pingers_testkit::LocationMode::Dwell)
    );
    Ok(())
}

/// Grace period (design B.4.7): Bob has processed a Commit Alice has not. He still matches her
/// record with the previous epoch's keys, and his knock names that epoch, so she can open it.
#[tokio::test]
async fn knock_during_the_grace_period_names_the_epoch_it_matched_under() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.parties.set_epoch(EpochState {
        current: Epoch::new(4),
        entered_at: town.now(),
        transition: EpochTransition::Other,
    });
    let arrives = bob.enter(spot, anchor).await?.confirmed;
    assert_eq!(arrives.knocks_sent, 1);
    let sent = bob.state.snapshot().sent_knocks;
    assert_eq!(sent.first().map(|knock| knock.epoch), Some(EPOCH));

    alice.platform.script_fix(spot.fix_inside(alice.now()));
    alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    let admitted = alice.meetups.admitted();
    let admission = admitted.first().ok_or(TestError::Missing("admission"))?;
    assert_eq!(admission.knock.knock.party_epoch, EPOCH);
    Ok(())
}

/// After a removal there is no grace: Bob, at the new epoch, cannot match Alice's record.
#[tokio::test]
async fn after_a_removal_the_previous_epoch_is_not_matched() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.parties.set_epoch(EpochState {
        current: Epoch::new(4),
        entered_at: town.now(),
        transition: EpochTransition::Removal,
    });
    let arrives = bob.enter(spot, anchor).await?.confirmed;
    assert!(arrives.matched.is_empty());
    assert_eq!(arrives.knocks_sent, 0);
    Ok(())
}

/// The poster must still hold the epoch of the record that was knocked: after a removal Commit
/// Alice drops the old keys and cannot open Bob's knock, so she decides nothing.
#[tokio::test]
async fn poster_past_a_removal_cannot_answer_a_knock_on_its_old_record() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.enter(spot, anchor).await?;

    alice.parties.set_epoch(EpochState {
        current: Epoch::new(4),
        entered_at: town.now(),
        transition: EpochTransition::Removal,
    });
    alice.platform.script_fix(spot.fix_inside(alice.now()));
    let woken = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert!(woken.knock_decisions.is_empty());
    assert!(alice.meetups.admitted().is_empty());
    // The Commit itself made her re-post, with the new epoch's record.
    assert!(matches!(
        woken.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::EpochChanged,
            real_records: 1,
            ..
        }
    ));
    let lease = alice.state.snapshot().leases;
    let real = lease
        .last()
        .and_then(|lease| lease.slots.iter().find_map(|slot| slot.real));
    assert_eq!(real.map(|record| record.epoch), Some(Epoch::new(4)));
    Ok(())
}

/// Nobody answers within a lease: one notice for the Spot (design B.4.4 step 7).
#[tokio::test]
async fn knock_unanswered_for_a_lease_raises_a_notice() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.enter(spot, anchor).await?;

    town.advance(LEASE_DURATION.saturating_sub(Duration::from_secs(1)));
    bob.platform.script_fix(spot.fix_inside(bob.now()));
    let before = bob.wake(WakeTrigger::Scheduled).await?;
    assert!(before.notices.is_empty());

    town.advance(Duration::from_secs(1));
    bob.platform.script_fix(spot.fix_inside(bob.now()));
    let after = bob.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(after.notices, vec![WakeNotice::UnansweredKnock(anchor)]);
    assert!(bob.state.snapshot().sent_knocks.is_empty());
    Ok(())
}

/// An answered knock raises no notice when it expires.
#[tokio::test]
async fn answered_knock_expires_quietly() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.enter(spot, anchor).await?;
    let reply = bob
        .state
        .snapshot()
        .sent_knocks
        .first()
        .map(|sent| sent.reply_mailbox)
        .ok_or(TestError::Missing("knock"))?;
    town.world
        .lock()
        .map_err(|_poisoned| TestError::Missing("world"))?
        .post(reply, b"welcome".to_vec());
    bob.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;

    town.advance(LEASE_DURATION);
    bob.platform.script_fix(spot.fix_inside(bob.now()));
    let later = bob.wake(WakeTrigger::Scheduled).await?;
    assert!(later.notices.is_empty());
    Ok(())
}

/// A record already knocked is not knocked again; once its poster refreshes it (a new
/// mailbox), it is (design B.4.4 step 7).
#[tokio::test]
async fn record_is_knocked_once_and_again_after_a_refresh() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.enter(spot, anchor).await?;

    town.advance(MIN_UPLOAD_SPACING);
    bob.platform.script_fix(spot.fix_inside(bob.now()));
    let resync = bob.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(resync.matched, vec![anchor]);
    assert_eq!(resync.knocks_sent, 0);

    alice.platform.script_fix(spot.fix_inside(alice.now()));
    alice.wake(WakeTrigger::Scheduled).await?;
    town.advance(MIN_UPLOAD_SPACING);
    bob.platform.script_fix(spot.fix_inside(bob.now()));
    let refreshed = bob.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(refreshed.knocks_sent, 1);
    Ok(())
}

/// Records from a Meetup this device is in need no knock.
#[tokio::test]
async fn fellow_meetup_members_are_not_knocked() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    let group = pingers_proto::GroupId::from_bytes([9; 32]);
    alice.meetups.join(anchor, group);
    bob.meetups.join(anchor, group);
    alice.enter(spot, anchor).await?;
    let arrives = bob.enter(spot, anchor).await?.confirmed;
    assert_eq!(arrives.matched, vec![anchor]);
    assert_eq!(arrives.knocks_sent, 0);
    Ok(())
}

/// With an empty mailbox pool, the Spot gets no real record (and no knock is possible).
#[tokio::test]
async fn without_a_pooled_mailbox_the_lease_is_all_dummies() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    alice.server.set_pool(0);
    let anchor = town.spot_ref("anchor")?;
    let entry = alice.enter(town.spot("anchor")?, anchor).await?;
    assert!(matches!(
        entry.confirmed.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::ConfirmedEntry,
            real_records: 0,
            ..
        }
    ));
    Ok(())
}

/// Bob has a knock mailbox but no reply mailbox: he matches but cannot knock yet.
#[tokio::test]
async fn without_a_reply_mailbox_no_knock_is_sent() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.server.set_pool(1);
    let arrives = bob.enter(spot, anchor).await?.confirmed;
    assert_eq!(arrives.matched, vec![anchor]);
    assert_eq!(arrives.knocks_sent, 0);
    assert!(bob.state.snapshot().sent_knocks.is_empty());
    Ok(())
}

/// Every page of the board is read and matched (design B.4.3 paging).
#[tokio::test]
async fn every_board_page_is_read() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    bob.server.set_page_size(2);
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    let arrives = bob.enter(spot, anchor).await?.confirmed;
    assert_eq!(arrives.matched, vec![anchor]);
    // Eight records in pages of two: the first comes with the upload, three more follow.
    let pages = bob
        .server
        .calls()
        .iter()
        .filter(|call| **call == Call::Page)
        .count();
    assert_eq!(pages, 6);
    Ok(())
}

/// Every Spot the device is confirmed inside gets its own real record (design B.4.2).
#[tokio::test]
async fn several_inside_spots_each_get_a_real_record() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    let bakery = town.spot_ref("bakery")?;
    // Make the bakery overlap the anchor so one position is inside both.
    let anchor_fence = town.spot("anchor")?.fence.clone();
    alice.parties.edit(|party| {
        for info in &mut party.spots {
            if info.spot == bakery.spot {
                info.fence = anchor_fence.clone();
            }
        }
    });
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    assert_eq!(alice.real_records(), 2);
    Ok(())
}
