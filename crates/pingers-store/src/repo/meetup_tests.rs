// SPDX-License-Identifier: AGPL-3.0-only
//! Tests for the Meetup repositories, including deletion at Meetup end.

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_mls::MlsStorage as _;
use zeroize::Zeroizing;

use super::*;
use crate::Database;
use crate::test_support::{Fixture, TestDir, TestError, key, party};

const MEETUP_TABLES: [&str; 6] = [
    "meetup",
    "meetup_msg",
    "meetup_peer",
    "meetup_self_lk",
    "location_grant",
    "location_ask",
];

fn meetup(party: &PartyId, fill: u8) -> Meetup {
    Meetup {
        group: GroupId::from_bytes([fill; 32]),
        party: *party,
        spot: SpotId::from_bytes([fill; 16]),
        started_at: UnixSeconds::new(500 + u64::from(fill)),
        grant_key: GrantSecretKey::from_bytes(Zeroizing::new([fill; 32])),
    }
}

fn chat(group: &GroupId, fill: u8, written_at: u64) -> Result<StoredChat, TestError> {
    Ok(StoredChat {
        group: *group,
        id: ChatMessageId::from_bytes([fill; 16]),
        parent: None,
        sender: LeafIndex::new(1),
        body: ChatBody::new(format!("message {fill}"))?,
        written_at: UnixSeconds::new(written_at),
    })
}

fn granted(fill: u8, expires_at: u64) -> GrantedKey {
    GrantedKey {
        generation: LocationGeneration::new(u32::from(fill)),
        key: LocationKey::from_bytes(Zeroizing::new([fill; 32])),
        expires_at: UnixSeconds::new(expires_at),
    }
}

fn position(latitude_e7: i32, taken_at: u64) -> Result<LocationPlaintext, TestError> {
    Ok(LocationPlaintext {
        position: E7Coordinate::new(latitude_e7, 23_522_000)?,
        accuracy: AccuracyMeters::new(12),
        heading: Some(HeadingDegrees::new(270)?),
        speed: Some(SpeedCentimetresPerSecond::new(140)),
        taken_at: UnixSeconds::new(taken_at),
    })
}

fn request(group: &GroupId, fill: u8, created_at: u64) -> LocationRequest {
    LocationRequest {
        group: *group,
        id: LocationRequestId::from_bytes([fill; 16]),
        direction: RequestDirection::Incoming,
        peer: LeafIndex::new(u32::from(fill)),
        created_at: UnixSeconds::new(created_at),
        state: LocationRequestState::Pending,
    }
}

fn self_key(generation: u32, fill: u8) -> SelfLocationKey {
    SelfLocationKey {
        generation: LocationGeneration::new(generation),
        key: LocationKey::from_bytes(Zeroizing::new([fill; 32])),
    }
}

/// Fills every Meetup table for `meetup`; its secrets are all `fill` bytes.
fn populate(session: &Session, meetup: &Meetup, fill: u8) -> Result<(), TestError> {
    session.meetups().start(meetup)?;
    session
        .meetup_messages()
        .insert(&chat(&meetup.group, 1, 10)?)?;
    let peers = session.meetup_peers();
    peers.seen(&meetup.group, LeafIndex::new(2), UnixSeconds::new(10))?;
    peers.set_key(&meetup.group, LeafIndex::new(2), &granted(fill, 900))?;
    peers.set_location(
        &meetup.group,
        LeafIndex::new(2),
        &position(488_566_000, 10)?,
    )?;
    session
        .self_location_keys()
        .insert(&meetup.group, &self_key(0, fill))?;
    session.location_grants().put(&OutgoingGrant {
        group: meetup.group,
        grantee: LeafIndex::new(2),
        generation: LocationGeneration::new(0),
        scope: GrantScope::ThisMeetup,
        expires_at: UnixSeconds::new(900),
    })?;
    session
        .location_asks()
        .insert(&request(&meetup.group, 2, 10))?;
    Ok(())
}

fn rows_for(fixture: &Fixture, group: &GroupId) -> Result<Vec<i64>, StoreError> {
    MEETUP_TABLES
        .iter()
        .map(|table| {
            fixture.session.with(|connection| {
                Ok(connection.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE group_id = ?1"),
                    [group.as_bytes()],
                    |row| row.get(0),
                )?)
            })
        })
        .collect()
}

