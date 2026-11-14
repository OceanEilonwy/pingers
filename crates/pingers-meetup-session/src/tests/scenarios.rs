// SPDX-License-Identifier: AGPL-3.0-only
//! End-to-end scenarios between simulated members.

use core::time::Duration;

use pingers_meetup::{
    AllowListChoice, AskBack, CadenceMode, ExactLocation, GrantScope, LocationAnswer,
    MeetPointDraft, MeetupAction, MeetupEvent,
};
use pingers_mls::CipherSuitePolicy;
use pingers_proto::{ChatBody, Delivery, LeafIndex, MeetPoint};

use super::{GROUP, Leftovers, TestError, TestResult, World, fix, subscription_of};
use crate::{
    AddMember, ChannelState, Ended, MAX_RECONNECT_DELAY, MeetupScreen, NETWORK_TIMEOUT,
    SessionError, knock_key_package,
};

const ALICE: LeafIndex = LeafIndex::new(0);
const BOB: LeafIndex = LeafIndex::new(1);
const CHLOE: LeafIndex = LeafIndex::new(2);

fn chat(text: &str) -> Result<MeetupAction, TestError> {
    Ok(MeetupAction::SendChat {
        body: ChatBody::new(text.to_owned())?,
        parent: None,
    })
}

#[tokio::test]
async fn members_exchange_frames_then_leaving_deletes_everything() -> TestResult {
    let world = World::new(2)?;
    let [mut alice, mut bob] = world.meetup_of_two().await?;
    alice.handle.own_fix(fix(world.now())?).await?;

    // Bob asks; Alice approves "for this meetup" and adds him to her allow-list.
    bob.handle.act(MeetupAction::AskLocation(ALICE)).await?;
    let request = match alice
        .event("a location request", |event| {
            matches!(event, MeetupEvent::LocationRequested { .. })
        })
        .await?
    {
        MeetupEvent::LocationRequested { from, request } => {
            assert_eq!(from, BOB);
            request
        }
        MeetupEvent::BoardEquivocation | _ => {
            return Err(TestError::Missing("a location request"));
        }
    };
    alice
        .handle
        .act(MeetupAction::AnswerRequest {
            request,
            answer: LocationAnswer::Allow {
                scope: GrantScope::ThisMeetup,
                ask_back: AskBack::DoNotAskBack,
                always: AllowListChoice::AddToAllowList,
            },
        })
        .await?;
    alice
        .event("the allow-list addition", |event| {
            *event == MeetupEvent::AlwaysAllowAdded(BOB)
        })
        .await?;
    let alice_device = world.device(0)?;
    let bob_device = world.device(1)?;
    assert!(
        alice_device
            .database()
            .session()?
            .always_allow()
            .contains(&world.party, &bob_device.key)?
    );
    bob.event("access to Alice", |event| {
        *event
            == MeetupEvent::LocationAccessChanged {
                peer: ALICE,
                access: ExactLocation::Shared { expires: None },
            }
    })
    .await?;

    // Alice's next background frame carries her fix; Bob's view shows it and its age.
    let fix_taken = world.now();
    world.clock.advance(Duration::from_secs(60));
    bob.event(
        "Alice's position",
        |event| matches!(event, MeetupEvent::PeerLocation(location) if location.leaf == ALICE),
    )
    .await?;
    // The event follows the view that shows it, so the shell reads the position it announces.
    let view = bob.handle.view().borrow().clone();
    assert_eq!(
        view.position_age(ALICE, world.now()),
        Some(world.now().saturating_duration_since(fix_taken))
    );
    world.clock.advance(Duration::from_secs(40));
    assert_eq!(
        view.position_age(ALICE, world.now()),
        Some(Duration::from_secs(100))
    );
    let alice_view = alice.view_until("Bob holding the key", |view| {
        view.peer(BOB).is_some_and(|peer| peer.holds_own_key)
    });
    alice_view.await?;

    // Chat goes through the group log (it wakes absent members) and is stored.
    alice.handle.act(chat("At the bar")?).await?;
    bob.event(
        "Alice's chat",
        |event| matches!(event, MeetupEvent::Chat { sender, .. } if *sender == ALICE),
    )
    .await?;
    assert_eq!(bob_device.chat_rows(&GROUP)?, 1);
    assert_eq!(alice_device.chat_rows(&GROUP)?, 1);

    // Alice leaves: her state goes at once, and Bob commits her proposal.
    assert_eq!(alice.handle.leave().await?, Ended::Left);
    assert_eq!(alice_device.leftovers(&GROUP)?, Leftovers::NOTHING);
    bob.view_until("Alice gone", |view| view.peers.is_empty())
        .await?;
    assert_eq!(world.server.commits(&GROUP), 2);
    // She took her own subscription off the group first, so nothing wakes her for it.
    assert_eq!(
        world.server.deleted_subscriptions(),
        vec![subscription_of(0)]
    );
    assert!(world.server.deleted().is_empty());

    // Bob, alone, leaves last: the group goes on the server and on his phone.
    assert_eq!(bob.handle.leave().await?, Ended::Left);
    assert_eq!(bob_device.leftovers(&GROUP)?, Leftovers::NOTHING);
    assert_eq!(world.server.deleted(), vec![GROUP]);
    // Alone, he deleted the whole group: no separate subscription delete.
    assert_eq!(
        world.server.deleted_subscriptions(),
        vec![subscription_of(0)]
    );
    Ok(())
}

