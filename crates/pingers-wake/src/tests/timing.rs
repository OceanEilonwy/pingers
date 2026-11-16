// SPDX-License-Identifier: AGPL-3.0-only
//! The budget, the cover cadence, Commits, geofence re-selection and restarts.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;

use pingers_geo::{GeofenceCapacity, Meters, PresenceState};
use pingers_presence::{
    DataMode, EpochState, EpochTransition, KnockDecision, MIN_UPLOAD_SPACING, UploadTrigger,
};
use pingers_proto::{Epoch, ReadBoard};

use super::fakes::{Call, CallKind, Effect};
use super::{BUDGET, TestError, Town};
use crate::{
    GeofenceEvent, GeofenceTransition, PortError, PushReason, RETRY_AFTER, UploadOutcome,
    WakeEnding, WakeState, WakeStep, WakeTrigger, ports::RegionId,
};

/// The answer to the upload never comes and the budget runs out: the lease the server may now
/// hold is remembered next to the old one, nothing after the upload runs, and the next wake
/// comes soon. A knock on the new lease's record is still answered.
#[tokio::test]
async fn budget_running_out_mid_upload_keeps_state_consistent() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    let uploads_before = alice.server.uploaded().len();

    alice.server.disturb(
        CallKind::Upload,
        Effect::Hang(BUDGET.saturating_add(Duration::from_secs(1))),
    );
    alice.server.take_calls();
    let cut = alice
        .wake(WakeTrigger::Push(PushReason::GroupCommit))
        .await?;
    assert_eq!(cut.ending, WakeEnding::OutOfBudget(WakeStep::Upload));
    assert_eq!(
        cut.upload,
        UploadOutcome::Interrupted(UploadTrigger::EpochChanged)
    );
    assert_eq!(cut.next_wake, RETRY_AFTER);
    assert_eq!(
        alice.platform.pending_wake().map(|wake| wake.after),
        Some(RETRY_AFTER)
    );
    assert!(!alice.server.calls().contains(&Call::Refill));
    assert_eq!(alice.board().leases.len(), 2);
    // The server did store it (the answer was lost, not the request).
    assert_eq!(
        alice.server.uploaded().len(),
        uploads_before.saturating_add(1)
    );

    // Bob matches the record of the lease whose answer Alice never saw, and she answers.
    alice.server.calm();
    bob.enter(spot, anchor).await?;
    // Over a minute since Alice's last fix: a routine fix, then the liveness fix.
    alice
        .platform
        .script_fixes([spot.fix_inside(alice.now()), spot.fix_inside(alice.now())]);
    let answered = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert!(matches!(
        answered
            .knock_decisions
            .first()
            .map(|outcome| outcome.decision),
        Some(KnockDecision::Accept(_))
    ));
    Ok(())
}

/// The upload completes but uses up the budget: the upload counts, later steps are skipped.
#[tokio::test]
async fn budget_running_out_after_the_upload_skips_the_rest() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    alice.server.disturb(CallKind::Upload, Effect::Slow(BUDGET));
    alice
        .platform
        .script_fix(town.spot("canal")?.fix_outside(alice.now())?);
    let woken = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(matches!(woken.upload, UploadOutcome::Uploaded { .. }));
    assert_eq!(
        woken.ending,
        WakeEnding::OutOfBudget(WakeStep::PushRegistration)
    );
    assert_eq!(alice.board().leases.len(), 1);
    assert_eq!(
        alice.board().last_upload,
        Some(
            alice
                .now()
                .checked_sub(BUDGET)
                .ok_or(TestError::Missing("time"))?
        )
    );
    Ok(())
}

/// A fix that never comes within the budget: the wake goes on without it and makes no request.
#[tokio::test]
async fn budget_running_out_during_the_fix_skips_the_network() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    alice.budget = Duration::ZERO;
    let woken = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(woken.ending, WakeEnding::OutOfBudget(WakeStep::Fix));
    assert_eq!(
        woken.upload,
        UploadOutcome::Interrupted(UploadTrigger::Wake)
    );
    assert!(alice.server.calls().is_empty());
    assert_eq!(
        alice.platform.pending_wake().map(|wake| wake.after),
        Some(RETRY_AFTER)
    );
    Ok(())
}