#[test]
fn ending_a_meetup_leaves_no_rows() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    let ended = meetup(&owner.id, 1);
    let other = meetup(&owner.id, 2);
    populate(session, &ended, 1)?;
    populate(session, &other, 2)?;
    assert_eq!(rows_for(&fixture, &ended.group)?, vec![1; 6]);
    session.meetups().end(&ended.group)?;
    assert_eq!(rows_for(&fixture, &ended.group)?, vec![0; 6]);
    assert_eq!(rows_for(&fixture, &other.group)?, vec![1; 6]);
    assert!(session.meetups().get(&ended.group)?.is_none());
    assert!(matches!(
        session.meetups().end(&ended.group),
        Err(StoreError::NotFound { table: "meetup" })
    ));
    Ok(())
}

#[test]
fn an_out_of_range_stored_coordinate_is_corrupt() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    let live = meetup(&owner.id, 1);
    populate(session, &live, 1)?;
    let stored = 98_765_432_101_i64;
    session.with(|connection| {
        Ok(connection.execute(
            "UPDATE meetup_peer SET location_lat_e7 = ?1 WHERE group_id = ?2",
            rusqlite::params![stored, live.group.as_bytes()],
        )?)
    })?;
    let result = session.meetup_peers().get(&live.group, LeafIndex::new(2));
    assert!(matches!(
        result,
        Err(StoreError::Corrupt {
            field: "meetup_peer.location_lat_e7"
        })
    ));
    Ok(())
}

#[test]
fn ending_a_meetup_deletes_its_mls_state() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    let ended = meetup(&owner.id, 1);
    let other = meetup(&owner.id, 2);
    session.meetups().start(&ended)?;
    session.meetups().start(&other)?;
    let mut ended_key = pingers_mls::group_storage_prefix(&ended.group)?;
    ended_key.extend_from_slice(b"epoch_secrets");
    let mut other_key = pingers_mls::group_storage_prefix(&other.group)?;
    other_key.extend_from_slice(b"epoch_secrets");
    session.write(&ended_key, &[0x5a; 32])?;
    session.write(&other_key, &[0x5b; 32])?;
    session.write(b"key_package/global", &[0x5c; 32])?;
    session.meetups().end(&ended.group)?;
    assert!(session.read(&ended_key)?.is_none());
    assert!(session.read(&other_key)?.is_some());
    assert!(session.read(b"key_package/global")?.is_some());
    Ok(())
}

/// Decrypts the whole database into a plaintext copy and returns its bytes.
fn decrypted_file(directory: &TestDir, path: &std::path::Path) -> Result<Vec<u8>, TestError> {
    let plain = directory.file("plain.db");
    let connection = crate::database::open_keyed(path, &key(1))?;
    connection.execute(
        "ATTACH DATABASE ?1 AS plaintext KEY ''",
        [plain.to_string_lossy()],
    )?;
    connection.query_row("SELECT sqlcipher_export('plaintext')", [], |_| Ok(()))?;
    connection.execute_batch("DETACH DATABASE plaintext")?;
    Ok(std::fs::read(&plain)?)
}

fn contains(haystack: &[u8], needle: &[u8; 32]) -> bool {
    haystack.windows(32).any(|window| window == needle)
}

#[test]
fn an_ended_meetups_keys_are_nowhere_in_the_database() -> Result<(), TestError> {
    let directory = TestDir::new()?;
    let path = directory.file("store.db");
    let database = Database::open(&path, key(1))?;
    let session = database.session()?;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    let ended = meetup(&owner.id, 0x5a);
    populate(&session, &ended, 0x5a)?;
    // Other data on the same pages, so the deleted rows' pages stay in use and only
    // secure_delete clears the freed cells.
    for fill in 0..50 {
        session.write(&[fill], &[0xee; 64])?;
    }
    session.checkpoint()?;
    assert!(contains(&decrypted_file(&directory, &path)?, &[0x5a; 32]));
    std::fs::remove_file(directory.file("plain.db"))?;
    session.meetups().end(&ended.group)?;
    drop(session);
    let bytes = decrypted_file(&directory, &path)?;
    assert!(
        !contains(&bytes, &[0x5a; 32]),
        "an ended Meetup's key is still stored"
    );
    Ok(())
}

