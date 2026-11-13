// SPDX-License-Identifier: AGPL-3.0-only
//! Focus, pause and exclusion zones: only dummies, no matching, no answer (design B.10, B.11).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_gate::{FocusState, GateDecision, PauseState, SuppressReason};
use pingers_geo::{Circle, Fence, Meters, PresenceState};
use pingers_presence::{MIN_UPLOAD_SPACING, UploadTrigger};

use super::{TestError, Town};
use crate::{PushReason, UploadOutcome, WakeTrigger};

/// Focus comes on while Alice holds a real record and Bob's knock waits in her mailbox: one
/// immediate all-dummy lease, the knock left unanswered, Meetups left, no fix taken.
#[tokio::test]
async fn focus_suppresses_everything_but_dummies() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.enter(spot, anchor).await?;
    alice
        .meetups
        .join(anchor, pingers_proto::GroupId::from_bytes([3; 32]));

    alice.platform.set_focus(FocusState::On);
    let fixes_before = alice.platform.fix_requests().len();
    let woken = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;

    assert_eq!(
        woken.decision,
        GateDecision::Suppress(SuppressReason::Focus)
    );
    assert_eq!(
        woken.transitions.first().map(|change| change.transition.to),
        Some(PresenceState::Suppressed)
    );
    assert!(matches!(
        woken.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::SuppressedWithRealRecord,
            real_records: 0,
            ..
        }
    ));
    assert_eq!(alice.real_records(), 0);
    assert!(woken.knock_decisions.is_empty());
    assert!(woken.matched.is_empty());
    assert!(alice.meetups.admitted().is_empty());
    assert_eq!(alice.meetups.leave_calls(), 1);
    assert_eq!(alice.platform.fix_requests().len(), fixes_before);
    Ok(())
}

/// Suppressed with no real record: cover uploads continue on their cadence, all dummies, and
/// the board read is not matched.
#[tokio::test]
async fn suppressed_cover_upload_reads_but_does_not_match() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let mut bob = town.phone(1)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    bob.settings.pause = PauseState::Paused;
    bob.platform.script_fix(spot.fix_inside(bob.now()));
    let paused = bob.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        paused.decision,
        GateDecision::Suppress(SuppressReason::Paused)
    );
    assert!(matches!(
        paused.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::Wake,
            real_records: 0,
            read: pingers_proto::ReadBoard::Read,
        }
    ));
    assert!(paused.matched.is_empty());
    // Paused: a fix could not change the answer, so none was asked for.
    assert!(bob.platform.fix_requests().is_empty());
    // The next wake within the spacing makes no request at all (design B.10).
    bob.server.take_calls();
    let quiet = bob.wake(WakeTrigger::FocusChanged).await?;
    assert!(matches!(quiet.upload, UploadOutcome::NotDue(_)));
    assert!(bob.server.calls().is_empty());
    Ok(())
}

/// An exclusion zone over the Spot: the fix touching it suppresses (design B.11) and the real
/// record is replaced at once; well away from it, sharing is back.
#[tokio::test]
async fn exclusion_zone_suppresses_everything_but_dummies() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    assert_eq!(alice.real_records(), 1);

    let centre = spot.fence.enclosing_circle().center();
    alice.settings.zones = vec![Fence::Circle(Circle::new(centre, Meters::from_whole(200))?)];
    town.advance(pingers_geo::ExclusionTrail::TRAIL_BUFFER);
    alice.platform.script_fix(spot.fix_inside(alice.now()));
    let in_zone = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        in_zone.decision,
        GateDecision::Suppress(SuppressReason::ExclusionZone)
    );
    assert!(matches!(
        in_zone.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::SuppressedWithRealRecord,
            real_records: 0,
            ..
        }
    ));

    // Fifteen minutes later and two kilometres away, the zone and its trail are behind her.
    town.advance(MIN_UPLOAD_SPACING.saturating_sub(pingers_geo::ExclusionTrail::TRAIL_BUFFER));
    let far = spot.fix_at_distance(Meters::from_whole(2_000), alice.now())?;
    alice.platform.script_fix(far);
    let trail = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(trail.decision, GateDecision::Share);
    Ok(())
}

/// Leaving the zone: a fix just outside it still falls in the trail buffer.
#[tokio::test]
async fn trail_buffer_keeps_suppressing_after_leaving_a_zone() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let spot = town.spot("anchor")?;
    let centre = spot.fence.enclosing_circle().center();
    alice.settings.zones = vec![Fence::Circle(Circle::new(centre, Meters::from_whole(200))?)];
    alice.platform.script_fix(spot.fix_inside(alice.now()));
    alice.wake(WakeTrigger::Scheduled).await?;

    town.advance(core::time::Duration::from_mins(2));
    let away = spot.fix_at_distance(Meters::from_whole(2_000), alice.now())?;
    alice.platform.script_fix(away);
    let leaving = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        leaving.decision,
        GateDecision::Suppress(SuppressReason::ExclusionZone)
    );
    // The trail can only end with a fix, so the wake still asks for one.
    assert_eq!(alice.platform.fix_requests().len(), 2);
    Ok(())
}

/// Sharing resumes after Focus ends: the Spot restarts from Outside and must be entered again
/// by the normal rule before a real record goes up.
#[tokio::test]
async fn after_focus_ends_the_spot_is_entered_again() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let anchor = town.spot_ref("anchor")?;
    let spot = town.spot("anchor")?;
    alice.enter(spot, anchor).await?;
    alice.platform.set_focus(FocusState::On);
    alice.wake(WakeTrigger::FocusChanged).await?;

    alice.platform.set_focus(FocusState::Off);
    town.advance(core::time::Duration::from_secs(1));
    alice.platform.script_fix(spot.fix_inside(alice.now()));
    let resumed = alice.wake(WakeTrigger::FocusChanged).await?;
    let states: Vec<PresenceState> = resumed
        .transitions
        .iter()
        .filter(|change| change.spot == anchor)
        .map(|change| change.transition.to)
        .collect();
    assert_eq!(
        states,
        vec![PresenceState::Outside, PresenceState::Candidate]
    );
    assert_eq!(alice.real_records(), 0);
    Ok(())
}