/// A failed group-log pull stops the network part: no board sync on stale epochs.
#[tokio::test]
async fn failed_group_log_pull_skips_the_board_sync() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    alice.server.disturb(CallKind::PullLogs, Effect::Fail);
    alice
        .platform
        .script_fix(town.spot("canal")?.fix_outside(alice.now())?);
    let woken = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        woken.ending,
        WakeEnding::PortFailed {
            step: WakeStep::GroupLogs,
            error: PortError::Unreachable,
        }
    );
    assert_eq!(alice.server.calls(), vec![Call::PullLogs]);
    assert!(alice.board().leases.is_empty());
    Ok(())
}

/// A wake within the cover spacing, with nothing to read and no Commit, sends nothing.
#[tokio::test]
async fn quiet_wake_makes_no_request() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let away = town.spot("canal")?;
    alice.platform.script_fix(away.fix_outside(alice.now())?);
    alice.wake(WakeTrigger::Scheduled).await?;
    alice.server.take_calls();

    town.advance(MIN_UPLOAD_SPACING.saturating_sub(Duration::from_secs(1)));
    alice.platform.script_fix(away.fix_outside(alice.now())?);
    let quiet = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        quiet.upload,
        UploadOutcome::NotDue(
            alice
                .now()
                .checked_add(Duration::from_secs(1))
                .ok_or(TestError::Missing("time"))?
        )
    );
    assert!(alice.server.calls().is_empty());
    // The next wake comes when the target spacing is reached.
    assert_eq!(
        quiet.next_wake,
        pingers_presence::TARGET_UPLOAD_SPACING
            .saturating_sub(MIN_UPLOAD_SPACING.saturating_sub(Duration::from_secs(1)))
    );

    town.advance(Duration::from_secs(1));
    alice.platform.script_fix(away.fix_outside(alice.now())?);
    let due = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(matches!(
        due.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::Wake,
            ..
        }
    ));
    Ok(())
}

/// A Commit push re-posts at once, ignoring the spacing, even with no real record (the
/// presence author's rule: post-Commit uploads must not reveal who is in a Spot).
#[tokio::test]
async fn commit_push_reposts_without_a_real_record() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let away = town.spot("canal")?;
    alice.platform.script_fix(away.fix_outside(alice.now())?);
    alice.wake(WakeTrigger::Scheduled).await?;

    town.advance(Duration::from_mins(1));
    alice.settings.data_mode = DataMode::LowData;
    let commit = alice
        .wake(WakeTrigger::Push(PushReason::GroupCommit))
        .await?;
    assert_eq!(
        commit.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::EpochChanged,
            real_records: 0,
            read: ReadBoard::Read,
        }
    );
    Ok(())
}

/// An epoch change seen in the Party view (another process pulled the logs) also re-posts.
#[tokio::test]
async fn epoch_change_in_the_party_view_reposts() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let away = town.spot("canal")?;
    alice.platform.script_fix(away.fix_outside(alice.now())?);
    alice.wake(WakeTrigger::Scheduled).await?;

    town.advance(Duration::from_mins(1));
    alice.parties.set_epoch(EpochState {
        current: Epoch::new(4),
        entered_at: town.now(),
        transition: EpochTransition::Other,
    });
    let changed = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(matches!(
        changed.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::EpochChanged,
            ..
        }
    ));
    // Seen once: the next wake is an ordinary one.
    town.advance(Duration::from_mins(1));
    let after = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(matches!(after.upload, UploadOutcome::NotDue(_)));
    Ok(())
}

/// "Low data" skips the board on a routine cover upload (design B.4.3).
#[tokio::test]
async fn low_data_cover_upload_skips_the_board() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    alice.settings.data_mode = DataMode::LowData;
    alice
        .platform
        .script_fix(town.spot("canal")?.fix_outside(alice.now())?);
    let cover = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(matches!(
        cover.upload,
        UploadOutcome::Uploaded {
            read: ReadBoard::Skip,
            ..
        }
    ));
    Ok(())
}

