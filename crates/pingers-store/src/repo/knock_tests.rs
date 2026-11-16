// SPDX-License-Identifier: AGPL-3.0-only
//! Tests for `knock_key`.

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;

use pingers_proto::{
    PushHandle, PushRelayKeyId, RelayId, RelayRef, SealedPush, SealedPushBlob, SubscriptionId,
    TimeToLive, Urgency,
};
use zeroize::Zeroizing;

use super::*;
use crate::test_support::{Fixture, TestError, party};

fn subscription(fill: u8) -> Result<Subscription, TestError> {
    Ok(Subscription {
        id: SubscriptionId::from_bytes([fill; 16]),
        push: SealedPush {
            relay: RelayRef::Listed(RelayId::new(u16::from(fill))),
            key_id: PushRelayKeyId::new(fill),
            blob: SealedPushBlob::from_bytes([fill; 320]),
            urgency: Urgency::Alert,
            handle: PushHandle::from_bytes([fill; 8]),
        },
        time_to_live: TimeToLive::new(Duration::from_hours(24))?,
    })
}

fn knock(party: &PartyId, fill: u8, created_at: u64) -> Result<KnockKey, TestError> {
    Ok(KnockKey {
        reply_mailbox: MailboxId::from_bytes([fill; 16]),
        party: *party,
        grant_key: GrantSecretKey::from_bytes(Zeroizing::new([fill; 32])),
        subscription: Some(subscription(fill)?),
        created_at: UnixSeconds::new(created_at),
    })
}

#[test]
fn a_knock_key_comes_back_by_its_reply_mailbox() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    session.knock_keys().put(&knock(&owner.id, 3, 900)?)?;
    let without = KnockKey {
        subscription: None,
        ..knock(&owner.id, 4, 950)?
    };
    session.knock_keys().put(&without)?;

    let kept = session
        .knock_keys()
        .get(&MailboxId::from_bytes([3; 16]))?
        .ok_or(StoreError::NotFound { table: "knock_key" })?;
    assert_eq!(kept.party, owner.id);
    assert_eq!(kept.grant_key.expose_bytes(), &[3; 32]);
    assert_eq!(kept.subscription, Some(subscription(3)?));
    assert_eq!(kept.created_at, UnixSeconds::new(900));
    let bare = session
        .knock_keys()
        .get(&MailboxId::from_bytes([4; 16]))?
        .ok_or(StoreError::NotFound { table: "knock_key" })?;
    assert_eq!(bare.subscription, None);
    assert!(
        session
            .knock_keys()
            .get(&MailboxId::from_bytes([5; 16]))?
            .is_none()
    );
    Ok(())
}

#[test]
fn deleting_a_knock_key_forgets_only_that_one() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    session.knock_keys().put(&knock(&owner.id, 3, 900)?)?;
    session.knock_keys().put(&knock(&owner.id, 4, 900)?)?;
    session
        .knock_keys()
        .delete(&MailboxId::from_bytes([3; 16]))?;
    // Deleting again is not an error: the Welcome may be read twice.
    session
        .knock_keys()
        .delete(&MailboxId::from_bytes([3; 16]))?;
    assert_eq!(fixture.rows("knock_key")?, 1);
    assert!(
        session
            .knock_keys()
            .get(&MailboxId::from_bytes([4; 16]))?
            .is_some()
    );
    Ok(())
}

#[test]
fn knock_keys_older_than_the_cutoff_are_deleted() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    session.knock_keys().put(&knock(&owner.id, 1, 899)?)?;
    session.knock_keys().put(&knock(&owner.id, 2, 900)?)?;
    session.knock_keys().put(&knock(&owner.id, 3, 901)?)?;
    assert_eq!(
        session.knock_keys().delete_before(UnixSeconds::new(900))?,
        1
    );
    // Made exactly at the cutoff: still waiting.
    assert!(
        session
            .knock_keys()
            .get(&MailboxId::from_bytes([2; 16]))?
            .is_some()
    );
    assert_eq!(fixture.rows("knock_key")?, 2);
    Ok(())
}

#[test]
fn leaving_the_party_deletes_its_knock_keys() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let left = party(1)?;
    let kept = party(2)?;
    session.parties().insert(&left)?;
    session.parties().insert(&kept)?;
    session.knock_keys().put(&knock(&left.id, 3, 900)?)?;
    session.knock_keys().put(&knock(&kept.id, 4, 900)?)?;
    session.parties().delete(&left.id)?;
    assert_eq!(fixture.rows("knock_key")?, 1);
    Ok(())
}

#[test]
fn a_reply_mailbox_holds_one_knock() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    session.knock_keys().put(&knock(&owner.id, 3, 900)?)?;
    let again = KnockKey {
        grant_key: GrantSecretKey::from_bytes(Zeroizing::new([9; 32])),
        ..knock(&owner.id, 3, 960)?
    };
    session.knock_keys().put(&again)?;
    let stored = session
        .knock_keys()
        .get(&MailboxId::from_bytes([3; 16]))?
        .ok_or(StoreError::NotFound { table: "knock_key" })?;
    assert_eq!(stored.grant_key.expose_bytes(), &[9; 32]);
    assert_eq!(stored.created_at, UnixSeconds::new(960));
    Ok(())
}

#[test]
fn an_undecodable_subscription_is_corrupt() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    session.knock_keys().put(&knock(&owner.id, 3, 900)?)?;
    session.with(|connection| {
        connection.execute("UPDATE knock_key SET subscription = x'00'", [])?;
        Ok(())
    })?;
    assert!(matches!(
        session.knock_keys().get(&MailboxId::from_bytes([3; 16])),
        Err(StoreError::Corrupt {
            field: "knock_key.subscription"
        })
    ));
    Ok(())
}
