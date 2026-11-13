// SPDX-License-Identifier: AGPL-3.0-only
//! Tests for the request queue, the last board upload and own group entry positions.

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::num::NonZeroUsize;

use pingers_crypto::GrantSecretKey;
use pingers_proto::SpotId;

use super::*;
use crate::test_support::{Fixture, TestDir, TestError, key, party};
use crate::{Database, Meetup};

fn bytes(fill: u8) -> RequestBytes {
    RequestBytes::from_bytes(Zeroizing::new(vec![fill; 40]))
}

fn capacity(count: usize) -> QueueCapacity {
    QueueCapacity::new(NonZeroUsize::new(count).unwrap_or(NonZeroUsize::MIN))
}

fn push(
    session: &Session,
    group: Option<GroupId>,
    keep_until: u64,
    fill: u8,
) -> Result<(), StoreError> {
    let _: Pushed = session.request_queue().push(
        &NewQueuedRequest {
            group,
            keep_until: UnixSeconds::new(keep_until),
            request: &bytes(fill),
        },
        QueueCapacity::default(),
    )?;
    Ok(())
}

/// The fill byte of each queued request, oldest first, emptying the queue.
fn drain(session: &Session) -> Result<Vec<u8>, StoreError> {
    let mut fills = Vec::new();
    while let Some(entry) = session.request_queue().take_oldest()? {
        fills.extend(entry.request.expose_bytes().first());
    }
    Ok(fills)
}

fn meetup(party: &pingers_proto::PartyId, fill: u8) -> Meetup {
    Meetup {
        group: GroupId::from_bytes([fill; 32]),
        party: *party,
        spot: SpotId::from_bytes([fill; 16]),
        started_at: UnixSeconds::new(500),
        grant_key: GrantSecretKey::from_bytes(Zeroizing::new([fill; 32])),
    }
}

#[test]
fn a_taken_request_comes_back_whole() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let group = GroupId::from_bytes([3; 32]);
    push(&fixture.session, Some(group), 700, 9)?;
    let entry = fixture
        .session
        .request_queue()
        .take_oldest()?
        .ok_or(StoreError::NotFound {
            table: "request_queue",
        })?;
    assert_eq!(entry.group, Some(group));
    assert_eq!(entry.keep_until, UnixSeconds::new(700));
    assert_eq!(entry.request.expose_bytes(), [9; 40].as_slice());
    assert_eq!(fixture.session.request_queue().count()?, 0);
    assert!(fixture.session.request_queue().take_oldest()?.is_none());
    Ok(())
}

#[test]
fn a_request_put_back_stays_ahead_of_newer_ones() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    push(session, None, 700, 1)?;
    let taken = session
        .request_queue()
        .take_oldest()?
        .ok_or(StoreError::NotFound {
            table: "request_queue",
        })?;
    // The queue is empty now: a newer request must still not take the old one's place.
    push(session, None, 700, 2)?;
    session.request_queue().put_back(&taken)?;
    assert_eq!(drain(session)?, [1, 2]);
    Ok(())
}

#[test]
fn a_full_queue_drops_its_oldest_entry() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let queue = fixture.session.request_queue();
    let entry = |fill| NewQueuedRequest {
        group: None,
        keep_until: UnixSeconds::new(700),
        request: fill,
    };
    let (one, two, three) = (bytes(1), bytes(2), bytes(3));
    assert_eq!(queue.push(&entry(&one), capacity(2))?, Pushed::Queued);
    assert_eq!(queue.push(&entry(&two), capacity(2))?, Pushed::Queued);
    assert_eq!(
        queue.push(&entry(&three), capacity(2))?,
        Pushed::QueuedDroppingOldest
    );
    assert_eq!(queue.count()?, 2);
    assert_eq!(drain(&fixture.session)?, [2, 3]);
    Ok(())
}

#[test]
fn queued_requests_are_kept_through_their_last_second() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    push(session, None, 100, 1)?;
    push(session, None, 200, 2)?;
    assert_eq!(
        session
            .request_queue()
            .drop_expired(UnixSeconds::new(100))?,
        0
    );
    assert_eq!(
        session
            .request_queue()
            .drop_expired(UnixSeconds::new(101))?,
        1
    );
    assert_eq!(drain(session)?, [2]);
    Ok(())
}

#[test]
fn ending_a_meetup_forgets_its_queued_requests_and_own_entries() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let owner = party(1)?;
    session.parties().insert(&owner)?;
    let ended = meetup(&owner.id, 2);
    let other = meetup(&owner.id, 3);
    session.meetups().start(&ended)?;
    session.meetups().start(&other)?;
    push(session, Some(ended.group), 700, 2)?;
    push(session, Some(other.group), 700, 3)?;
    push(session, None, 700, 4)?;
    session
        .own_group_entries()
        .remember(&ended.group, SequenceNumber::new(5))?;
    session
        .own_group_entries()
        .remember(&other.group, SequenceNumber::new(5))?;
    session.meetups().end(&ended.group)?;
    assert!(
        session
            .own_group_entries()
            .positions(&ended.group)?
            .is_empty()
    );
    assert_eq!(
        session.own_group_entries().positions(&other.group)?,
        [SequenceNumber::new(5)]
    );
    assert_eq!(drain(session)?, [3, 4]);
    Ok(())
}

