// SPDX-License-Identifier: AGPL-3.0-only
//! Parties on different servers: one board round per server, each with its own lease, knocks
//! and mailboxes, its share of the budget, and its own ending.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;
use std::sync::{Arc, Mutex};

use pingers_geo::{Coordinate, Meters};
use pingers_presence::{KnockDecision, MIN_UPLOAD_SPACING, SpotRef, UploadTrigger};
use pingers_proto::{MailboxId, PartyId, ServerId, SizeClass};
use pingers_testkit::{PartyBuilder, PartyFixture, random_array, seeded_rng};
use pingers_time::UnixSeconds;

use super::fakes::{Call, CallKind, Effect, RosterMember, World};
use super::{BUDGET, Phone, TestError, Town, member_key, server_id};
use crate::ports::{PartyInfo, SpotInfo};
use crate::{
    BoardReport, BoardSnapshot, KnockStatus, PortError, PushReason, RETRY_AFTER, SentKnock,
    UploadOutcome, WakeEnding, WakeSnapshot, WakeState, WakeStep, WakeTrigger,
};

/// The second server. Its origin sorts before the town's, so on a tie it goes first.
const HARBOUR_SERVER: &str = "https://harbour.example";

/// The town, and a second Party on a second server whose one Spot, the quay, lies exactly on
/// the town's anchor: a phone entering the anchor is Inside a Spot on each server.
struct TwoServers {
    town: Town,
    harbour: PartyFixture,
    seed: [u8; 32],
    server: ServerId,
    world: Arc<Mutex<World>>,
}

impl TwoServers {
    fn new() -> Result<Self, TestError> {
        let town = Town::new()?;
        let mut rng = seeded_rng(2);
        let harbour = PartyBuilder::new()
            .members(2)
            .circle_spot(
                "quay",
                Coordinate::new(51.5007, -0.1246)?,
                Meters::from_whole(150),
            )?
            .build(&mut rng)?;
        Ok(Self {
            town,
            harbour,
            seed: random_array(&mut rng),
            server: server_id(HARBOUR_SERVER)?,
            world: World::shared(),
        })
    }

    /// Member `index`'s phone, in both Parties and able to reach both servers.
    fn phone(&self, index: u8) -> Result<Phone, TestError> {
        let mut phone = self.town.phone(index)?;
        let member = self
            .harbour
            .members
            .get(usize::from(index))
            .ok_or(TestError::Missing("member"))?;
        let roster = (0_u8..)
            .zip(&self.harbour.members)
            .map(|(position, member)| RosterMember {
                leaf: member.leaf,
                key: member_key(position).public_key(),
            })
            .collect();
        let info = PartyInfo {
            party: self.harbour.id,
            server: self.server.clone(),
            epoch: self.town.settled_epoch(),
            own_leaf: member.leaf,
            knock_class: SizeClass::Bytes1024,
            spots: self
                .harbour
                .spots
                .iter()
                .map(|spot| SpotInfo {
                    spot: spot.id,
                    fence: spot.fence.clone(),
                })
                .collect(),
        };
        phone.parties.add(info, self.seed, roster);
        phone
            .server
            .add_server(self.server.clone(), Arc::clone(&self.world));
        Ok(phone)
    }

    fn quay(&self) -> Result<SpotRef, TestError> {
        Ok(SpotRef {
            party: self.harbour.id,
            spot: self.harbour.spot("quay")?.id,
        })
    }

    /// The phone enters the anchor (and so the quay) and both are confirmed.
    async fn enter(&self, phone: &mut Phone) -> Result<(), TestError> {
        let anchor = self.town.spot_ref("anchor")?;
        phone.enter(self.town.spot("anchor")?, anchor).await?;
        Ok(())
    }
}

/// The Spots with a real record in a board's newest lease.
fn real_spots(board: &BoardSnapshot) -> Vec<SpotRef> {
    board.leases.last().map_or_else(Vec::new, |lease| {
        lease
            .slots
            .iter()
            .filter_map(|slot| slot.real.map(|real| real.spot))
            .collect()
    })
}

/// The knock mailbox of the real record for `spot` in a board's newest lease.
fn knock_mailbox(board: &BoardSnapshot, spot: SpotRef) -> Result<MailboxId, TestError> {
    board
        .leases
        .last()
        .and_then(|lease| {
            lease
                .slots
                .iter()
                .find_map(|slot| slot.real.filter(|real| real.spot == spot))
        })
        .map(|real| real.knock_mailbox)
        .ok_or(TestError::Missing("real record"))
}

