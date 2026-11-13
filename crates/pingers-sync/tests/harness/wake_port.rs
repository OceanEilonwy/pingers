// SPDX-License-Identifier: AGPL-3.0-only
//! What pingers-wake's `ServerPort` needs: the mailbox pool, lease replacement with kept
//! revoke keys, the knock subscription, and clock skew.

use std::time::Duration;

use pingers_crypto::{
    PushRelayPrivateKey, PushTokenToOpen, RevokeKey, open_push_token, push_bind_for_subscription,
    revoke_commitment,
};
use pingers_proto::{Lease, ReadBoard, Record, SecretBytes, TimeToLive, Urgency};
use pingers_store::MailboxPurpose;
use pingers_sync::{
    ClockOffset, ClockStatus, Delivered, NewLease, NewMailbox, PoolRefill, PoolTarget, Route,
    Sending, SyncError, WakeBudget, WakePush,
};
use pingers_testkit::seeded_rng;
use pingers_time::{Clock as _, UnixSeconds};
use zeroize::Zeroizing;

use crate::mailbox::{knock_mailbox, seal};
use crate::support::{TestError, TestResult, World};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pool_fills_one_per_call_and_hands_out_long_enough_mailboxes() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let budget = WakeBudget::UNLIMITED;
    let target = PoolTarget {
        purpose: MailboxPurpose::Knock,
        size: 2,
        lifetime: TimeToLive::new(Duration::from_hours(2))?,
    };
    assert_eq!(
        device.engine.refill_mailbox_pool(&target, budget).await?,
        PoolRefill::Registered { pooled: 1 }
    );
    assert_eq!(
        device.engine.refill_mailbox_pool(&target, budget).await?,
        PoolRefill::Registered { pooled: 2 }
    );
    assert_eq!(
        device.engine.refill_mailbox_pool(&target, budget).await?,
        PoolRefill::Full { pooled: 2 }
    );
    device.link.set_offline(true);
    let reply = PoolTarget {
        purpose: MailboxPurpose::Reply,
        ..target
    };
    assert_eq!(
        device.engine.refill_mailbox_pool(&reply, budget).await?,
        PoolRefill::NotSent(pingers_sync::NotSentReason::NoRelay)
    );
    device.link.set_offline(false);

    // A reply mailbox that dies before a lease would end is not handed out for it.
    let short = NewMailbox {
        purpose: MailboxPurpose::Reply,
        lifetime: TimeToLive::new(Duration::from_mins(30))?,
        ..knock_mailbox()?
    };
    let _ = device.engine.register_mailbox(&short, budget).await?;
    let start = world.clock.now().bucket();
    assert_eq!(
        device
            .engine
            .take_mailbox_for_lease(MailboxPurpose::Reply, start)?,
        None
    );
    // The same 30-minute mailbox serves anything that ends sooner.
    let soon = UnixSeconds::new(world.clock.now().get().saturating_add(20 * 60));
    assert!(
        device
            .engine
            .take_mailbox_until(MailboxPurpose::Reply, soon)?
            .is_some()
    );
    let first = device
        .engine
        .take_mailbox_for_lease(MailboxPurpose::Knock, start)?;
    let second = device
        .engine
        .take_mailbox_for_lease(MailboxPurpose::Knock, start)?;
    assert!(first.is_some() && second.is_some() && first != second);
    assert_eq!(
        device
            .engine
            .take_mailbox_for_lease(MailboxPurpose::Knock, start)?,
        None
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_knock_carries_an_alert_subscription_only_with_wake_pushes_on() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let lifetime = TimeToLive::new(Duration::from_hours(1))?;
    assert!(device.engine.knock_subscription(lifetime)?.is_none());

    let knock_relay = PushRelayPrivateKey::generate(&mut seeded_rng(3));
    let alert_relay = PushRelayPrivateKey::generate(&mut seeded_rng(4));
    let mut alert = seal(alert_relay.public_key(2)?);
    alert.urgency = Urgency::Alert;
    device.engine.set_wake_push(Some(WakePush {
        knock: seal(knock_relay.public_key(1)?),
        alert,
    }));
    let subscription = device
        .engine
        .knock_subscription(lifetime)?
        .ok_or(TestError::Unexpected("no subscription"))?;
    assert_eq!(subscription.push.urgency, Urgency::Alert);
    assert_eq!(subscription.push.key_id.get(), 2);
    let _ = open_push_token(
        &alert_relay,
        &PushTokenToOpen {
            key_id: 2,
            bind: push_bind_for_subscription(&subscription.id),
            blob: &subscription.push.blob,
        },
    )?;

    // Knock mailboxes are sealed with the knock push: the harness does not list its relay, so the
    // server refuses it, which shows the push was attached.
    let _ = device.stock_tokens().await?;
    let target = PoolTarget {
        purpose: MailboxPurpose::Knock,
        size: 1,
        lifetime,
    };
    assert!(matches!(
        device
            .engine
            .refill_mailbox_pool(&target, WakeBudget::UNLIMITED)
            .await,
        Err(SyncError::Status {
            route: Route::CreateMailbox,
            status: 422
        })
    ));
    device.engine.set_wake_push(None);
    assert_eq!(
        device
            .engine
            .refill_mailbox_pool(&target, WakeBudget::UNLIMITED)
            .await?,
        PoolRefill::Registered { pooled: 1 }
    );
    Ok(())
}

