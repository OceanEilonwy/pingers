// SPDX-License-Identifier: AGPL-3.0-only
//! Location grants are stored in the group log (design B.6.1): a member whose live channel
//! was down when one was sent gets it from the log read that follows every reconnect.

use core::time::Duration;

use pingers_meetup::{
    AllowListChoice, AskBack, ExactLocation, GrantScope, LocationAnswer, MeetupAction, MeetupEvent,
};
use pingers_mls::CipherSuitePolicy;
use pingers_proto::{Delivery, LeafIndex};

use super::{Live, TestError, TestResult, World, fix};
use crate::{AddMember, ChannelState, knock_key_package};

const ALICE: LeafIndex = LeafIndex::new(0);
const BOB: LeafIndex = LeafIndex::new(1);
const CHLOE: LeafIndex = LeafIndex::new(2);

const ALLOW: LocationAnswer = LocationAnswer::Allow {
    scope: GrantScope::ThisMeetup,
    ask_back: AskBack::DoNotAskBack,
    always: AllowListChoice::JustThisTime,
};

/// Waits for the next location request on `live`.
async fn next_request(live: &mut Live) -> Result<pingers_proto::LocationRequestId, TestError> {
    match live
        .event("a location request", |event| {
            matches!(event, MeetupEvent::LocationRequested { .. })
        })
        .await?
    {
        MeetupEvent::LocationRequested { request, .. } => Ok(request),
        MeetupEvent::BoardEquivocation | _ => Err(TestError::Missing("a location request")),
    }
}

/// Drops the live channels of `members`; re-opening them is refused until
/// [`FakeServer::refuse_streams`] is lifted.
async fn go_offline(world: &World, members: &[(&Live, &'static str)]) -> TestResult {
    world.server.refuse_streams(true);
    for (live, name) in members {
        world.server.drop_channels(name);
        live.view_until("the channel down", |view| {
            view.channel == ChannelState::Reconnecting
        })
        .await?;
    }
    Ok(())
}

/// Lifts the refusal and waits until each member re-opened its channel (and so read the
/// log).
async fn back_online(world: &World, members: &[(&Live, &'static str)]) -> TestResult {
    world.server.refuse_streams(false);
    world.clock.advance(Duration::from_secs(1));
    for (live, name) in members {
        world.server.until_opened(name, 2).await?;
        live.connected().await?;
    }
    Ok(())
}

fn access(live: &Live, peer: LeafIndex) -> Option<ExactLocation> {
    live.handle
        .view()
        .borrow()
        .peer(peer)
        .map(|peer| peer.access)
}

#[tokio::test]
async fn a_grant_sent_while_the_requester_is_offline_arrives_from_the_log() -> TestResult {
    let world = World::new(2)?;
    let [mut alice, mut bob] = world.meetup_of_two().await?;
    alice.handle.own_fix(fix(world.now())?).await?;
    bob.handle.act(MeetupAction::AskLocation(ALICE)).await?;
    let request = next_request(&mut alice).await?;

    // Bob's channel goes down before Alice approves: her grant reaches only the group log.
    go_offline(&world, &[(&bob, "Bob")]).await?;
    let stored_before = world.server.uplinks("Alice", Delivery::Stored);
    alice
        .handle
        .act(MeetupAction::AnswerRequest {
            request,
            answer: ALLOW,
        })
        .await?;
    assert_eq!(
        world.server.uplinks("Alice", Delivery::Stored),
        stored_before.saturating_add(1)
    );
    assert_eq!(access(&bob, ALICE), Some(ExactLocation::Private));

    // Back online, Bob reads the log and gets the key, then Alice's next position.
    back_online(&world, &[(&bob, "Bob")]).await?;
    bob.event("access to Alice from the log", |event| {
        *event
            == MeetupEvent::LocationAccessChanged {
                peer: ALICE,
                access: ExactLocation::Shared { expires: None },
            }
    })
    .await?;
    world.clock.advance(Duration::from_secs(60));
    bob.view_until("Alice's position", |view| {
        view.peer(ALICE).is_some_and(|peer| peer.position.is_some())
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn revoking_while_members_are_offline_reaches_only_those_who_keep_access() -> TestResult {
    let world = World::new(3)?;
    let [mut alice, bob] = world.meetup_of_two().await?;
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
    let chloe = world.join(2, welcome, knock.grant_key).await?;
    chloe.connected().await?;
    alice.handle.own_fix(fix(world.now())?).await?;

    // Alice shares with both.
    for asker in [&bob, &chloe] {
        asker.handle.act(MeetupAction::AskLocation(ALICE)).await?;
        let request = next_request(&mut alice).await?;
        alice
            .handle
            .act(MeetupAction::AnswerRequest {
                request,
                answer: ALLOW,
            })
            .await?;
        asker
            .view_until("access to Alice", |view| {
                view.peer(ALICE)
                    .is_some_and(|peer| matches!(peer.access, ExactLocation::Shared { .. }))
            })
            .await?;
    }

    // Both drop off; Alice stops sharing with Bob, which rotates her key.
    let away = [(&bob, "Bob"), (&chloe, "Chloé")];
    go_offline(&world, &away).await?;
    alice.handle.act(MeetupAction::RevokeLocation(BOB)).await?;

    // Back online, both read the log; Alice's next frame is under the new key.
    back_online(&world, &away).await?;
    world.clock.advance(Duration::from_secs(60));
    chloe
        .view_until("Alice's position under the new key", |view| {
            view.peer(ALICE).is_some_and(|peer| peer.position.is_some())
        })
        .await?;
    let bob_view = bob
        .view_until("Bob rotated away", |view| {
            view.peer(ALICE)
                .is_some_and(|peer| peer.access == ExactLocation::Private)
        })
        .await?;
    assert!(
        bob_view
            .peer(ALICE)
            .is_some_and(|peer| peer.position.is_none())
    );
    Ok(())
}

#[tokio::test]
async fn a_push_has_an_offline_member_read_the_log_at_once() -> TestResult {
    let world = World::new(2)?;
    let [alice, bob] = world.meetup_of_two().await?;
    // Bob's channel is down and stays down: only the log has Alice's message.
    go_offline(&world, &[(&bob, "Bob")]).await?;
    alice
        .handle
        .act(MeetupAction::SendChat {
            body: pingers_proto::ChatBody::new("On my way".to_owned())?,
            parent: None,
        })
        .await?;
    let bob_device = world.device(1)?;
    let group = bob.handle.group();
    assert_eq!(bob_device.chat_rows(&group)?, 0);
    // The push for his subscription has him read it, without waiting for the channel.
    bob.handle.read_log().await?;
    assert_eq!(bob_device.chat_rows(&group)?, 1);
    Ok(())
}
