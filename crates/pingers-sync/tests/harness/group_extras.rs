// SPDX-License-Identifier: AGPL-3.0-only
//! The rest of a group's routes: subscriptions, the state snapshot, ephemeral messages,
//! deletion, and a server that refuses a commit outright.

use std::time::Duration;

use pingers_crypto::PushRelayPrivateKey;
use pingers_proto::{
    AppMessage, CreateGroup, Delivery, Epoch, MlsMessageBytes, SealedSnapshot, TimeToLive,
};
use pingers_sync::{Delivered, GroupCall, GroupError, Route, Sending, SyncError, WakeBudget};
use pingers_testkit::seeded_rng;

use crate::group::{GROUP, Member, create, key_for};
use crate::mailbox::seal;
use crate::support::{TestResult, World};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_state_and_deletion() -> TestResult {
    let world = World::start()?;
    let alice = world.device()?;
    let _ = alice.stock_tokens().await?;
    let budget = WakeBudget::UNLIMITED;
    alice
        .engine
        .create_group(
            &CreateGroup {
                group_id: GROUP,
                auth_key: key_for(Epoch::new(0)).public_key(),
                subscriptions: Vec::new(),
                state: Some(SealedSnapshot::from_bytes(vec![5; 10])),
            },
            budget,
        )
        .await?;
    let key = key_for(Epoch::new(0));
    let call = GroupCall {
        group: &GROUP,
        key: &key,
    };

    let state = alice.engine.group_state(call, budget).await?;
    assert_eq!(state.epoch, Epoch::new(0));
    assert_eq!(state.state.as_bytes(), [5; 10]);

    let relay_key = PushRelayPrivateKey::generate(&mut seeded_rng(3)).public_key(1)?;
    let subscription = alice
        .engine
        .new_subscription(&seal(relay_key), TimeToLive::new(Duration::from_hours(24))?)?;
    alice
        .engine
        .put_subscription(call, &subscription, budget)
        .await?;
    let stored = alice.session.subscriptions().get(&GROUP)?;
    assert_eq!(
        stored.as_ref().map(|stored| stored.id),
        Some(subscription.id)
    );
    assert_eq!(
        stored.map(|stored| stored.handle),
        Some(subscription.push.handle)
    );

    alice
        .engine
        .delete_subscription(call, &subscription.id, budget)
        .await?;
    assert!(alice.session.subscriptions().get(&GROUP)?.is_none());
    // Already gone on the server (404): still fine.
    alice
        .engine
        .delete_subscription(call, &subscription.id, budget)
        .await?;

    let ephemeral = alice
        .engine
        .post_group_message(
            call,
            &AppMessage {
                epoch: Epoch::new(0),
                body: MlsMessageBytes::from_bytes(vec![1]),
                delivery: Delivery::Ephemeral,
                wake: None,
            },
            Sending::or_drop(budget),
        )
        .await?;
    assert_eq!(
        ephemeral,
        Delivered::Done(None),
        "an ephemeral message has no position"
    );

    alice.engine.delete_group(call, budget).await?;
    alice.engine.delete_group(call, budget).await?;
    assert!(matches!(
        alice.engine.group_state(call, budget).await,
        Err(SyncError::Status {
            route: Route::GroupState,
            status: 404
        })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_commit_is_an_error_not_a_conflict() -> TestResult {
    let world = World::start()?;
    let alice = world.device()?;
    let _ = alice.stock_tokens().await?;
    create(&alice).await?;
    alice.link.set_refusing_commits(true);
    let mut member = Member::new(b'A');
    let outcome = alice
        .engine
        .commit(&GROUP, &mut member, WakeBudget::UNLIMITED)
        .await;
    assert!(matches!(
        outcome,
        Err(GroupError::Sync(SyncError::Status {
            route: Route::Commit,
            status: 503
        }))
    ));
    assert_eq!(member.epoch, Epoch::new(0));
    Ok(())
}
