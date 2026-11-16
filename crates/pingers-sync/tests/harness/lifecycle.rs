// SPDX-License-Identifier: AGPL-3.0-only
//! Mailbox and subscription lifetimes, policy changes, and the live channel's guard rails.

use std::time::Duration;

use pingers_crypto::{LogAuthKeyPair, PushRelayPrivateKey};
use pingers_proto::{GroupId, SubscriptionId, TimeToLive};
use pingers_store::MailboxPurpose;
use pingers_sync::{
    ApiRequest, CredentialPolicy, Delivered, GroupSigner, NewMailbox, ProverStatus, Route, Sending,
    SyncError, WakeBudget,
};
use pingers_testkit::seeded_rng;
use pingers_time::Clock as _;
use pingers_transport::TransportError;
use zeroize::Zeroizing;

use crate::mailbox::{knock_mailbox, seal};
use crate::support::{TestError, TestResult, World};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mailbox_the_server_already_forgot_is_forgotten_locally() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let budget = WakeBudget::UNLIMITED;
    let Delivered::Done(mailbox) = device
        .engine
        .register_mailbox(&knock_mailbox()?, budget)
        .await?
    else {
        return Err(TestError::Unexpected("registration was not sent"));
    };
    // Delete it on the server behind the engine's back.
    let stored = device
        .session()?
        .mailboxes()
        .get(&mailbox)?
        .ok_or(TestError::Unexpected("not stored"))?;
    let deleted = device
        .engine
        .send(
            ApiRequest::delete_mailbox(&mailbox, &stored.read_key),
            Sending::or_drop(budget),
        )
        .await?
        .answered(Route::DeleteMailbox)?;
    assert_eq!(deleted.status(), 204);

    device.engine.delete_mailbox(&mailbox, budget).await?;
    assert!(device.session()?.mailboxes().get(&mailbox)?.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_mailboxes_and_subscriptions_are_forgotten() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let budget = WakeBudget::UNLIMITED;
    let short = NewMailbox {
        lifetime: TimeToLive::new(Duration::from_mins(1))?,
        ..knock_mailbox()?
    };
    let _ = device.engine.register_mailbox(&short, budget).await?;
    let _ = device
        .engine
        .register_mailbox(&knock_mailbox()?, budget)
        .await?;
    let relay_key = PushRelayPrivateKey::generate(&mut seeded_rng(3)).public_key(1)?;
    let subscription = device
        .engine
        .new_subscription(&seal(relay_key), TimeToLive::new(Duration::from_mins(1))?)?;
    let group = GroupId::from_bytes([3; 32]);
    device
        .engine
        .remember_subscription(&group, &subscription)
        .await?;
    assert_eq!(
        device.engine.forget_expired().await?,
        0,
        "nothing has expired yet"
    );

    world.clock.advance(Duration::from_secs(61));
    assert_eq!(device.engine.forget_expired().await?, 2);
    assert_eq!(
        device
            .engine
            .pooled_mailboxes(MailboxPurpose::Knock)
            .await?,
        1
    );
    assert!(device.session()?.subscriptions().get(&group)?.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_policy_applies_to_the_next_request() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    device.engine.set_credential_policy(CredentialPolicy {
        accepted: Vec::new(),
        prover: ProverStatus::Enabled,
    });
    assert!(matches!(
        device.engine.credential_choice().await,
        Err(SyncError::NoCredential)
    ));
    assert!(matches!(
        device
            .engine
            .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
            .await,
        Err(SyncError::NoCredential)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_live_channel_goes_only_through_the_relay() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let key = LogAuthKeyPair::from_bytes(Zeroizing::new([1; 32]));
    let signer = GroupSigner {
        key: &key,
        now: world.clock.now(),
    };
    let live = || {
        ApiRequest::live(
            &GroupId::from_bytes([4; 32]),
            Some(&SubscriptionId::from_bytes([5; 16])),
            &signer,
        )
    };
    // Only the live route may open a stream.
    assert!(matches!(
        device.engine.open_live(ApiRequest::params()).await,
        Err(SyncError::Transport(TransportError::ClassNotAllowed))
    ));
    device.link.set_offline(true);
    assert!(matches!(
        device.engine.open_live(live()?).await,
        Err(SyncError::NoRelay { route: Route::Live })
    ));
    Ok(())
}