fn ago(now: UnixSeconds, minutes: u64) -> Result<UnixSeconds, TestError> {
    now.checked_sub(Duration::from_mins(minutes))
        .ok_or(TestError::Missing("time"))
}

/// A board that uploaded at `last_upload` and holds nothing.
fn uploaded_board(server: &ServerId, last_upload: UnixSeconds) -> BoardSnapshot {
    BoardSnapshot {
        server: server.clone(),
        leases: Vec::new(),
        sent_knocks: Vec::new(),
        decided_knocks: Vec::new(),
        last_upload: Some(last_upload),
    }
}

/// Each server's board gets a lease of its own, whose only real record is its own Party's
/// Spot: no board learns of a Spot on another server.
#[tokio::test]
async fn each_board_gets_a_lease_with_only_its_own_spots() -> Result<(), TestError> {
    let world = TwoServers::new()?;
    let mut alice = world.phone(0)?;
    world.enter(&mut alice).await?;
    assert_eq!(
        real_spots(&alice.board()),
        vec![world.town.spot_ref("anchor")?]
    );
    assert_eq!(
        real_spots(&alice.board_on(&world.server)),
        vec![world.quay()?]
    );
    // Two wakes (candidate, confirmed), one lease each on each board.
    assert_eq!(alice.server.uploaded_to(&world.town.server).len(), 2);
    assert_eq!(alice.server.uploaded_to(&world.server).len(), 2);
    Ok(())
}

/// Bob matches Alice's record on each board and knocks on the server that holds it; Alice
/// reads each knock mailbox on its own server.
#[tokio::test]
async fn knocks_and_mailbox_reads_go_to_the_records_server() -> Result<(), TestError> {
    let world = TwoServers::new()?;
    let mut alice = world.phone(0)?;
    let mut bob = world.phone(1)?;
    world.enter(&mut alice).await?;
    let quay_mailbox = knock_mailbox(&alice.board_on(&world.server), world.quay()?)?;
    let anchor_mailbox = knock_mailbox(&alice.board(), world.town.spot_ref("anchor")?)?;

    world.enter(&mut bob).await?;
    let to_harbour = bob.server.calls_to(&world.server);
    assert!(to_harbour.contains(&Call::PostKnock(quay_mailbox)));
    assert!(!to_harbour.contains(&Call::PostKnock(anchor_mailbox)));
    assert!(
        bob.server
            .calls_to(&world.town.server)
            .contains(&Call::PostKnock(anchor_mailbox))
    );

    alice.server.take_calls();
    alice
        .platform
        .script_fix(world.town.spot("anchor")?.fix_inside(alice.now()));
    alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    let read_on_harbour = alice.server.calls_to(&world.server);
    assert!(read_on_harbour.contains(&Call::ReadMailbox(quay_mailbox)));
    assert!(!read_on_harbour.contains(&Call::ReadMailbox(anchor_mailbox)));
    Ok(())
}

/// Knocks on two boards in one wake share the one liveness fix: the second round does not
/// ask for another (and, finding none, refuse its knock).
#[tokio::test]
async fn rounds_share_one_liveness_fix() -> Result<(), TestError> {
    let world = TwoServers::new()?;
    let mut alice = world.phone(0)?;
    let mut bob = world.phone(1)?;
    world.enter(&mut alice).await?;
    world.enter(&mut bob).await?;
    // Alice's last fix is half a minute old: the only fix this wake takes is the liveness fix.
    alice
        .platform
        .script_fix(world.town.spot("anchor")?.fix_inside(alice.now()));
    let answered = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    let decisions: Vec<KnockDecision> = answered
        .knock_decisions
        .iter()
        .map(|outcome| outcome.decision)
        .collect();
    assert_eq!(decisions.len(), 2);
    assert!(
        decisions
            .iter()
            .all(|decision| matches!(decision, KnockDecision::Accept(_)))
    );
    Ok(())
}

