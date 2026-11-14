// SPDX-License-Identifier: AGPL-3.0-only
//! Push maintenance against the real server and push relay: a new token reaches every
//! registration one request at a time, "No wake-up pushes" clears them, and the relay's next
//! key comes through the server with its activation time.

use std::time::Duration;

use pingers_crypto::{PushRelayPublicKey, key_config_hash};
use pingers_ohttp::KeyPins;
use pingers_proto::{
    CreateGroup, Epoch, Padded, PushClass, PushPlatform, PushToken, PushTokenPlaintext, RelayRef,
    SizeClass, TimeToLive, Urgency,
};
use pingers_store::MailboxPurpose;
use pingers_sync::{
    GroupMembership, Maintained, Maintenance, NextPushRelayKey, PoolRefill, PoolTarget, PushSeal,
    Pushes, Sending, WakeBudget, WakePush,
};
use pingers_testkit::{HARNESS_PUSH_RELAY_ID, HarnessConfig};
use pingers_time::{Clock as _, UnixSeconds};

use crate::group::{GROUP, key_for};
use crate::support::{Device, TestError, TestResult, World, real_pause};

fn relay_key(world: &World) -> Result<PushRelayPublicKey, TestError> {
    let public: [u8; 32] = world
        .harness
        .endpoints()
        .push_relay_key
        .as_slice()
        .try_into()
        .map_err(|_| TestError::Unexpected("push relay key length"))?;
    Ok(PushRelayPublicKey::new(1, public))
}

/// The wake push for `token`, sealed to the harness push relay for a day.
fn wake_push(world: &World, token: &str) -> Result<WakePush, TestError> {
    let seal = |class, urgency| -> Result<PushSeal, TestError> {
        Ok(PushSeal {
            relay: RelayRef::Listed(HARNESS_PUSH_RELAY_ID),
            relay_key: relay_key(world)?,
            token: PushTokenPlaintext {
                platform: PushPlatform::UnifiedPush,
                class,
                token: PushToken::from_bytes(token.as_bytes().to_vec()),
                not_after: UnixSeconds::new(world.clock.now().get().saturating_add(86_400)),
            },
            urgency,
        })
    };
    Ok(WakePush {
        knock: seal(PushClass::Location, Urgency::Alert)?,
        alert: seal(PushClass::Standard, Urgency::Alert)?,
    })
}

fn maintenance(groups: &[GroupMembership], pushes: Pushes) -> Result<Maintenance<'_>, TestError> {
    Ok(Maintenance {
        pool: PoolTarget {
            purpose: MailboxPurpose::Knock,
            size: 1,
            lifetime: TimeToLive::new(Duration::from_hours(24))?,
        },
        groups,
        subscription_lifetime: TimeToLive::new(Duration::from_hours(24 * 30))?,
        pushes,
    })
}

async fn step(device: &Device, groups: &[GroupMembership]) -> Result<Maintained, TestError> {
    step_with(device, groups, Pushes::Current).await
}

async fn step_with(
    device: &Device,
    groups: &[GroupMembership],
    pushes: Pushes,
) -> Result<Maintained, TestError> {
    Ok(device
        .engine
        .maintain_push_registrations(&maintenance(groups, pushes)?, WakeBudget::UNLIMITED)
        .await?)
}

