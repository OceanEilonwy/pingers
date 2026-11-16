// SPDX-License-Identifier: AGPL-3.0-only
//! The Privacy Pass stock: refilled below its minimum, spent one per anonymous write, and never
//! refilled past the server's daily allowance.

use std::time::Duration;

use pingers_anon::TokenCount;
use pingers_proto::TokenAllowance;
use pingers_store::MailboxPurpose;
use pingers_sync::{
    CredentialChoice, Delivered, ProcessRole, Refill, Route, SyncError, TokenStock, WakeBudget,
};
use pingers_time::{Clock as _, UnixSeconds};

use crate::mailbox::knock_mailbox;
use crate::support::{TestError, TestResult, World, bearer};

/// When the UTC day after the clock's starts.
fn next_midnight(world: &World) -> Result<UnixSeconds, TestError> {
    world
        .clock
        .now()
        .get()
        .checked_div(86_400)
        .and_then(|day| day.checked_add(1))
        .and_then(|day| day.checked_mul(86_400))
        .map(UnixSeconds::new)
        .ok_or(TestError::Unexpected("time"))
}

/// A stock of a day's whole allowance: refill below 300, up to 300.
fn whole_day() -> Result<TokenStock, TestError> {
    Ok(TokenStock::new(300, TokenCount::new(300)?)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refill_never_exceeds_the_daily_limit() -> TestResult {
    let world = World::start()?;
    let device = world.device_with_stock(whole_day()?)?;
    let params = device.engine.params(WakeBudget::UNLIMITED).await?;
    assert_eq!(params.privacy_pass_daily_limit, TokenAllowance::new(300));
    assert_eq!(
        device.stock_tokens().await?,
        Refill::Fetched {
            added: 300,
            held: 300
        }
    );
    // One spent: the stock is below its minimum, but today's allowance is spent, so nothing
    // is asked for (no 429 to provoke).
    let registered = device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await?;
    assert!(matches!(registered, Delivered::Done(_)));
    let resets_at = next_midnight(&world)?;
    for _ in 0..2 {
        assert_eq!(
            device.engine.stock_tokens(WakeBudget::UNLIMITED).await?,
            Refill::DailyLimitReached {
                held: 299,
                resets_at
            }
        );
    }
    assert_eq!(device.issuer.requests(), 1);
    // The next UTC day: exactly the shortfall.
    world.clock.advance(Duration::from_secs(
        resets_at.get().saturating_sub(world.clock.now().get()),
    ));
    assert_eq!(
        device.stock_tokens().await?,
        Refill::Fetched {
            added: 1,
            held: 300
        }
    );
    assert_eq!(device.issuer.requests(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_for_the_allowance_says_what_is_left_and_only_that_is_asked_for() -> TestResult {
    let world = World::start()?;
    // Another install of the same account took 290 of today's tokens: this device does not
    // know, so its first batch asks for 300 and is refused whole.
    let other = world.device_with_stock(TokenStock::new(290, TokenCount::new(290)?)?)?;
    assert!(matches!(
        other.stock_tokens().await?,
        Refill::Fetched { added: 290, .. }
    ));
    let device = world.device_with_stock(whole_day()?)?;
    assert_eq!(
        device.stock_tokens().await?,
        Refill::Fetched {
            added: 10,
            held: 10
        },
        "the server's 429 said 10 were left, and 10 were asked for"
    );
    assert_eq!(device.issuer.requests(), 2);
    // The allowance is now known to be spent: nothing more today.
    assert!(matches!(
        device.stock_tokens().await?,
        Refill::DailyLimitReached { held: 10, .. }
    ));
    assert_eq!(device.issuer.requests(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_token_stock_is_refilled_below_its_minimum() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    assert_eq!(device.engine.token_count().await?, 0);

    assert_eq!(
        device.stock_tokens().await?,
        Refill::Fetched { added: 5, held: 5 }
    );
    assert_eq!(device.issuer.requests(), 1);
    assert_eq!(device.stock_tokens().await?, Refill::Stocked { held: 5 });
    assert_eq!(device.issuer.requests(), 1, "a full stock fetches nothing");

    for _ in 0..2 {
        let registered = device
            .engine
            .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
            .await?;
        assert!(matches!(registered, Delivered::Done(_)));
    }
    assert_eq!(
        device.stock_tokens().await?,
        Refill::Stocked { held: 3 },
        "3 is the minimum"
    );
    let _ = device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await?;
    assert_eq!(
        device.stock_tokens().await?,
        Refill::Fetched { added: 3, held: 5 }
    );
    assert_eq!(device.issuer.requests(), 2);
    assert_eq!(
        device
            .engine
            .pooled_mailboxes(MailboxPurpose::Knock)
            .await?,
        3
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_issuer_refusal_is_reported_and_adds_nothing() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    device.issuer.set_unavailable(true);
    let refill = device
        .engine
        .refill_tokens(&device.issuer.info(), &bearer(), WakeBudget::UNLIMITED)
        .await;
    assert!(matches!(
        refill,
        Err(SyncError::Status {
            route: Route::PrivacyPassIssue,
            status: 503
        })
    ));
    assert_eq!(device.engine.token_count().await?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_tokens_or_proofs_nothing_is_sent() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    // No RLN identity: the main app falls back to tokens, and holds none.
    assert_eq!(
        device.engine.credential_choice().await?,
        CredentialChoice::PrivacyPass
    );
    let registered = device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await;
    assert!(matches!(registered, Err(SyncError::NoCredential)));
    assert!(device.link.seen().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_extension_spends_tokens_even_where_proofs_are_accepted() -> TestResult {
    let world = World::start()?;
    let address = world.harness.endpoints().ohttp_relay.to_string();
    let device = world.device_with(&address, ProcessRole::Extension)?;
    assert_eq!(
        device.engine.credential_choice().await?,
        CredentialChoice::PrivacyPass
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_that_leaves_nothing_is_remembered_until_the_next_day() -> TestResult {
    let world = World::start()?;
    // Another install of the same account took the whole day's tokens.
    let other = world.device_with_stock(whole_day()?)?;
    assert!(matches!(
        other.stock_tokens().await?,
        Refill::Fetched { added: 300, .. }
    ));
    let device = world.device_with_stock(whole_day()?)?;
    assert!(matches!(
        device.stock_tokens().await?,
        Refill::DailyLimitReached { held: 0, .. }
    ));
    assert_eq!(device.issuer.requests(), 1);
    // The server's "none left" was kept: no second refusal is asked for today.
    assert!(matches!(
        device.stock_tokens().await?,
        Refill::DailyLimitReached { held: 0, .. }
    ));
    assert_eq!(device.issuer.requests(), 1);
    Ok(())
}