/// With room for two Spots, the nearest two and a horizon are registered; moving within the
/// horizon changes nothing; leaving it chooses again around the new position.
#[tokio::test]
async fn geofences_are_chosen_again_after_moving() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    alice.settings.geofence_capacity = GeofenceCapacity::new(2);
    let anchor = town.spot("anchor")?;
    let bakery = town.spot("bakery")?;
    let canal = town.spot("canal")?;

    alice
        .wake(WakeTrigger::Fix(anchor.fix_inside(alice.now())))
        .await?;
    let first = alice.platform.os_geofences();
    let centres = |regions: &[pingers_testkit::Geofence]| -> Vec<pingers_geo::Coordinate> {
        regions
            .iter()
            .filter(|region| region.id == "spot")
            .map(|region| region.region.center())
            .collect()
    };
    assert_eq!(
        centres(&first),
        vec![
            anchor.fence.enclosing_circle().center(),
            bakery.fence.enclosing_circle().center()
        ]
    );
    assert_eq!(
        first.iter().filter(|region| region.id == "horizon").count(),
        1
    );
    assert_eq!(alice.platform.geofence_updates(), 1);

    town.advance(Duration::from_secs(5));
    let nearby = anchor.fix_at_distance(Meters::from_whole(300), alice.now())?;
    alice.wake(WakeTrigger::Fix(nearby)).await?;
    assert_eq!(alice.platform.geofence_updates(), 1);

    town.advance(Duration::from_secs(5));
    alice
        .wake(WakeTrigger::Fix(canal.fix_inside(alice.now())))
        .await?;
    assert_eq!(alice.platform.geofence_updates(), 2);
    assert_eq!(
        centres(&alice.platform.os_geofences()),
        vec![
            canal.fence.enclosing_circle().center(),
            bakery.fence.enclosing_circle().center()
        ]
    );
    Ok(())
}

/// The OS reporting a horizon exit chooses again even before a fix shows the move.
#[tokio::test]
async fn horizon_exit_chooses_geofences_again() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    alice.settings.geofence_capacity = GeofenceCapacity::new(2);
    let anchor = town.spot("anchor")?;
    alice
        .wake(WakeTrigger::Fix(anchor.fix_inside(alice.now())))
        .await?;
    town.advance(Duration::from_secs(5));
    alice
        .wake(WakeTrigger::Geofence(GeofenceEvent {
            region: RegionId::Horizon,
            transition: GeofenceTransition::Exit,
            at: alice.now(),
        }))
        .await?;
    assert_eq!(alice.platform.geofence_updates(), 2);
    Ok(())
}

/// A Spot deleted from the Party leaves the registered geofences, and its presence with it.
#[tokio::test]
async fn deleted_spot_leaves_the_geofences() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot("anchor")?;
    alice
        .wake(WakeTrigger::Fix(anchor.fix_inside(alice.now())))
        .await?;
    assert_eq!(alice.platform.os_geofences().len(), 3);

    let canal = town.spot_ref("canal")?;
    alice
        .parties
        .edit(|party| party.spots.retain(|info| info.spot != canal.spot));
    town.advance(Duration::from_secs(5));
    alice
        .wake(WakeTrigger::Fix(anchor.fix_inside(alice.now())))
        .await?;
    assert_eq!(alice.platform.os_geofences().len(), 2);
    assert_eq!(alice.state.snapshot().spots.len(), 2);
    Ok(())
}

/// The restart case: Alice is Inside with a real record when the process dies; she walks away
/// and the restarted process sees one fix beyond the exit buffer. pingers-geo alone would keep
/// her Inside (one fix is not enough, the exit evidence was not stored) for up to 60 minutes;
/// the wake does not trust the restored Inside, so her real record is replaced at once.
#[tokio::test]
async fn after_a_restart_a_departed_device_stops_posting_at_once() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    assert_eq!(alice.real_records(), 1);

    alice.state = WakeState::restore(alice.state.snapshot());
    town.advance(Duration::from_mins(5));
    alice.platform.script_fix(spot.fix_outside(alice.now())?);
    let restarted = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(
        restarted.transitions.is_empty(),
        "pingers-geo still says Inside"
    );
    assert_eq!(
        restarted.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::ExitWithRealRecord,
            real_records: 0,
            read: ReadBoard::Read,
        }
    );
    assert_eq!(alice.real_records(), 0);
    assert_eq!(
        alice.platform.location_mode(),
        pingers_testkit::LocationMode::Idle
    );
    // It asks again soon, for the fix that completes the exit.
    assert_eq!(
        restarted.next_wake,
        pingers_geo::SpotPresence::ENTRY_FIX_SPACING
    );
    town.advance(pingers_geo::SpotPresence::ENTRY_FIX_SPACING);
    alice.platform.script_fix(spot.fix_outside(alice.now())?);
    let gone = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        gone.transitions.first().map(|change| change.transition.to),
        Some(PresenceState::Outside)
    );
    Ok(())
}

