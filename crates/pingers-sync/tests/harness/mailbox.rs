// SPDX-License-Identifier: AGPL-3.0-only
//! A mailbox's whole life through the relay, and sealed push for subscriptions.

use std::time::Duration;

use pingers_crypto::{
    PushRelayPrivateKey, PushTokenToOpen, open_push_token, push_bind_for_subscription,
};
use pingers_proto::{
    Padded, PushClass, PushPlatform, PushToken, PushTokenPlaintext, RelayId, RelayRef, SizeClass,
    TimeToLive, Urgency,
};
use pingers_store::MailboxPurpose;
use pingers_sync::{Delivered, NewMailbox, PushSeal, Route, Sending, SyncError, WakeBudget};
use pingers_testkit::seeded_rng;
use pingers_transport::PrivacyClass;

use crate::support::{TestError, TestResult, World};

pub(crate) fn knock_mailbox() -> Result<NewMailbox, TestError> {
    Ok(NewMailbox {
        purpose: MailboxPurpose::Knock,
        lifetime: TimeToLive::new(Duration::from_secs(3_600))?,
        push: None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mailbox_round_trip_through_the_relay() -> TestResult {
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
    assert_eq!(
        device.engine.token_count().await?,
        4,
        "registration spent a token"
    );
    assert_eq!(
        device
            .engine
            .pooled_mailboxes(MailboxPurpose::Knock)
            .await?,
        1
    );
    assert_eq!(
        device.engine.take_mailbox(MailboxPurpose::Knock).await?,
        Some(mailbox)
    );
    assert_eq!(
        device
            .engine
            .pooled_mailboxes(MailboxPurpose::Knock)
            .await?,
        0
    );

    let knock = Padded::pad(b"a knock", SizeClass::Bytes1024)?;
    let Delivered::Done(sequence) = device
        .engine
        .post_to_mailbox(&mailbox, &knock, Sending::or_drop(budget))
        .await?
    else {
        return Err(TestError::Unexpected("post was not sent"));
    };
    assert_eq!(
        device.engine.token_count().await?,
        3,
        "the post spent a token"
    );

    let read = device.engine.read_mailbox(&mailbox, None, budget).await?;
    assert_eq!(read.messages.len(), 1);
    assert_eq!(
        read.messages.first().map(|message| message.sequence),
        Some(sequence)
    );
    assert_eq!(
        read.messages
            .first()
            .map(|message| message.body.payload())
            .transpose()?,
        Some(b"a knock".as_slice())
    );
    let later = device
        .engine
        .read_mailbox(&mailbox, Some(sequence), budget)
        .await?;
    assert!(later.messages.is_empty());

    device.engine.delete_mailbox(&mailbox, budget).await?;
    assert!(matches!(
        device.engine.read_mailbox(&mailbox, None, budget).await,
        Err(SyncError::UnknownMailbox)
    ));

    // Everything went through the relay; only the writes carried a credential.
    let seen = device.link.seen();
    assert!(
        seen.iter()
            .all(|seen| seen.class == PrivacyClass::Anonymous)
    );
    let credentials: Vec<(&str, bool)> = seen
        .iter()
        .map(|seen| {
            let token = seen
                .authorization
                .as_deref()
                .is_some_and(|value| value.starts_with("PrivateToken token="));
            (seen.method.as_str(), token)
        })
        .collect();
    assert_eq!(
        credentials,
        [
            ("POST", true),
            ("POST", true),
            ("GET", false),
            ("GET", false),
            ("DELETE", false)
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mailbox_the_server_refuses_is_not_kept() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    // The harness server does not list relay 9, so the registration is refused (422).
    let relay_key = PushRelayPrivateKey::generate(&mut seeded_rng(3)).public_key(1)?;
    let mailbox = pingers_sync::NewMailbox {
        push: Some(seal(relay_key)),
        ..knock_mailbox()?
    };
    let refused = device
        .engine
        .register_mailbox(&mailbox, WakeBudget::UNLIMITED)
        .await;
    assert!(matches!(
        refused,
        Err(SyncError::Status {
            route: Route::CreateMailbox,
            status: 422
        })
    ));
    assert_eq!(
        device
            .engine
            .pooled_mailboxes(MailboxPurpose::Knock)
            .await?,
        0
    );
    Ok(())
}

pub(crate) fn seal(relay_key: pingers_crypto::PushRelayPublicKey) -> PushSeal {
    PushSeal {
        // Not the harness push relay (id 1), so a registration carrying it is refused.
        relay: RelayRef::Listed(RelayId::new(9)),
        relay_key,
        token: PushTokenPlaintext {
            platform: PushPlatform::UnifiedPush,
            class: PushClass::Standard,
            token: PushToken::from_bytes(b"https://push.example/abc".to_vec()),
            not_after: pingers_time::UnixSeconds::new(2_000_000_000),
        },
        urgency: Urgency::Quiet,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscription_is_sealed_to_the_relay_and_bound_to_its_id() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let relay_private = PushRelayPrivateKey::generate(&mut seeded_rng(3));
    let relay_key = relay_private.public_key(7)?;
    let lifetime = TimeToLive::new(Duration::from_hours(24))?;
    let subscription = device.engine.new_subscription(&seal(relay_key), lifetime)?;
    assert_eq!(subscription.push.key_id.get(), 7);
    assert_eq!(subscription.push.urgency, Urgency::Quiet);
    assert_eq!(subscription.time_to_live, lifetime);
    let opened = open_push_token(
        &relay_private,
        &PushTokenToOpen {
            key_id: 7,
            bind: push_bind_for_subscription(&subscription.id),
            blob: &subscription.push.blob,
        },
    )?;
    assert_eq!(opened.token.as_bytes(), b"https://push.example/abc");
    // Two subscriptions never share an id, handle or ciphertext.
    let other = device.engine.new_subscription(&seal(relay_key), lifetime)?;
    assert_ne!(other.id, subscription.id);
    assert_ne!(other.push.handle, subscription.push.handle);
    assert_ne!(other.push.blob, subscription.push.blob);
    Ok(())
}
