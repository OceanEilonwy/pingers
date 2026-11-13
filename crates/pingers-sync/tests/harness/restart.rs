// SPDX-License-Identifier: AGPL-3.0-only
//! What survives an app restart: a fresh engine and session over the same database file
//! still flushes the queue, replaces the last lease and passes over its own group entries.

use pingers_proto::{AppMessage, Delivery, Epoch, MlsMessageBytes, Padded, SecretBytes, SizeClass};
use pingers_sync::{Delivered, GroupCall, NotSentReason, Route, Sending, SyncError, WakeBudget};

use crate::group::{GROUP, Member, create, key_for};
use crate::queue::{later, registered};
use crate::support::{TestError, TestResult, World};
use crate::wake_port::new_lease;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_queued_before_a_restart_is_sent_after_it() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let mailbox = registered(&device).await?;
    let budget = WakeBudget::UNLIMITED;
    device.link.set_offline(true);
    let knock = Padded::pad(b"knock before restart", SizeClass::Bytes1024)?;
    let posted = device
        .engine
        .post_to_mailbox(
            &mailbox,
            &knock,
            Sending::or_keep_until(later(&world, 600), budget),
        )
        .await?;
    assert_eq!(posted, Delivered::Queued(NotSentReason::NoRelay));

    let device = world.restart(device)?;
    assert_eq!(device.engine.queued().await?, 1);
    device.link.set_offline(false);
    let flushed = device.engine.flush_queue(budget).await?;
    let statuses: Vec<(Route, u16)> = flushed
        .sent
        .iter()
        .map(|response| (response.route(), response.status()))
        .collect();
    assert_eq!(statuses, [(Route::PostToMailbox, 202)]);
    assert_eq!(flushed.remaining, 0);
    let read = device.engine.read_mailbox(&mailbox, None, budget).await?;
    assert_eq!(read.messages.len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_upload_after_a_restart_replaces_the_old_lease() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let budget = WakeBudget::UNLIMITED;
    let Delivered::Done(first) = device
        .engine
        .upload_lease(new_lease(&world, 1), budget)
        .await?
    else {
        return Err(TestError::Unexpected("first upload not sent"));
    };

    let device = world.restart(device)?;
    let Delivered::Done(_) = device
        .engine
        .upload_lease(new_lease(&world, 2), budget)
        .await?
    else {
        return Err(TestError::Unexpected("upload after the restart not sent"));
    };
    // The upload after the restart revealed the first lease's key: the lease is gone.
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
async fn an_own_message_pulled_after_a_restart_is_skipped() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    create(&device).await?;
    let key = key_for(Epoch::new(0));
    let posted = device
        .engine
        .post_group_message(
            GroupCall {
                group: &GROUP,
                key: &key,
            },
            &AppMessage {
                epoch: Epoch::new(0),
                body: MlsMessageBytes::from_bytes(vec![7]),
                delivery: Delivery::Stored,
                wake: None,
            },
            Sending::or_drop(WakeBudget::UNLIMITED),
        )
        .await?;
    assert!(matches!(posted, Delivered::Done(Some(_))));

    let device = world.restart(device)?;
    let mut member = Member::new(b'A');
    let pulled = device
        .engine
        .pull_group(&GROUP, &mut member, WakeBudget::UNLIMITED)
        .await
        .map_err(|_| TestError::Unexpected("pull failed"))?;
    assert_eq!((pulled.applied, pulled.skipped), (0, 1));
    // Pulled past, the position is forgotten rather than kept for ever.
    assert!(
        device
            .session()?
            .own_group_entries()
            .positions(&GROUP)?
            .is_empty()
    );
    Ok(())
}