#[test]
fn deleting_a_party_forgets_its_and_its_meetups_requests() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let session = &fixture.session;
    let deleted = party(1)?;
    let kept = party(2)?;
    session.parties().insert(&deleted)?;
    session.parties().insert(&kept)?;
    let deleted_meetup = meetup(&deleted.id, 3);
    session.meetups().start(&deleted_meetup)?;
    push(session, Some(deleted.group), 700, 1)?;
    push(session, Some(kept.group), 700, 2)?;
    push(session, Some(deleted_meetup.group), 700, 3)?;
    session
        .own_group_entries()
        .remember(&deleted.group, SequenceNumber::new(1))?;
    session
        .own_group_entries()
        .remember(&deleted_meetup.group, SequenceNumber::new(1))?;
    session.parties().delete(&deleted.id)?;
    assert_eq!(drain(session)?, [2]);
    assert!(
        session
            .own_group_entries()
            .positions(&deleted.group)?
            .is_empty()
    );
    assert!(
        session
            .own_group_entries()
            .positions(&deleted_meetup.group)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn the_last_upload_replaces_the_one_before() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let uploads = fixture.session.board_upload();
    assert!(uploads.last()?.is_none());
    for fill in [1, 2] {
        uploads.remember(&LastUpload {
            lease: LeaseId::from_bytes([fill; 16]),
            revoke_key: RevokeKey::from_bytes(Zeroizing::new([fill; 32])),
        })?;
    }
    let last = uploads.last()?.ok_or(StoreError::NotFound {
        table: "board_upload",
    })?;
    assert_eq!(last.lease, LeaseId::from_bytes([2; 16]));
    assert_eq!(last.revoke_key.expose_bytes(), &[2; 32]);
    assert_eq!(fixture.rows("board_upload")?, 1);
    Ok(())
}

#[test]
fn own_entries_are_forgot_up_to_and_including_the_pulled_position() -> Result<(), TestError> {
    let fixture = Fixture::new()?;
    let entries = fixture.session.own_group_entries();
    let group = GroupId::from_bytes([1; 32]);
    let other = GroupId::from_bytes([2; 32]);
    for sequence in [9, 4, 7, 4] {
        entries.remember(&group, SequenceNumber::new(sequence))?;
    }
    entries.remember(&other, SequenceNumber::new(1))?;
    assert_eq!(
        entries.positions(&group)?,
        [4, 7, 9].map(SequenceNumber::new)
    );
    entries.forget_through(&group, SequenceNumber::new(7))?;
    assert_eq!(entries.positions(&group)?, [SequenceNumber::new(9)]);
    assert_eq!(entries.positions(&other)?, [SequenceNumber::new(1)]);
    Ok(())
}

#[test]
fn a_version_two_database_keeps_its_rows_and_gains_the_sync_tables() -> Result<(), TestError> {
    let directory = TestDir::new()?;
    let path = directory.file("v2.db");
    let owner = party(1)?;
    let started = meetup(&owner.id, 2);
    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(&key(1).pragma())?;
        crate::migrations::migrate_to(&connection, 2)?;
        connection.execute(
            "INSERT INTO party (party_id, mls_group_id, name, my_leaf, created_at) \
             VALUES (?1, ?2, 'Party', 1, 1000)",
            params![owner.id.as_bytes(), owner.group.as_bytes()],
        )?;
        connection.execute(
            "INSERT INTO meetup (group_id, party_id, spot_id, started_at, gk_sk) \
             VALUES (?1, ?2, ?3, 500, zeroblob(32))",
            params![
                started.group.as_bytes(),
                owner.id.as_bytes(),
                started.spot.as_bytes()
            ],
        )?;
    }
    let database = Database::open(&path, key(1))?;
    let session = database.session()?;
    assert_eq!(session.parties().list()?.len(), 1);
    push(&session, Some(started.group), 700, 1)?;
    push(&session, None, 700, 2)?;
    // The trigger covers a Meetup that existed before the migration.
    session.meetups().end(&started.group)?;
    assert_eq!(drain(&session)?, [2]);
    Ok(())
}

#[test]
fn the_default_capacity_is_thirty_two() {
    assert_eq!(QueueCapacity::default().get(), 32);
}

#[test]
fn a_queued_request_prints_no_bytes() {
    assert_eq!(format!("{:?}", bytes(9)), "RequestBytes(redacted)");
}