fn new_lease(world: &World, fill: u8) -> NewLease {
    let revoke_key = RevokeKey::from_bytes(Zeroizing::new([fill; 32]));
    NewLease {
        lease: Lease {
            start: world.clock.now().bucket(),
            records: [0_u8, 1, 2, 3].map(|slot| {
                Record::from_bytes(&[fill.wrapping_mul(16).wrapping_add(slot); Record::SIZE])
            }),
            revoke_commitment: revoke_commitment(&revoke_key),
        },
        revoke_key,
        read: ReadBoard::Skip,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_upload_replaces_the_previous_lease() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let budget = WakeBudget::UNLIMITED;
    // A small correction is applied, then replaced by what the board sync reports.
    let now = world.clock.now();
    let ahead = UnixSeconds::new(now.get().saturating_add(100));
    assert_eq!(
        device.engine.observe_server_time(ahead),
        ClockStatus::Corrected(ClockOffset::between(ahead, now))
    );
    assert_eq!(device.engine.corrected_now(), ahead);

    let Delivered::Done(first) = device
        .engine
        .upload_lease(new_lease(&world, 1), budget)
        .await?
    else {
        return Err(TestError::Unexpected("first upload not sent"));
    };
    assert_eq!(
        device.engine.clock_status(),
        ClockStatus::Corrected(ClockOffset::default())
    );
    let Delivered::Done(_) = device
        .engine
        .upload_lease(new_lease(&world, 2), budget)
        .await?
    else {
        return Err(TestError::Unexpected("second upload not sent"));
    };
    // The second upload revealed the first lease's key: it is gone.
    let revoked = device
        .engine
        .revoke_lease(
            &first.lease_id,
            &SecretBytes::new([1; 32]),
            Sending::or_drop(budget),
        )
        .await;
    assert!(matches!(
        revoked,
        Err(SyncError::Status {
            route: Route::BoardRevokeLease,
            status: 404
        })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_skewed_clock_posts_nothing_until_corrected() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let budget = WakeBudget::UNLIMITED;
    let now = world.clock.now();
    let far = UnixSeconds::new(now.get().saturating_add(600));
    assert!(matches!(
        device.engine.observe_server_time(far),
        ClockStatus::Skewed(_)
    ));
    assert_eq!(
        device.engine.corrected_now(),
        now,
        "a skew is never applied"
    );
    assert!(matches!(
        device
            .engine
            .register_mailbox(&knock_mailbox()?, budget)
            .await,
        Err(SyncError::ClockSkew { offset }) if offset.seconds() == 600
    ));
    assert!(device.link.seen().is_empty(), "nothing was sent");
    // Requests without a credential (reads) still go.
    let _ = device
        .engine
        .board_page(&pingers_proto::BoardCursor::from_bytes(vec![1]), budget)
        .await;
    assert_eq!(device.link.seen().len(), 1);

    let _ = device.engine.observe_server_time(now);
    assert!(matches!(
        device
            .engine
            .register_mailbox(&knock_mailbox()?, budget)
            .await?,
        Delivered::Done(_)
    ));
    Ok(())
}
