// SPDX-License-Identifier: AGPL-3.0-only
//! The wake budget: nothing starts without enough time left, and nothing outlives it.

use std::time::Duration;

use pingers_proto::{Padded, SizeClass};
use pingers_sync::{Delivered, NotSentReason, Route, Sending, SyncError, WakeBudget};
use pingers_time::{Clock as _, UnixSeconds};

use crate::mailbox::knock_mailbox;
use crate::support::{TestError, TestResult, World, advancing_once, bearer};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wake_budget_is_honoured() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let Delivered::Done(mailbox) = device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await?
    else {
        return Err(TestError::Unexpected("registration was not sent"));
    };
    let sent_before = device.link.seen().len();
    let tokens = device.engine.token_count().await?;

    // One second left: too little to start anything.
    let nearly_over = WakeBudget::from_now(&world.clock, Duration::from_secs(1));
    let until = UnixSeconds::new(world.clock.now().get().saturating_add(600));
    let knock = Padded::pad(b"knock", SizeClass::Bytes1024)?;
    let posted = device
        .engine
        .post_to_mailbox(&mailbox, &knock, Sending::or_keep_until(until, nearly_over))
        .await?;
    assert_eq!(posted, Delivered::Queued(NotSentReason::BudgetExhausted));
    assert!(matches!(
        device
            .engine
            .read_mailbox(&mailbox, None, nearly_over)
            .await,
        Err(SyncError::BudgetExhausted {
            route: Route::ReadMailbox
        })
    ));
    // An identified request cannot be queued: it fails.
    let empty = world.device()?;
    assert!(matches!(
        empty
            .engine
            .refill_tokens(&empty.issuer.info(), &bearer(), nearly_over)
            .await,
        Err(SyncError::BudgetExhausted {
            route: Route::PrivacyPassIssue
        })
    ));
    assert_eq!(empty.issuer.requests(), 0);
    let flushed = device.engine.flush_queue(nearly_over).await?;
    assert!(flushed.sent.is_empty());
    assert_eq!(flushed.remaining, 1, "the queue waits for a wake with time");
    assert_eq!(device.link.seen().len(), sent_before, "nothing was sent");
    assert_eq!(
        device.engine.token_count().await?,
        tokens,
        "no token was spent"
    );

    // A request in flight when the budget ends is abandoned at the deadline.
    device.link.set_stalled(true);
    let budget = WakeBudget::from_now(&world.clock, Duration::from_secs(10));
    let seen_before = device.link.seen().len();
    // Time moves only once the request is out, so it is abandoned in flight, not refused
    // before it starts because a busy machine was slow to get there.
    let read = advancing_once(
        &world.clock,
        Duration::from_secs(1),
        || device.link.seen().len() > seen_before,
        device.engine.read_mailbox(&mailbox, None, budget),
    )
    .await?;
    assert_eq!(
        device.link.seen().len(),
        seen_before + 1,
        "the read was sent"
    );
    assert!(matches!(
        read,
        Err(SyncError::BudgetExhausted {
            route: Route::ReadMailbox
        })
    ));
    assert_eq!(budget.remaining(&world.clock), Some(Duration::ZERO));

    // The next wake, with time, sends what was queued.
    device.link.set_stalled(false);
    let flushed = device.engine.flush_queue(WakeBudget::UNLIMITED).await?;
    assert_eq!(flushed.sent.len(), 1);
    Ok(())
}
