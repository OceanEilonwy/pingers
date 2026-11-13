// SPDX-License-Identifier: AGPL-3.0-only
//! The Privacy Pass stock: refilled below its minimum, spent one per anonymous write.

use pingers_store::MailboxPurpose;
use pingers_sync::{
    CredentialChoice, Delivered, ProcessRole, Refill, Route, SyncError, WakeBudget,
};

use crate::mailbox::knock_mailbox;
use crate::support::{TestResult, World, bearer};

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