#[test]
fn ending_inside_a_transaction_defers_the_checkpoint() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let owner = party(1)?;
    fixture.session.parties().insert(&owner)?;
    let live = meetup(&owner.id, 1);
    populate(&fixture.session, &live, 1)?;
    fixture
        .session
        .transaction(|session| session.meetups().end(&live.group))?;
    assert_eq!(fixture.rows("meetup")?, 0);
    Ok(())
}

#[test]
fn deleting_the_party_ends_its_meetups() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let owner = party(1)?;
    fixture.session.parties().insert(&owner)?;
    populate(&fixture.session, &meetup(&owner.id, 1), 1)?;
    fixture.session.parties().delete(&owner.id)?;
    for table in MEETUP_TABLES {
        assert_eq!(fixture.rows(table)?, 0, "{table}");
    }
    Ok(())
}

#[test]
fn meetup_rows_need_a_live_meetup() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let group = GroupId::from_bytes([9; 32]);
    assert!(
        fixture
            .session
            .self_location_keys()
            .insert(&group, &self_key(0, 1))
            .is_err()
    );
    assert!(
        fixture
            .session
            .meetup_peers()
            .seen(&group, LeafIndex::new(1), UnixSeconds::new(1))
            .is_err()
    );
    Ok(())
}

#[test]
fn meetups_round_trip() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let owner = party(1)?;
    fixture.session.parties().insert(&owner)?;
    let meetups = fixture.session.meetups();
    let later = meetup(&owner.id, 2);
    let earlier = meetup(&owner.id, 1);
    meetups.start(&later)?;
    meetups.start(&earlier)?;
    assert!(meetups.start(&earlier).is_err());
    let loaded = meetups
        .get(&later.group)?
        .ok_or(StoreError::NotFound { table: "meetup" })?;
    assert_eq!(loaded.party, later.party);
    assert_eq!(loaded.spot, later.spot);
    assert_eq!(loaded.started_at, later.started_at);
    assert_eq!(loaded.grant_key.expose_bytes(), &[2; 32]);
    let groups: Vec<_> = meetups.list()?.iter().map(|meetup| meetup.group).collect();
    assert_eq!(groups, vec![earlier.group, later.group]);
    Ok(())
}

fn live_meetup(fixture: &Fixture) -> Result<Meetup, TestError> {
    let owner = party(1)?;
    fixture.session.parties().insert(&owner)?;
    let live = meetup(&owner.id, 1);
    fixture.session.meetups().start(&live)?;
    Ok(live)
}

#[test]
fn chat_is_listed_in_writing_order_without_duplicates() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let live = live_meetup(&fixture)?;
    let messages = fixture.session.meetup_messages();
    let root = chat(&live.group, 1, 20)?;
    let reply = StoredChat {
        parent: Some(root.id),
        sender: LeafIndex::new(4),
        ..chat(&live.group, 2, 30)?
    };
    let first = chat(&live.group, 3, 10)?;
    messages.insert(&reply)?;
    messages.insert(&root)?;
    messages.insert(&first)?;
    messages.insert(&StoredChat {
        body: ChatBody::new("tampered".into())?,
        ..root.clone()
    })?;
    assert_eq!(messages.list(&live.group)?, vec![first, root, reply]);
    Ok(())
}

