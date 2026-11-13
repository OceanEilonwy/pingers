// SPDX-License-Identifier: AGPL-3.0-only
//! With no relay reachable, anonymous requests are queued or dropped, never sent direct.

use std::time::Duration;

use pingers_proto::{LeaseId, MailboxId, Padded, SecretBytes, SizeClass};
use pingers_sync::{Delivered, NotSentReason, ProcessRole, Route, Sending, SyncError, WakeBudget};
use pingers_time::{Clock as _, UnixSeconds};

use crate::mailbox::knock_mailbox;
use crate::support::{Device, TestError, TestResult, World};

async fn registered(device: &Device) -> Result<MailboxId, TestError> {
    let _ = device.stock_tokens().await?;
    match device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await?
    {
        Delivered::Done(mailbox) => Ok(mailbox),
        Delivered::Queued(_) | Delivered::Dropped(_) => {
            Err(TestError::Unexpected("registration was not sent"))
        }
    }
}

fn later(world: &World, seconds: u64) -> UnixSeconds {
    UnixSeconds::new(world.clock.now().get().saturating_add(seconds))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsent_requests_wait_for_a_relay_and_go_out_in_a_later_flush() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let mailbox = registered(&device).await?;
    let budget = WakeBudget::UNLIMITED;
    let tokens = device.engine.token_count().await?;
    device.link.set_offline(true);

    let knock = Padded::pad(b"queued knock", SizeClass::Bytes1024)?;
    let posted = device
        .engine
        .post_to_mailbox(
            &mailbox,
            &knock,
            Sending::or_keep_until(later(&world, 600), budget),
        )
        .await?;
    assert_eq!(posted, Delivered::Queued(NotSentReason::NoRelay));
    assert_eq!(
        device.engine.token_count().await?,
        tokens,
        "the token of a request that never left is kept"
    );
    // A lease revocation is not worth keeping: it is dropped.
    let revoked = device
        .engine
        .revoke_lease(
            &LeaseId::from_bytes([1; 16]),
            &SecretBytes::new([2; 32]),
            Sending::or_drop(budget),
        )
        .await?;
    assert_eq!(revoked, Delivered::Dropped(NotSentReason::NoRelay));
    // A read the caller needs now fails instead.
    assert!(matches!(
        device.engine.read_mailbox(&mailbox, None, budget).await,
        Err(SyncError::NoRelay {
            route: Route::ReadMailbox
        })
    ));
    assert_eq!(device.engine.queued(), 1);

    let still_offline = device.engine.flush_queue(budget).await?;
    assert!(still_offline.sent.is_empty());
    assert_eq!(still_offline.remaining, 1);

    device.link.set_offline(false);
    let flushed = device.engine.flush_queue(budget).await?;
    assert_eq!(flushed.remaining, 0);
    assert_eq!(flushed.expired, 0);
    let statuses: Vec<(Route, u16)> = flushed
        .sent
        .iter()
        .map(|response| (response.route(), response.status()))
        .collect();
    assert_eq!(statuses, [(Route::PostToMailbox, 202)]);
    assert_eq!(device.engine.token_count().await?, tokens.saturating_sub(1));
    let read = device.engine.read_mailbox(&mailbox, None, budget).await?;
    assert_eq!(read.messages.len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_requests_expire_and_keep_their_order() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let mailbox = registered(&device).await?;
    let budget = WakeBudget::UNLIMITED;
    device.link.set_offline(true);

    // Already past its time: dropped at once.
    let stale = device
        .engine
        .post_to_mailbox(
            &mailbox,
            &Padded::pad(b"stale", SizeClass::Bytes1024)?,
            Sending::or_keep_until(later(&world, 0).previous_second(), budget),
        )
        .await?;
    assert_eq!(stale, Delivered::Dropped(NotSentReason::NoRelay));
    for (body, keep) in [(b"short", 60), (b"first", 600), (b"secnd", 600)] {
        let queued = device
            .engine
            .post_to_mailbox(
                &mailbox,
                &Padded::pad(body, SizeClass::Bytes1024)?,
                Sending::or_keep_until(later(&world, keep), budget),
            )
            .await?;
        assert_eq!(queued, Delivered::Queued(NotSentReason::NoRelay));
    }
    world.clock.advance(Duration::from_secs(61));
    device.link.set_offline(false);
    let flushed = device.engine.flush_queue(budget).await?;
    assert_eq!(flushed.expired, 1);
    assert_eq!(flushed.sent.len(), 2);
    let read = device.engine.read_mailbox(&mailbox, None, budget).await?;
    let bodies: Vec<&[u8]> = read
        .messages
        .iter()
        .map(|message| message.body.payload())
        .collect::<Result<_, _>>()?;
    assert_eq!(bodies, [b"first".as_slice(), b"secnd".as_slice()]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_relay_sends_nothing_anywhere() -> TestResult {
    let world = World::start()?;
    // Nothing listens on the discard port of loopback.
    let device = world.device_with("127.0.0.1:9", ProcessRole::MainApp)?;
    let _ = device.stock_tokens().await?;
    let tokens = device.engine.token_count().await?;
    let issued = device.issuer.requests();
    let budget = WakeBudget::UNLIMITED;
    let knock = Padded::pad(b"knock", SizeClass::Bytes1024)?;
    let posted = device
        .engine
        .post_to_mailbox(
            &MailboxId::from_bytes([9; 16]),
            &knock,
            Sending::or_keep_until(later(&world, 600), budget),
        )
        .await?;
    assert_eq!(posted, Delivered::Queued(NotSentReason::NoRelay));
    let dropped = device
        .engine
        .register_mailbox(&knock_mailbox()?, budget)
        .await?;
    assert_eq!(dropped, Delivered::Dropped(NotSentReason::NoRelay));
    assert_eq!(device.engine.token_count().await?, tokens);
    // Nothing went to the direct (identified) route instead.
    assert_eq!(device.issuer.requests(), issued);
    Ok(())
}

trait PreviousSecond {
    fn previous_second(self) -> Self;
}

impl PreviousSecond for UnixSeconds {
    fn previous_second(self) -> Self {
        Self::new(self.get().saturating_sub(1))
    }
}
