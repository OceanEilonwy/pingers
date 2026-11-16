// SPDX-License-Identifier: AGPL-3.0-only
//! Tests for the `server` table and per-server scoping.

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_proto::{GroupId, KeyConfigHash, PartyId, PushHandle, ServerId, SubscriptionId};
use pingers_time::UnixSeconds;

use crate::server::pins_from_blob;
use crate::test_support::{Fixture, TestError, party, server_record, test_server};
use crate::{
    AccountRegistration, HandleKind, HandleOwner, ServerRecord, ServerTrust, StoreError,
    StoredSubscription, TokenKeyId, UnspentToken,
};

const OTHER: &str = "https://other.example";

fn other() -> Result<ServerId, TestError> {
    Ok(ServerId::new(OTHER.to_owned())?)
}

fn with_other() -> Result<Fixture, TestError> {
    let fixture = Fixture::new()?;
    fixture.session.servers().put(&server_record(OTHER)?)?;
    Ok(fixture)
}

#[test]
fn a_server_record_round_trips_and_an_update_keeps_its_rows() -> Result<(), TestError> {
    let fixture = with_other()?;
    let record = ServerRecord {
        server: other()?,
        ohttp_relays: vec![
            "https://relay-a.example/ohttp".to_owned(),
            "https://relay-b.example".to_owned(),
        ],
        push_relay: Some("https://push.example".to_owned()),
        gateway_pins: vec![
            KeyConfigHash::from_bytes([1; 32]),
            KeyConfigHash::from_bytes([2; 32]),
        ],
        push_relay_pins: vec![KeyConfigHash::from_bytes([3; 32])],
        trust: ServerTrust::Invite,
    };
    fixture
        .session
        .servers()
        .set_server_pin(&record.server, &KeyConfigHash::from_bytes([9; 32]))?;
    fixture
        .session
        .server(&record.server)?
        .account()
        .set_registration(&AccountRegistration::Registered { code: None })?;
    fixture.session.servers().put(&record)?;
    assert_eq!(
        fixture.session.servers().get(&record.server)?,
        Some(record.clone())
    );
    // Reconfiguring a server keeps what belongs to it.
    assert_eq!(
        fixture.session.servers().server_pin(&record.server)?,
        Some(KeyConfigHash::from_bytes([9; 32]))
    );
    assert!(matches!(
        fixture
            .session
            .server(&record.server)?
            .account()
            .registration()?,
        AccountRegistration::Registered { .. }
    ));
    let fewer = ServerRecord {
        ohttp_relays: vec!["https://relay-c.example".to_owned()],
        trust: ServerTrust::Confirmed,
        ..record
    };
    fixture.session.servers().put(&fewer)?;
    assert_eq!(fixture.session.servers().get(&fewer.server)?, Some(fewer));
    Ok(())
}

#[test]
fn servers_are_listed_in_the_order_they_were_added() -> Result<(), TestError> {
    let fixture = with_other()?;
    let order: Vec<ServerId> = fixture
        .session
        .servers()
        .list()?
        .into_iter()
        .map(|record| record.server)
        .collect();
    assert_eq!(order, [test_server()?, other()?]);
    Ok(())
}

#[test]
fn an_unknown_server_has_no_rows_to_reach() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    assert!(matches!(
        fixture.session.server(&other()?),
        Err(StoreError::UnknownServer)
    ));
    assert!(matches!(
        fixture.session.servers().server_pin(&other()?),
        Err(StoreError::UnknownServer)
    ));
    assert!(matches!(
        fixture
            .session
            .servers()
            .set_server_pin(&other()?, &KeyConfigHash::from_bytes([1; 32])),
        Err(StoreError::NotFound { .. })
    ));
    let mut stray = party(4)?;
    stray.server = other()?;
    assert!(matches!(
        fixture.session.parties().insert(&stray),
        Err(StoreError::UnknownServer)
    ));
    Ok(())
}

#[test]
fn each_server_keeps_its_own_account_tokens_and_queue() -> Result<(), TestError> {
    let fixture = with_other()?;
    let first = fixture.server()?;
    let second = fixture.session.server(&other()?)?;
    first
        .account()
        .set_registration(&AccountRegistration::Registered { code: None })?;
    second.privacy_pass_tokens().add(
        &[
            UnspentToken::from_bytes(vec![1]),
            UnspentToken::from_bytes(vec![2]),
        ],
        &TokenKeyId::from_bytes([0; 32]),
    )?;
    assert!(matches!(
        second.account().registration()?,
        AccountRegistration::Pending { code: None }
    ));
    assert_eq!(first.privacy_pass_tokens().count()?, 0);
    assert!(first.privacy_pass_tokens().take()?.is_none());
    assert_eq!(second.privacy_pass_tokens().count()?, 2);
    Ok(())
}

#[test]
fn deleting_a_server_deletes_its_rows_and_parties_only() -> Result<(), TestError> {
    let fixture = with_other()?;
    let mut elsewhere = party(2)?;
    elsewhere.server = other()?;
    fixture.session.parties().insert(&party(1)?)?;
    fixture.session.parties().insert(&elsewhere)?;
    for server in [test_server()?, other()?] {
        fixture
            .session
            .server(&server)?
            .account()
            .set_registration(&AccountRegistration::Registered { code: None })?;
    }
    fixture.session.servers().delete(&other()?)?;
    let left: Vec<PartyId> = fixture
        .session
        .parties()
        .list()?
        .into_iter()
        .map(|party| party.id)
        .collect();
    assert_eq!(left, [party(1)?.id]);
    assert_eq!(fixture.rows("account_registration")?, 1);
    assert!(matches!(
        fixture.session.servers().delete(&other()?),
        Err(StoreError::NotFound { .. })
    ));
    Ok(())
}

#[test]
fn a_push_handle_names_its_server_and_kind() -> Result<(), TestError> {
    let fixture = with_other()?;
    let handle = PushHandle::from_bytes([6; 8]);
    assert_eq!(fixture.session.push_handle_owner(&handle)?, None);
    fixture
        .session
        .server(&other()?)?
        .subscriptions()
        .put(&StoredSubscription {
            group: GroupId::from_bytes([1; 32]),
            id: SubscriptionId::from_bytes([2; 16]),
            handle,
            expires_at: UnixSeconds::new(100),
            seal: None,
        })?;
    assert_eq!(
        fixture.session.push_handle_owner(&handle)?,
        Some(HandleOwner {
            server: other()?,
            kind: HandleKind::Subscription,
        })
    );
    Ok(())
}

#[test]
fn pins_and_trust_that_do_not_decode_are_corrupt() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    assert_eq!(pins_from_blob(&[0; 64], "test")?.len(), 2);
    assert!(matches!(
        pins_from_blob(&[0; 33], "test"),
        Err(StoreError::Corrupt { .. })
    ));
    fixture.session.with(|connection| {
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON; UPDATE server SET trust = 7;")?;
        Ok(())
    })?;
    assert!(matches!(
        fixture.session.servers().list(),
        Err(StoreError::Corrupt {
            field: "server.trust"
        })
    ));
    Ok(())
}