#[test]
fn a_peer_position_needs_their_key_and_goes_with_it() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let live = live_meetup(&fixture)?;
    let peers = fixture.session.meetup_peers();
    let ben = LeafIndex::new(3);
    assert!(matches!(
        peers.set_key(&live.group, ben, &granted(1, 100)),
        Err(StoreError::NotFound {
            table: "meetup_peer"
        })
    ));
    peers.seen(&live.group, ben, UnixSeconds::new(5))?;
    assert!(
        peers
            .set_location(&live.group, ben, &position(1, 5)?)
            .is_err()
    );
    peers.set_key(&live.group, ben, &granted(1, 100))?;
    let here = position(488_566_000, 6)?;
    peers.set_location(&live.group, ben, &here)?;
    peers.seen(&live.group, ben, UnixSeconds::new(7))?;
    let missing = || StoreError::NotFound {
        table: "meetup_peer",
    };
    let loaded = peers.get(&live.group, ben)?.ok_or_else(missing)?;
    assert_eq!(loaded.last_seen, UnixSeconds::new(7));
    assert_eq!(loaded.location, Some(here));
    let key = loaded.key.ok_or_else(missing)?;
    assert_eq!(key.generation, LocationGeneration::new(1));
    assert_eq!(key.key.expose_bytes(), &[1; 32]);
    assert_eq!(key.expires_at, UnixSeconds::new(100));
    peers.clear_key(&live.group, ben)?;
    let cleared = peers.get(&live.group, ben)?.ok_or_else(missing)?;
    assert!(cleared.key.is_none());
    assert!(cleared.location.is_none());
    assert_eq!(cleared.last_seen, UnixSeconds::new(7));
    Ok(())
}

#[test]
fn a_new_position_overwrites_the_old_one() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let live = live_meetup(&fixture)?;
    let peers = fixture.session.meetup_peers();
    let ben = LeafIndex::new(3);
    peers.seen(&live.group, ben, UnixSeconds::new(5))?;
    peers.set_key(&live.group, ben, &granted(1, 100))?;
    peers.set_location(&live.group, ben, &position(100, 5)?)?;
    let without_extras = LocationPlaintext {
        heading: None,
        speed: None,
        ..position(200, 6)?
    };
    peers.set_location(&live.group, ben, &without_extras)?;
    assert_eq!(fixture.rows("meetup_peer")?, 1);
    let loaded = peers.get(&live.group, ben)?;
    assert_eq!(loaded.and_then(|peer| peer.location), Some(without_extras));
    Ok(())
}

#[test]
fn peers_are_listed_by_leaf_and_removed() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let live = live_meetup(&fixture)?;
    let peers = fixture.session.meetup_peers();
    for leaf in [5, 2, 9] {
        peers.seen(&live.group, LeafIndex::new(leaf), UnixSeconds::new(1))?;
    }
    let leaves =
        |list: Vec<MeetupPeer>| -> Vec<u32> { list.iter().map(|peer| peer.leaf.get()).collect() };
    assert_eq!(leaves(peers.list(&live.group)?), vec![2, 5, 9]);
    peers.remove(&live.group, LeafIndex::new(5))?;
    assert_eq!(leaves(peers.list(&live.group)?), vec![2, 9]);
    assert!(peers.get(&live.group, LeafIndex::new(5))?.is_none());
    let not_found = |result: Result<(), StoreError>| {
        matches!(
            result,
            Err(StoreError::NotFound {
                table: "meetup_peer"
            })
        )
    };
    assert!(not_found(peers.remove(&live.group, LeafIndex::new(5))));
    assert!(not_found(peers.clear_key(&live.group, LeafIndex::new(5))));
    assert!(not_found(peers.set_location(
        &live.group,
        LeafIndex::new(5),
        &position(1, 1)?
    )));
    Ok(())
}

#[test]
fn self_keys_rotate_to_the_newest_generation() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let live = live_meetup(&fixture)?;
    let keys = fixture.session.self_location_keys();
    assert!(keys.current(&live.group)?.is_none());
    keys.insert(&live.group, &self_key(0, 10))?;
    keys.insert(&live.group, &self_key(2, 12))?;
    keys.insert(&live.group, &self_key(1, 11))?;
    assert!(keys.insert(&live.group, &self_key(1, 11)).is_err());
    let current = keys.current(&live.group)?.ok_or(StoreError::NotFound {
        table: "meetup_self_lk",
    })?;
    assert_eq!(current.generation, LocationGeneration::new(2));
    assert_eq!(current.key.expose_bytes(), &[12; 32]);
    assert_eq!(
        keys.delete_before(&live.group, LocationGeneration::new(2))?,
        2
    );
    assert_eq!(fixture.rows("meetup_self_lk")?, 1);
    Ok(())
}