/// The harbour server refuses the upload; the town server still gets its lease in the same
/// wake, and the report says which round ended how.
#[tokio::test]
async fn failing_server_does_not_stop_the_others_upload() -> Result<(), TestError> {
    let world = TwoServers::new()?;
    let mut alice = world.phone(0)?;
    alice
        .server
        .disturb_on(&world.server, CallKind::Upload, Effect::Fail);
    alice
        .platform
        .script_fix(world.town.spot("canal")?.fix_outside(alice.now())?);
    let woken = alice.wake(WakeTrigger::Scheduled).await?;
    let failed = WakeEnding::PortFailed {
        step: WakeStep::Upload,
        error: PortError::Unreachable,
    };
    assert_eq!(
        woken.boards.first(),
        Some(&BoardReport {
            server: world.server.clone(),
            upload: UploadOutcome::Interrupted(UploadTrigger::Wake),
            ending: failed,
        })
    );
    let town = woken.boards.get(1).ok_or(TestError::Missing("round"))?;
    assert_eq!(town.server, world.town.server);
    assert!(matches!(town.upload, UploadOutcome::Uploaded { .. }));
    assert_eq!(town.ending, WakeEnding::Completed);
    assert_eq!(woken.ending, failed);
    assert_eq!(woken.next_wake, RETRY_AFTER);
    Ok(())
}

/// The harbour server never answers: its round gives up when its half of the budget is spent,
/// and the town round still has the other half for a slow upload.
#[tokio::test]
async fn hanging_server_takes_only_its_share_of_the_budget() -> Result<(), TestError> {
    let world = TwoServers::new()?;
    let mut alice = world.phone(0)?;
    alice
        .server
        .disturb_on(&world.server, CallKind::PullLogs, Effect::Creep);
    let half = BUDGET.checked_div(2).ok_or(TestError::Missing("half"))?;
    alice.server.disturb_on(
        &world.town.server,
        CallKind::Upload,
        Effect::Slow(half.saturating_sub(Duration::from_secs(5))),
    );
    alice
        .platform
        .script_fix(world.town.spot("canal")?.fix_outside(alice.now())?);
    let start = alice.now();
    let woken = alice.wake(WakeTrigger::Scheduled).await?;

    let harbour = woken.boards.first().ok_or(TestError::Missing("round"))?;
    assert_eq!(harbour.ending, WakeEnding::OutOfBudget(WakeStep::GroupLogs));
    let town = woken.boards.get(1).ok_or(TestError::Missing("round"))?;
    assert_eq!(town.ending, WakeEnding::Completed);
    assert!(matches!(town.upload, UploadOutcome::Uploaded { .. }));
    let town_starts = alice
        .server
        .server_calls()
        .into_iter()
        .find(|call| call.server == world.town.server)
        .map(|call| call.at);
    assert_eq!(town_starts, start.checked_add(half));
    Ok(())
}

/// Restores Alice with the town board last uploaded `town_minutes` ago and the harbour board
/// `harbour_minutes` ago.
fn restore_uploads(
    world: &TwoServers,
    alice: &mut Phone,
    town_minutes: u64,
    harbour_minutes: u64,
) -> Result<(), TestError> {
    let now = alice.now();
    alice.state = WakeState::restore(WakeSnapshot {
        boards: vec![
            uploaded_board(&world.town.server, ago(now, town_minutes)?),
            uploaded_board(&world.server, ago(now, harbour_minutes)?),
        ],
        ..WakeSnapshot::default()
    });
    Ok(())
}

/// The board whose last upload is oldest goes first, ahead of the origin order, so a server
/// that keeps eating the budget at the front cannot starve the others wake after wake.
#[tokio::test]
async fn oldest_upload_goes_first() -> Result<(), TestError> {
    let world = TwoServers::new()?;
    let mut alice = world.phone(0)?;
    restore_uploads(&world, &mut alice, 10, 2)?;
    alice
        .platform
        .script_fix(world.town.spot("canal")?.fix_outside(alice.now())?);
    let woken = alice.wake(WakeTrigger::Scheduled).await?;
    let order: Vec<ServerId> = woken
        .boards
        .iter()
        .map(|board| board.server.clone())
        .collect();
    assert_eq!(order, vec![world.town.server.clone(), world.server.clone()]);
    Ok(())
}

/// The next wake comes when the board that uploaded longest ago is due for its cover upload,
/// not the one that uploaded last.
#[tokio::test]
async fn next_wake_follows_the_oldest_board() -> Result<(), TestError> {
    let world = TwoServers::new()?;
    let mut alice = world.phone(0)?;
    restore_uploads(&world, &mut alice, 15, 5)?;
    alice
        .platform
        .script_fix(world.town.spot("canal")?.fix_outside(alice.now())?);
    let woken = alice.wake(WakeTrigger::Scheduled).await?;
    assert_eq!(
        woken.next_wake,
        pingers_presence::TARGET_UPLOAD_SPACING.saturating_sub(Duration::from_mins(15))
    );
    Ok(())
}