/// After a restart inside the Spot, a fix within the exit buffer confirms it and posting goes
/// on as before.
#[tokio::test]
async fn after_a_restart_a_fix_inside_confirms_the_spot() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;

    alice.state = WakeState::restore(alice.state.snapshot());
    town.advance(MIN_UPLOAD_SPACING);
    // In the band between the fence and the exit buffer: still a confirming fix.
    let band = spot.fix_at_distance(Meters::from_whole(170), alice.now())?;
    alice.platform.script_fix(band);
    let restarted = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        restarted.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::Wake,
            real_records: 1,
            read: ReadBoard::Read,
        }
    );
    Ok(())
}

/// A wake with no fix after a restart: the restored Inside is not trusted, so the record goes.
#[tokio::test]
async fn after_a_restart_without_a_fix_the_record_is_withdrawn() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    alice.enter(town.spot("anchor")?, anchor).await?;
    alice.state = WakeState::restore(alice.state.snapshot());
    let restarted = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(matches!(
        restarted.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::ExitWithRealRecord,
            ..
        }
    ));
    Ok(())
}

/// Inside without a confirming fix for 60 minutes times out (design F.2): a refreshed real
/// record is replaced at once.
#[tokio::test]
async fn inside_timeout_withdraws_the_record() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    alice.enter(town.spot("anchor")?, anchor).await?;
    let refresh_after = pingers_presence::TARGET_UPLOAD_SPACING;
    town.advance(refresh_after);
    let refreshed = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(matches!(
        refreshed.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::Wake,
            real_records: 1,
            ..
        }
    ));
    town.advance(pingers_geo::SpotPresence::INSIDE_TIMEOUT.saturating_sub(refresh_after));
    let timed_out = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        timed_out
            .transitions
            .first()
            .map(|change| change.transition.to),
        Some(PresenceState::Outside)
    );
    assert!(matches!(
        timed_out.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::ExitWithRealRecord,
            real_records: 0,
            ..
        }
    ));
    Ok(())
}

/// The OS dwell event confirms a circle Spot without a second fix (design F.2).
#[tokio::test]
async fn os_dwell_confirms_entry() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.platform.script_fix(spot.fix_inside(alice.now()));
    alice
        .wake(WakeTrigger::Geofence(GeofenceEvent {
            region: RegionId::Spot(anchor),
            transition: GeofenceTransition::Enter,
            at: alice.now(),
        }))
        .await?;
    town.advance(Duration::from_secs(5));
    let dwell = alice
        .wake(WakeTrigger::Geofence(GeofenceEvent {
            region: RegionId::Spot(anchor),
            transition: GeofenceTransition::Dwell,
            at: alice.now(),
        }))
        .await?;
    assert!(matches!(
        dwell.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::ConfirmedEntry,
            real_records: 1,
            ..
        }
    ));
    Ok(())
}

/// The OS exit event plus one fix beyond the buffer ends Inside, and the record goes at once.
#[tokio::test]
async fn os_exit_with_a_fix_beyond_withdraws_the_record() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    town.advance(Duration::from_mins(2));
    alice.platform.script_fix(spot.fix_outside(alice.now())?);
    let left = alice
        .wake(WakeTrigger::Geofence(GeofenceEvent {
            region: RegionId::Spot(anchor),
            transition: GeofenceTransition::Exit,
            at: alice.now(),
        }))
        .await?;
    assert_eq!(
        left.transitions.first().map(|change| change.transition.to),
        Some(PresenceState::Outside)
    );
    assert!(matches!(
        left.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::ExitWithRealRecord,
            real_records: 0,
            ..
        }
    ));
    Ok(())
}

/// "No wake-up pushes": mailboxes are read on every wake, push or not.
#[tokio::test]
async fn without_pushes_every_wake_reads_the_mailboxes() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    alice.settings.mailbox_reads = crate::MailboxReads::EveryWake;
    let anchor = town.spot_ref("anchor")?;
    alice.enter(town.spot("anchor")?, anchor).await?;
    let reads = alice
        .server
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::ReadMailbox(_)))
        .count();
    assert_eq!(reads, 1);
    Ok(())
}