#[test]
fn grants_replace_revoke_and_expire() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let live = live_meetup(&fixture)?;
    let grants = fixture.session.location_grants();
    let grant = |leaf: u32, scope, expires_at: u64| OutgoingGrant {
        group: live.group,
        grantee: LeafIndex::new(leaf),
        generation: LocationGeneration::new(0),
        scope,
        expires_at: UnixSeconds::new(expires_at),
    };
    grants.put(&grant(4, GrantScope::FifteenMinutes, 100))?;
    grants.put(&grant(2, GrantScope::ThisMeetup, 900))?;
    grants.put(&grant(3, GrantScope::FifteenMinutes, 50))?;
    let rotated = OutgoingGrant {
        generation: LocationGeneration::new(1),
        ..grant(2, GrantScope::ThisMeetup, 900)
    };
    grants.put(&rotated)?;
    assert_eq!(
        grants.list(&live.group)?,
        vec![
            rotated,
            grant(3, GrantScope::FifteenMinutes, 50),
            grant(4, GrantScope::FifteenMinutes, 100)
        ]
    );
    assert_eq!(
        grants.take_expired(&live.group, UnixSeconds::new(49))?,
        vec![]
    );
    assert_eq!(
        grants.take_expired(&live.group, UnixSeconds::new(100))?,
        vec![LeafIndex::new(3), LeafIndex::new(4)]
    );
    grants.revoke(&live.group, LeafIndex::new(2))?;
    assert!(grants.list(&live.group)?.is_empty());
    assert!(matches!(
        grants.revoke(&live.group, LeafIndex::new(2)),
        Err(StoreError::NotFound {
            table: "location_grant"
        })
    ));
    Ok(())
}

#[test]
fn asks_change_state_and_expire() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let live = live_meetup(&fixture)?;
    let asks = fixture.session.location_asks();
    let incoming = request(&live.group, 1, 200);
    let outgoing = LocationRequest {
        direction: RequestDirection::Outgoing,
        ..request(&live.group, 2, 100)
    };
    asks.insert(&incoming)?;
    asks.insert(&outgoing)?;
    assert!(asks.insert(&incoming).is_err());
    asks.set_state(&live.group, &incoming.id, LocationRequestState::Approved)?;
    asks.set_state(&live.group, &outgoing.id, LocationRequestState::Lapsed)?;
    assert_eq!(
        asks.list(&live.group)?,
        vec![
            LocationRequest {
                state: LocationRequestState::Lapsed,
                ..outgoing
            },
            LocationRequest {
                state: LocationRequestState::Approved,
                ..incoming
            }
        ]
    );
    asks.set_state(&live.group, &incoming.id, LocationRequestState::Pending)?;
    assert_eq!(
        asks.list(&live.group)?.last().map(|ask| ask.state),
        Some(LocationRequestState::Pending)
    );
    assert_eq!(asks.delete_created_before(UnixSeconds::new(100))?, 0);
    assert_eq!(asks.delete_created_before(UnixSeconds::new(150))?, 1);
    assert_eq!(asks.list(&live.group)?.len(), 1);
    assert!(matches!(
        asks.set_state(&live.group, &outgoing.id, LocationRequestState::Pending),
        Err(StoreError::NotFound {
            table: "location_ask"
        })
    ));
    Ok(())
}

#[test]
fn ending_a_meetup_forgets_its_subscription() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    let ended = meetup(&owner.id, 1);
    let other = meetup(&owner.id, 2);
    for (meetup, fill) in [(&ended, 1_u8), (&other, 2)] {
        session.meetups().start(meetup)?;
        session.subscriptions().put(&crate::StoredSubscription {
            group: meetup.group,
            id: pingers_proto::SubscriptionId::from_bytes([fill; 16]),
            handle: pingers_proto::PushHandle::from_bytes([fill; 8]),
            expires_at: UnixSeconds::new(9_000),
            seal: None,
        })?;
    }
    session.meetups().end(&ended.group)?;
    assert!(session.subscriptions().get(&ended.group)?.is_none());
    assert!(session.subscriptions().get(&other.group)?.is_some());
    Ok(())
}