/// After leaving the harbour Party, the harbour board gets one more lease, all dummies, to
/// withdraw the quay's record, and then no cover leases while that lease lives on.
#[tokio::test]
async fn board_without_a_party_only_withdraws_its_record() -> Result<(), TestError> {
    let world = TwoServers::new()?;
    let mut alice = world.phone(0)?;
    world.enter(&mut alice).await?;
    alice.parties.remove(world.harbour.id);
    world.town.advance(Duration::from_secs(1));
    let withdrawn = alice.wake(WakeTrigger::Scheduled).await?;
    let harbour = withdrawn
        .boards
        .iter()
        .find(|board| board.server == world.server)
        .ok_or(TestError::Missing("round"))?;
    assert!(matches!(
        harbour.upload,
        UploadOutcome::Uploaded {
            trigger: UploadTrigger::ExitWithRealRecord,
            real_records: 0,
            ..
        }
    ));

    world.town.advance(MIN_UPLOAD_SPACING);
    alice.server.take_calls();
    alice
        .platform
        .script_fix(world.town.spot("anchor")?.fix_inside(alice.now()));
    alice.wake(WakeTrigger::Scheduled).await?;
    assert!(
        !alice
            .server
            .calls_to(&world.server)
            .iter()
            .any(|call| matches!(call, Call::Upload { .. }))
    );
    Ok(())
}

/// A board with no Party, no lease the server may still hold and no knock waiting for a
/// Welcome is forgotten, and its server is not contacted.
#[tokio::test]
async fn board_with_nothing_left_is_dropped() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let gone = server_id("https://gone.example")?;
    alice.state = WakeState::restore(WakeSnapshot {
        boards: vec![uploaded_board(&gone, ago(alice.now(), 30)?)],
        ..WakeSnapshot::default()
    });
    alice
        .platform
        .script_fix(town.spot("canal")?.fix_outside(alice.now())?);
    let woken = alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert!(woken.boards.iter().all(|board| board.server != gone));
    let kept: Vec<ServerId> = alice
        .state
        .snapshot()
        .boards
        .into_iter()
        .map(|board| board.server)
        .collect();
    assert_eq!(kept, vec![town.server.clone()]);
    Ok(())
}

/// A board left with a knock still waiting for its Welcome gets a round of its own, though
/// no Party is on it any more, so the reply mailbox is still read on that server.
#[tokio::test]
async fn board_left_with_a_waiting_knock_still_reads_the_reply() -> Result<(), TestError> {
    let town = Town::new()?;
    let mut alice = town.phone(0)?;
    let waiting = server_id("https://waiting.example")?;
    alice.server.add_server(waiting.clone(), World::shared());
    let reply = MailboxId::from_bytes([7; 16]);
    let left_party = SpotRef {
        party: PartyId::from_bytes([9; 16]),
        spot: town.spot_ref("anchor")?.spot,
    };
    alice.state = WakeState::restore(WakeSnapshot {
        boards: vec![BoardSnapshot {
            server: waiting.clone(),
            leases: Vec::new(),
            sent_knocks: vec![SentKnock {
                spot: left_party,
                epoch: super::EPOCH,
                knock_mailbox: MailboxId::from_bytes([8; 16]),
                matched: pingers_proto::RecordRandomness::from_bytes([6; 16]),
                matched_start: alice.now().bucket(),
                reply_mailbox: reply,
                sent_at: alice.now(),
                status: KnockStatus::Waiting,
            }],
            decided_knocks: Vec::new(),
            last_upload: None,
        }],
        ..WakeSnapshot::default()
    });
    alice
        .platform
        .script_fix(town.spot("canal")?.fix_outside(alice.now())?);
    alice.wake(WakeTrigger::Push(PushReason::Mailbox)).await?;
    assert!(
        alice
            .server
            .calls_to(&waiting)
            .contains(&Call::ReadMailbox(reply))
    );
    assert!(
        alice
            .state
            .snapshot()
            .boards
            .iter()
            .any(|board| board.server == waiting)
    );
    Ok(())
}