/// Polls the harness for a push to `token`: the server forwards it from a task of its own.
async fn pushed_to(world: &World, token: &str) -> bool {
    for _ in 0..200 {
        if world
            .harness
            .pushes()
            .iter()
            .any(|push| push.token.as_bytes() == token.as_bytes())
        {
            return true;
        }
        real_pause().await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_token_is_sealed_into_a_registered_mailbox_and_wakes_there() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    device
        .engine
        .set_wake_push(Some(wake_push(&world, "https://push.example/old")?));
    // An empty pool comes first.
    assert_eq!(
        step(&device, &[]).await?,
        Maintained::Pool(PoolRefill::Registered { pooled: 1 })
    );
    assert_eq!(step(&device, &[]).await?, Maintained::UpToDate);

    device
        .engine
        .set_wake_push(Some(wake_push(&world, "https://push.example/new")?));
    assert_eq!(step(&device, &[]).await?, Maintained::MailboxResealed);
    assert_eq!(step(&device, &[]).await?, Maintained::UpToDate);

    let mailbox = device
        .engine
        .take_mailbox(MailboxPurpose::Knock)
        .await?
        .ok_or(TestError::Unexpected("no pooled mailbox"))?;
    let knock = Padded::pad(b"a knock", SizeClass::Bytes1024)?;
    let _ = device
        .engine
        .post_to_mailbox(&mailbox, &knock, Sending::or_drop(WakeBudget::UNLIMITED))
        .await?;
    assert!(pushed_to(&world, "https://push.example/new").await);
    assert!(
        !world
            .harness
            .pushes()
            .iter()
            .any(|push| push.token.as_bytes() == b"https://push.example/old")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_are_added_resealed_cleared_and_forgotten() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let budget = WakeBudget::UNLIMITED;
    device
        .engine
        .create_group(
            &CreateGroup {
                group_id: GROUP,
                auth_key: key_for(Epoch::new(0)).public_key(),
                subscriptions: Vec::new(),
                state: None,
            },
            pingers_sync::GroupStart {
                lifetime: pingers_sync::GroupLifetime::Party,
                epoch: Epoch::new(0),
            },
            budget,
        )
        .await?;
    let groups = [GroupMembership {
        group: GROUP,
        key: key_for(Epoch::new(0)),
    }];
    device
        .engine
        .set_wake_push(Some(wake_push(&world, "https://push.example/one")?));
    assert!(matches!(step(&device, &groups).await?, Maintained::Pool(_)));
    assert_eq!(step(&device, &groups).await?, Maintained::SubscriptionAdded);
    let added = device
        .session()?
        .subscriptions()
        .get(&GROUP)?
        .ok_or(TestError::Unexpected("not recorded"))?;
    assert!(added.seal.is_some());
    assert_eq!(step(&device, &groups).await?, Maintained::UpToDate);

    device
        .engine
        .set_wake_push(Some(wake_push(&world, "https://push.example/two")?));
    assert_eq!(
        step(&device, &groups).await?,
        Maintained::SubscriptionResealed
    );
    let resealed = device
        .session()?
        .subscriptions()
        .get(&GROUP)?
        .ok_or(TestError::Unexpected("not recorded"))?;
    // The server holds the same subscription, sealed anew under a fresh handle.
    assert_eq!(resealed.id, added.id);
    assert_ne!(resealed.handle, added.handle);
    assert_ne!(resealed.seal, added.seal);
    assert_eq!(step(&device, &groups).await?, Maintained::MailboxResealed);
    assert_eq!(step(&device, &groups).await?, Maintained::UpToDate);

    // Not known this wake: no seal is touched.
    let unknown = step_with(&device, &groups, Pushes::Unknown).await?;
    assert_eq!(unknown, Maintained::UpToDate);
    // "No wake-up pushes": the subscription goes, then the mailbox's push.
    let off = |device| step_with(device, &groups, Pushes::Off);
    assert_eq!(off(&device).await?, Maintained::SubscriptionRemoved);
    assert_eq!(off(&device).await?, Maintained::MailboxPushRemoved);
    assert_eq!(off(&device).await?, Maintained::UpToDate);

    // Pushes back on: the group gets a subscription again.
    device
        .engine
        .set_wake_push(Some(wake_push(&world, "https://push.example/two")?));
    assert_eq!(step(&device, &groups).await?, Maintained::SubscriptionAdded);
    // A group this device left: its subscription is forgotten without a request.
    let seen = device.link.seen().len();
    assert_eq!(step(&device, &[]).await?, Maintained::MailboxResealed);
    assert!(device.session()?.subscriptions().get(&GROUP)?.is_none());
    assert_eq!(device.link.seen().len(), seen.saturating_add(1));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_sent_without_a_relay_and_the_registration_stays_stale() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    device
        .engine
        .set_wake_push(Some(wake_push(&world, "https://push.example/a")?));
    let _ = step(&device, &[]).await?;
    device
        .engine
        .set_wake_push(Some(wake_push(&world, "https://push.example/b")?));
    device.link.set_offline(true);
    assert_eq!(
        step(&device, &[]).await?,
        Maintained::NotSent(pingers_sync::NotSentReason::NoRelay)
    );
    device.link.set_offline(false);
    assert_eq!(step(&device, &[]).await?, Maintained::MailboxResealed);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_next_relay_key_comes_through_the_server_with_its_activation() -> TestResult {
    let activates_at = UnixSeconds::new(pingers_testkit::TEST_START.get().saturating_add(86_400));
    let world = World::start_with(&HarnessConfig {
        push_relay_next_key_at: Some(activates_at),
        ..HarnessConfig::default()
    })?;
    let device = world.device()?;
    let next_public: [u8; 32] = world
        .harness
        .endpoints()
        .push_relay_next_key
        .clone()
        .ok_or(TestError::Unexpected("no next key"))?
        .as_slice()
        .try_into()
        .map_err(|_| TestError::Unexpected("next key length"))?;
    let next_hash = key_config_hash(
        &pingers_proto::PushRelayKeyConfig {
            key_id: pingers_proto::PushRelayKeyId::new(2),
            public_key: next_public,
        }
        .to_bytes(),
    );
    let budget = WakeBudget::UNLIMITED;
    let NextPushRelayKey::Pinned(scheduled) = device
        .engine
        .push_relay_next_key(
            HARNESS_PUSH_RELAY_ID,
            Some(&KeyPins::from(next_hash)),
            budget,
        )
        .await?
    else {
        return Err(TestError::Unexpected("the next key was not pinned"));
    };
    assert_eq!(scheduled.activates_at, activates_at);
    assert_eq!(scheduled.verified.key.key_id(), 2);
    assert_eq!(scheduled.verified.key.public_key(), &next_public);
    assert_eq!(
        device
            .engine
            .push_relay_next_key(
                HARNESS_PUSH_RELAY_ID,
                Some(&KeyPins::from(pingers_proto::KeyConfigHash::from_bytes(
                    [1; 32]
                ))),
                budget
            )
            .await?,
        NextPushRelayKey::Unpinned { activates_at }
    );
    // Once it is published there is no next key.
    world.clock.advance(Duration::from_hours(24));
    assert_eq!(
        device
            .engine
            .push_relay_next_key(HARNESS_PUSH_RELAY_ID, None, budget)
            .await?,
        NextPushRelayKey::NotScheduled
    );
    let published = device
        .engine
        .push_relay_key(HARNESS_PUSH_RELAY_ID, None, budget)
        .await?;
    assert_eq!(published.key.key_id(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mailbox_the_server_forgot_is_forgotten_instead_of_tried_again() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    device
        .engine
        .set_wake_push(Some(wake_push(&world, "https://push.example/a")?));
    let _ = step(&device, &[]).await?;
    let _ = step(&device, &[]).await?;
    let mailbox = device
        .engine
        .take_mailbox(MailboxPurpose::Knock)
        .await?
        .ok_or(TestError::Unexpected("no pooled mailbox"))?;
    let stored = device
        .session()?
        .mailboxes()
        .get(&mailbox)?
        .ok_or(TestError::Unexpected("not stored"))?;
    // Deleted on the server behind the engine's back.
    let deleted = device
        .engine
        .send(
            pingers_sync::ApiRequest::delete_mailbox(&mailbox, &stored.read_key),
            Sending::or_drop(WakeBudget::UNLIMITED),
        )
        .await?
        .answered(pingers_sync::Route::DeleteMailbox)?;
    assert_eq!(deleted.status(), 204);
    device
        .engine
        .set_wake_push(Some(wake_push(&world, "https://push.example/b")?));
    // The empty pool comes first; then the forgotten mailbox, once.
    assert!(matches!(step(&device, &[]).await?, Maintained::Pool(_)));
    assert_eq!(step(&device, &[]).await?, Maintained::Forgotten);
    assert!(device.session()?.mailboxes().get(&mailbox)?.is_none());
    assert_eq!(step(&device, &[]).await?, Maintained::UpToDate);
    Ok(())
}