/// A Commit push while the Party view already shows the new epoch is still an epoch change.
#[tokio::test]
async fn commit_push_with_a_new_epoch_in_view_reposts() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let away = town.spot("canal")?;
    alice.platform.script_fix(away.fix_outside(alice.now())?);
    alice.wake(WakeTrigger::Scheduled).await?;
    town.advance(Duration::from_mins(1));
    alice.parties.set_epoch(EpochState {
        current: Epoch::new(4),
        entered_at: town.now(),
        transition: EpochTransition::Other,
    });
    let commit = alice
        .wake(WakeTrigger::Push(PushReason::GroupCommit))
        .await?;
    assert!(matches!(
        commit.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::EpochChanged,
            ..
        }
    ));
    Ok(())
}

/// The re-post after a Commit failed: the next wake still replaces the old epoch's record,
/// though the epoch change itself is no longer new.
#[tokio::test]
async fn failed_epoch_repost_is_retried_on_the_next_wake() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    alice.parties.set_epoch(EpochState {
        current: Epoch::new(4),
        entered_at: town.now(),
        transition: EpochTransition::Other,
    });
    alice.server.disturb(CallKind::Upload, Effect::Fail);
    let failed = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        failed.upload,
        UploadOutcome::Interrupted(UploadTrigger::EpochChanged)
    );

    alice.server.calm();
    town.advance(Duration::from_secs(1));
    let retried = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(matches!(
        retried.upload,
        UploadOutcome::Uploaded {
            real_records: 1,
            ..
        }
    ));
    Ok(())
}

/// A lease is forgotten when it expires (bucket start + 4): a real record that has left the
/// board needs no withdrawing.
#[tokio::test]
async fn expired_lease_needs_no_withdrawal() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    alice.enter(town.spot("anchor")?, anchor).await?;
    let start = alice
        .board()
        .leases
        .last()
        .map(|lease| lease.start)
        .ok_or(TestError::Missing("lease"))?;
    let expiry = start
        .checked_add(pingers_presence::LEASE_BUCKETS)
        .ok_or(TestError::Missing("bucket"))?
        .start();
    town.advance(expiry.saturating_duration_since(town.now()));
    alice.settings.pause = pingers_gate::PauseState::Paused;
    let paused = alice.wake(WakeTrigger::Scheduled).await?;
    assert!(matches!(
        paused.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::Wake,
            ..
        }
    ));
    Ok(())
}

/// Everything the snapshot holds survives a restore, each board under its own server.
#[tokio::test]
async fn restore_keeps_every_stored_field() -> Result<(), TestError> {
    let town = Town::new()?;
    let anchor = town.spot_ref("anchor")?;
    let mailbox = pingers_proto::MailboxId::from_bytes([5; 16]);
    let record = pingers_proto::RecordRandomness::from_bytes([6; 16]);
    let lease = crate::LiveLease {
        start: town.now().bucket(),
        slots: [crate::LeaseSlot {
            randomness: record,
            real: None,
        }; pingers_proto::Lease::SLOTS],
    };
    let snapshot = crate::WakeSnapshot {
        spots: Vec::new(),
        last_in_zone: Some(town.now()),
        boards: vec![
            crate::BoardSnapshot {
                server: town.server.clone(),
                leases: Vec::new(),
                sent_knocks: vec![crate::SentKnock {
                    spot: anchor,
                    epoch: Epoch::new(3),
                    knock_mailbox: mailbox,
                    matched: record,
                    matched_start: town.now().bucket(),
                    reply_mailbox: mailbox,
                    sent_at: town.now(),
                    status: crate::KnockStatus::Waiting,
                }],
                decided_knocks: vec![crate::DecidedKnock {
                    reply_mailbox: mailbox,
                    at: town.now(),
                }],
                last_upload: Some(town.now()),
            },
            crate::BoardSnapshot {
                server: super::server_id("https://harbour.example")?,
                leases: vec![lease],
                sent_knocks: Vec::new(),
                decided_knocks: Vec::new(),
                last_upload: None,
            },
        ],
        epochs: vec![crate::KnownEpoch {
            party: town.party.id,
            epoch: Epoch::new(3),
        }],
        peer_activity: vec![crate::PeerActivity {
            spot: anchor,
            at: town.now(),
        }],
    };
    assert_eq!(WakeState::restore(snapshot.clone()).snapshot(), snapshot);
    Ok(())
}