#[tokio::test]
async fn each_device_sets_its_own_cadence() -> TestResult {
    let world = World::new(2)?;
    let [alice, bob] = world.meetup_of_two().await?;
    // The first tick sent one location frame on the channel and one dummy grant to the log.
    world
        .server
        .until_uplinks("Alice", Delivery::Ephemeral, 1)
        .await?;
    let start = world.server.uplinks("Alice", Delivery::Ephemeral);
    assert_eq!(start, 1);
    assert_eq!(world.server.uplinks("Alice", Delivery::Stored), 1);

    // Opening the screen makes a frame due at once, then one every 2 s.
    world.clock.advance(Duration::from_secs(2));
    alice.handle.screen(MeetupScreen::Open).await?;
    world
        .server
        .until_uplinks("Alice", Delivery::Ephemeral, 2)
        .await?;
    for expected in 3..=5 {
        world.clock.advance(Duration::from_secs(2));
        world
            .server
            .until_uplinks("Alice", Delivery::Ephemeral, expected)
            .await?;
    }
    let view = alice
        .view_until("live cadence", |view| view.cadence == CadenceMode::Live)
        .await?;
    assert_eq!(view.cadence, CadenceMode::Live);
    // Bob's app did not go live because Alice did.
    assert_eq!(bob.handle.view().borrow().cadence, CadenceMode::Background);

    // Closing it waits a full 60 s after the last frame.
    alice.handle.screen(MeetupScreen::Closed).await?;
    alice
        .view_until("background cadence", |view| {
            view.cadence == CadenceMode::Background
        })
        .await?;
    world.clock.advance(Duration::from_secs(58));
    alice.handle.own_fix(fix(world.now())?).await?;
    assert_eq!(world.server.uplinks("Alice", Delivery::Ephemeral), 5);
    world.clock.advance(Duration::from_secs(2));
    world
        .server
        .until_uplinks("Alice", Delivery::Ephemeral, 6)
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_dropped_channel_reopens_and_catches_up_from_the_log() -> TestResult {
    let world = World::new(2)?;
    let [mut alice, bob] = world.meetup_of_two().await?;
    assert_eq!(world.server.opened("Alice"), 1);
    // Board hashes are ephemeral: on the channel while it is up.
    let before = world.server.uplinks("Alice", Delivery::Ephemeral);
    alice.handle.board_seen(Vec::new()).await?;
    world
        .server
        .until_uplinks("Alice", Delivery::Ephemeral, before.saturating_add(1))
        .await?;

    // The exchange drops and the relay refuses the first attempts.
    world.server.refuse_streams(true);
    world.server.drop_channels("Alice");
    alice
        .view_until("the channel down", |view| {
            view.channel == ChannelState::Reconnecting
        })
        .await?;
    // It is retried at once, then after a backoff.
    world.server.until_refused("Alice", 1).await?;
    // No retry before the backoff, and an ephemeral frame is dropped, not posted.
    alice.handle.board_seen(Vec::new()).await?;
    assert_eq!(world.server.refused("Alice"), 1);
    assert_eq!(world.server.ephemeral_posts(), 0);
    // Meanwhile Bob writes: it reaches only the group log.
    bob.handle.act(chat("Where are you?")?).await?;

    // The relay is back; the next attempt (after a 1 s backoff) succeeds and the log is read.
    world.server.refuse_streams(false);
    world.clock.advance(Duration::from_secs(1));
    world.server.until_opened("Alice", 2).await?;
    alice.connected().await?;
    alice
        .event(
            "Bob's chat from the log",
            |event| matches!(event, MeetupEvent::Chat { sender, .. } if *sender == BOB),
        )
        .await?;
    assert_eq!(world.device(0)?.chat_rows(&GROUP)?, 1);

    // And the new channel carries frames again.
    let before = world.server.uplinks("Alice", Delivery::Ephemeral);
    world.clock.advance(Duration::from_secs(60));
    world
        .server
        .until_uplinks("Alice", Delivery::Ephemeral, before.saturating_add(1))
        .await?;
    Ok(())
}

/// Waits until both members' first ticks are out: each sent one frame on its channel and one
/// dummy grant. Until then a member can still send something the clock has not been moved
/// for, and the other would count it as heard at whatever time the test has moved to.
async fn first_ticks_sent(world: &World) -> TestResult {
    for member in ["Alice", "Bob"] {
        for delivery in [Delivery::Ephemeral, Delivery::Stored] {
            world.server.until_uplinks(member, delivery, 1).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_silent_member_is_removed() -> TestResult {
    let world = World::new(2)?;
    let [alice, bob] = world.meetup_of_two().await?;
    first_ticks_sent(&world).await?;
    // Bob's phone dies: no frame, no leave.
    drop(bob);
    world.server.until_no_channel("Bob").await?;
    world.server.until_drained("Alice").await?;
    world.clock.advance(Duration::from_mins(11));
    let view = alice
        .view_until("Bob removed", |view| view.peers.is_empty())
        .await?;
    assert!(view.peer(BOB).is_none());
    assert_eq!(world.server.commits(&GROUP), 2);

    // Alone for 30 minutes: the Meetup ends, on the server too.
    world.clock.advance(Duration::from_mins(31));
    assert_eq!(alice.handle.finished().await?, Ended::AloneTooLong);
    assert_eq!(world.device(0)?.leftovers(&GROUP)?, Leftovers::NOTHING);
    assert_eq!(world.server.deleted(), vec![GROUP]);
    Ok(())
}

#[tokio::test]
async fn a_member_removed_while_away_ends_its_meetup() -> TestResult {
    let world = World::new(2)?;
    let [alice, bob] = world.meetup_of_two().await?;
    first_ticks_sent(&world).await?;
    // Bob loses the network: no channel, and nothing he commits or posts gets through.
    world.server.refuse_commits_from("Bob");
    world.server.refuse_posts_from("Bob");
    world.server.refuse_streams(true);
    world.server.drop_channels("Bob");
    // He notices at once and tries again straight away; Alice has taken in all he sent.
    world.server.until_refused("Bob", 1).await?;
    world.server.until_drained("Alice").await?;
    world.clock.advance(Duration::from_mins(11));
    alice
        .view_until("Bob removed", |view| view.peers.is_empty())
        .await?;
    // His second attempt came after the 1 s backoff; the next waits 2 s.
    world.server.until_refused("Bob", 2).await?;

    // Back online, Bob reads Alice's commit from the log and learns he was removed.
    world.server.refuse_streams(false);
    world.server.allow_posts_from("Bob");
    world.clock.advance(MAX_RECONNECT_DELAY);
    let ended = tokio::time::timeout(super::PATIENCE, bob.handle.finished())
        .await
        .map_err(|_| TestError::Timeout("Bob's end"))?;
    assert_eq!(ended?, Ended::Removed);
    assert_eq!(world.device(1)?.leftovers(&GROUP)?, Leftovers::NOTHING);
    assert!(world.server.deleted().is_empty());
    alice.handle.act(chat("Still here")?).await?;
    Ok(())
}

#[tokio::test]
async fn a_leaver_is_committed_once_the_network_is_back() -> TestResult {
    let world = World::new(2)?;
    let [alice, bob] = world.meetup_of_two().await?;
    world.server.refuse_commits_from("Bob");
    assert_eq!(alice.handle.leave().await?, Ended::Left);
    // Bob's commit of Alice's proposal fails; the next tick retries it.
    world.server.until_refused_commits(1).await?;
    world.server.allow_commits_from("Bob");
    world.clock.advance(Duration::from_secs(60));
    bob.view_until("Alice gone", |view| view.peers.is_empty())
        .await?;
    assert_eq!(world.server.commits(&GROUP), 2);
    Ok(())
}

#[tokio::test]
async fn refused_actions_and_adds_leave_the_meetup_running() -> TestResult {
    let world = World::new(2)?;
    let [alice, mut bob] = world.meetup_of_two().await?;
    assert!(matches!(
        alice.handle.act(MeetupAction::AskLocation(ALICE)).await,
        Err(SessionError::Meetup(_))
    ));
    assert!(matches!(
        alice
            .handle
            .add_member(AddMember {
                key_package: pingers_proto::KeyPackageBytes::from_bytes(vec![1, 2, 3]),
                subscription: None,
            })
            .await,
        Err(SessionError::Mls(_))
    ));
    alice.handle.act(chat("Still fine")?).await?;
    bob.event("the chat", |event| {
        matches!(event, MeetupEvent::Chat { .. })
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn meetups_left_by_a_killed_process_are_discarded() -> TestResult {
    let world = World::new(2)?;
    let [alice, bob] = world.meetup_of_two().await?;
    drop(alice);
    drop(bob);
    let alice_device = world.device(0)?;
    assert_eq!(crate::discard_stale(alice_device.database()).await?, 1);
    assert_eq!(alice_device.leftovers(&GROUP)?, Leftovers::NOTHING);
    assert_eq!(crate::discard_stale(alice_device.database()).await?, 0);
    Ok(())
}

#[tokio::test]
async fn a_late_joiner_gets_the_snapshot_but_no_history() -> TestResult {
    let world = World::new(3)?;
    let [alice, mut bob] = world.meetup_of_two().await?;
    alice
        .handle
        .act(MeetupAction::SetMeetPoint(MeetPointDraft::Fixed {
            position: fix(world.now())?.coordinate,
            label: None,
        }))
        .await?;
    alice.handle.act(chat("Before Chloé")?).await?;
    bob.event("the early chat", |event| {
        matches!(event, MeetupEvent::Chat { .. })
    })
    .await?;
    let alice_leaf = alice.handle.view().borrow().own_leaf;
    let bob_set_by = bob.handle.view().borrow().meet_point_set_by;
    assert_eq!(bob_set_by, Some(alice_leaf));

    // Chloé knocks; Alice adds her and hands back the Welcome.
    let knock = knock_key_package(
        world.device(2)?.database(),
        CipherSuitePolicy::Classical,
        Box::new(pingers_testkit::seeded_rng(3)),
    )
    .await?;
    let welcome = alice
        .handle
        .add_member(AddMember {
            key_package: knock.key_package,
            subscription: None,
        })
        .await?;
    bob.view_until("Chloé added", |view| view.peer(CHLOE).is_some())
        .await?;
    let mut chloe = world.join(2, welcome, knock.grant_key).await?;
    let alice_point = alice.handle.view().borrow().meet_point.clone();
    let chloe_view = chloe
        .view_until("the meet point", |view| view.meet_point.is_some())
        .await?;
    assert!(matches!(
        chloe_view.meet_point,
        Some(MeetPoint::Fixed { .. })
    ));
    assert_eq!(chloe_view.meet_point, alice_point);
    // The snapshot came from Alice, the lowest leaf.
    assert_eq!(chloe_view.meet_point_set_by, Some(alice_leaf));
    assert_eq!(chloe_view.peers.len(), 2);
    // The whole log was read before the snapshot applied: no earlier chat came with it.
    let events = chloe.drain();
    assert!(events.contains(&MeetupEvent::MeetPointChanged));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, MeetupEvent::Chat { .. }))
    );
    assert_eq!(world.device(2)?.chat_rows(&GROUP)?, 0);
    Ok(())
}

#[tokio::test]
async fn a_network_call_that_hangs_times_out() -> TestResult {
    let world = World::new(2)?;
    let [alice, _bob] = world.meetup_of_two().await?;
    world.server.hang_posts(true);
    let chat_sent = alice.handle.act(chat("Anyone?")?);
    let time_passes = async {
        world.server.until_hung(1).await?;
        world.clock.advance(NETWORK_TIMEOUT);
        Ok::<(), TestError>(())
    };
    let (outcome, passed) = tokio::join!(chat_sent, time_passes);
    passed?;
    assert!(matches!(outcome, Err(SessionError::Timeout)));
    // The Meetup carries on.
    world.server.hang_posts(false);
    alice.handle.act(chat("Now?")?).await?;
    Ok(())
}
