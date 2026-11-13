// SPDX-License-Identifier: AGPL-3.0-only
//! A [`DeliveryStore`] in one SQLite file (design C.1: `grp`, `grp_msg`, `grp_sub`,
//! `grp_state`). Subscriptions are kept in their wire encoding; bodies are opaque blobs.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use pingers_proto::{
    ContentType, Epoch, GroupAuthKey, GroupId, GroupLogEntry, GroupState, MlsMessageBytes,
    SealedSnapshot, SequenceNumber, Subscription, SubscriptionId, WireDecode as _, WireEncode as _,
};
use pingers_time::UnixSeconds;
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};

use crate::DeliveryError;
use crate::store::{
    CommitWrite, DeliveryStore, GroupHead, NewCommit, NewEntry, NewGroup, PAGE_ENTRIES,
    StoredSubscription,
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS grp (
    group_id        BLOB PRIMARY KEY,
    auth_pk         BLOB NOT NULL,
    head_epoch      INTEGER NOT NULL,
    idle_expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS grp_idle ON grp (idle_expires_at);
CREATE TABLE IF NOT EXISTS grp_msg (
    group_id     BLOB NOT NULL,
    seq          INTEGER NOT NULL,
    epoch        INTEGER NOT NULL,
    content_type INTEGER NOT NULL,
    body         BLOB NOT NULL,
    PRIMARY KEY (group_id, seq)
) STRICT;
CREATE TABLE IF NOT EXISTS grp_sub (
    group_id     BLOB NOT NULL,
    sub_id       BLOB NOT NULL,
    subscription BLOB NOT NULL,
    expires_at   INTEGER NOT NULL,
    PRIMARY KEY (group_id, sub_id)
) STRICT;
CREATE INDEX IF NOT EXISTS grp_sub_expiry ON grp_sub (expires_at);
CREATE TABLE IF NOT EXISTS grp_state (
    group_id BLOB PRIMARY KEY,
    epoch    INTEGER NOT NULL,
    blob     BLOB NOT NULL
) STRICT;
";

/// Group logs, subscriptions and snapshots in SQLite. One connection behind a lock; every
/// trait method is one immediate transaction.
#[derive(Debug)]
pub struct SqliteDeliveryStore {
    connection: Mutex<Connection>,
}

impl SqliteDeliveryStore {
    /// Opens or creates the database at `path` and its tables.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::Sqlite`] if it cannot be opened or the schema cannot be created.
    pub fn open(path: &Path) -> Result<Self, DeliveryError> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        Self::with_connection(connection)
    }

    /// A database that lives only as long as the store.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::Sqlite`] if the schema cannot be created.
    pub fn open_in_memory() -> Result<Self, DeliveryError> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(connection: Connection) -> Result<Self, DeliveryError> {
        connection.execute_batch(SCHEMA)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, DeliveryError> {
        self.connection.lock().map_err(|_| DeliveryError::Poisoned)
    }

    /// Runs `action` in an immediate transaction and commits it if it succeeds.
    fn transact<R>(
        &self,
        action: impl FnOnce(&Transaction<'_>) -> Result<R, DeliveryError>,
    ) -> Result<R, DeliveryError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = action(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }
}

fn head_of(transaction: &Transaction<'_>, group: &GroupId) -> Result<GroupHead, DeliveryError> {
    let row: Option<(u64, Vec<u8>)> = transaction
        .query_row(
            "SELECT head_epoch, auth_pk FROM grp WHERE group_id = ?1",
            [group.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (head_epoch, auth_key) = row.ok_or(DeliveryError::UnknownGroup)?;
    Ok(GroupHead {
        head_epoch: Epoch::new(head_epoch),
        auth_key: GroupAuthKey::from_bytes(
            auth_key
                .try_into()
                .map_err(|_| DeliveryError::CorruptField { field: "auth_pk" })?,
        ),
    })
}

fn set_idle(
    transaction: &Transaction<'_>,
    group: &GroupId,
    idle_expires_at: UnixSeconds,
) -> Result<(), DeliveryError> {
    let changed = transaction.execute(
        "UPDATE grp SET idle_expires_at = ?2 WHERE group_id = ?1",
        params![group.as_bytes().as_slice(), idle_expires_at.get()],
    )?;
    if changed == 0 {
        return Err(DeliveryError::UnknownGroup);
    }
    Ok(())
}

/// One log row to insert.
struct Row<'body> {
    epoch: Epoch,
    content_type: ContentType,
    body: &'body MlsMessageBytes,
}

fn push_entry(
    transaction: &Transaction<'_>,
    group: &GroupId,
    row: &Row<'_>,
) -> Result<SequenceNumber, DeliveryError> {
    let last: Option<u64> = transaction.query_row(
        "SELECT MAX(seq) FROM grp_msg WHERE group_id = ?1",
        [group.as_bytes().as_slice()],
        |row| row.get(0),
    )?;
    let sequence = match last {
        Some(last) => SequenceNumber::new(last)
            .checked_next()
            .ok_or(DeliveryError::CounterOverflow)?,
        None => SequenceNumber::new(1),
    };
    transaction.execute(
        "INSERT INTO grp_msg (group_id, seq, epoch, content_type, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            group.as_bytes().as_slice(),
            sequence.get(),
            row.epoch.get(),
            content_type_to_column(row.content_type),
            row.body.as_bytes()
        ],
    )?;
    Ok(sequence)
}

fn put_subscription_row(
    transaction: &Transaction<'_>,
    group: &GroupId,
    stored: &StoredSubscription,
) -> Result<(), DeliveryError> {
    let encoded = stored
        .subscription
        .encode()
        .map_err(DeliveryError::Corrupt)?;
    transaction.execute(
        "INSERT OR REPLACE INTO grp_sub (group_id, sub_id, subscription, expires_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            group.as_bytes().as_slice(),
            stored.subscription.id.as_bytes().as_slice(),
            encoded,
            stored.expires_at.get()
        ],
    )?;
    Ok(())
}

fn put_state(
    transaction: &Transaction<'_>,
    group: &GroupId,
    epoch: Epoch,
    state: &SealedSnapshot,
) -> Result<(), DeliveryError> {
    transaction.execute(
        "INSERT OR REPLACE INTO grp_state (group_id, epoch, blob) VALUES (?1, ?2, ?3)",
        params![group.as_bytes().as_slice(), epoch.get(), state.as_bytes()],
    )?;
    Ok(())
}

const fn content_type_to_column(content_type: ContentType) -> u8 {
    match content_type {
        ContentType::Application => 1,
        ContentType::Proposal => 2,
        ContentType::Commit => 3,
    }
}

const fn content_type_from_column(value: u8) -> Result<ContentType, DeliveryError> {
    match value {
        1 => Ok(ContentType::Application),
        2 => Ok(ContentType::Proposal),
        3 => Ok(ContentType::Commit),
        _ => Err(DeliveryError::CorruptField {
            field: "content_type",
        }),
    }
}

fn group_id_from_column(bytes: Vec<u8>) -> Result<GroupId, DeliveryError> {
    Ok(GroupId::from_bytes(bytes.try_into().map_err(|_| {
        DeliveryError::CorruptField { field: "group_id" }
    })?))
}

impl DeliveryStore for SqliteDeliveryStore {
    fn create_group(&self, group: NewGroup) -> Result<(), DeliveryError> {
        self.transact(|transaction| {
            let inserted = transaction.execute(
                "INSERT OR IGNORE INTO grp (group_id, auth_pk, head_epoch, idle_expires_at)
                 VALUES (?1, ?2, 0, ?3)",
                params![
                    group.id.as_bytes().as_slice(),
                    group.auth_key.as_bytes().as_slice(),
                    group.idle_expires_at.get()
                ],
            )?;
            if inserted == 0 {
                return Err(DeliveryError::GroupExists);
            }
            if let Some(state) = &group.state {
                put_state(transaction, &group.id, Epoch::new(0), state)?;
            }
            for stored in &group.subscriptions {
                put_subscription_row(transaction, &group.id, stored)?;
            }
            Ok(())
        })
    }

    fn head(&self, group: &GroupId) -> Result<GroupHead, DeliveryError> {
        self.transact(|transaction| head_of(transaction, group))
    }

    fn touch(&self, group: &GroupId, idle_expires_at: UnixSeconds) -> Result<(), DeliveryError> {
        self.transact(|transaction| set_idle(transaction, group, idle_expires_at))
    }

    fn append(
        &self,
        group: &GroupId,
        entry: NewEntry,
        idle_expires_at: UnixSeconds,
    ) -> Result<SequenceNumber, DeliveryError> {
        self.transact(|transaction| {
            set_idle(transaction, group, idle_expires_at)?;
            push_entry(
                transaction,
                group,
                &Row {
                    epoch: entry.epoch,
                    content_type: ContentType::Application,
                    body: &entry.body,
                },
            )
        })
    }

    fn append_commit(
        &self,
        group: &GroupId,
        commit: NewCommit,
        idle_expires_at: UnixSeconds,
    ) -> Result<CommitWrite, DeliveryError> {
        self.transact(|transaction| {
            let head = head_of(transaction, group)?;
            if head.head_epoch != commit.epoch {
                return Ok(CommitWrite::EpochTaken {
                    head_epoch: head.head_epoch,
                });
            }
            let sequence = push_entry(
                transaction,
                group,
                &Row {
                    epoch: commit.epoch,
                    content_type: ContentType::Commit,
                    body: &commit.commit,
                },
            )?;
            transaction.execute(
                "UPDATE grp SET head_epoch = ?2, auth_pk = ?3, idle_expires_at = ?4
                 WHERE group_id = ?1",
                params![
                    group.as_bytes().as_slice(),
                    commit.next_epoch.get(),
                    commit.next_auth_key.as_bytes().as_slice(),
                    idle_expires_at.get()
                ],
            )?;
            if let Some(state) = &commit.state {
                put_state(transaction, group, commit.next_epoch, state)?;
            }
            for removed in &commit.subscriptions_removed {
                transaction.execute(
                    "DELETE FROM grp_sub WHERE group_id = ?1 AND sub_id = ?2",
                    params![group.as_bytes().as_slice(), removed.as_bytes().as_slice()],
                )?;
            }
            for added in &commit.subscriptions_added {
                put_subscription_row(transaction, group, added)?;
            }
            Ok(CommitWrite::Appended { sequence })
        })
    }

    fn entries_after(
        &self,
        group: &GroupId,
        after: SequenceNumber,
    ) -> Result<Vec<GroupLogEntry>, DeliveryError> {
        self.transact(|transaction| {
            head_of(transaction, group)?;
            let limit = u64::try_from(PAGE_ENTRIES).unwrap_or(u64::MAX);
            let mut statement = transaction.prepare(
                "SELECT seq, epoch, content_type, body FROM grp_msg
                 WHERE group_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
            )?;
            let rows = statement.query_map(
                params![group.as_bytes().as_slice(), after.get(), limit],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, u8>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )?;
            rows.map(|row| {
                let (sequence, epoch, content_type, body) = row?;
                Ok(GroupLogEntry {
                    sequence: SequenceNumber::new(sequence),
                    epoch: Epoch::new(epoch),
                    content_type: content_type_from_column(content_type)?,
                    body: MlsMessageBytes::from_bytes(body),
                })
            })
            .collect()
        })
    }

    fn state(&self, group: &GroupId) -> Result<Option<GroupState>, DeliveryError> {
        self.transact(|transaction| {
            head_of(transaction, group)?;
            let row: Option<(u64, Vec<u8>)> = transaction
                .query_row(
                    "SELECT epoch, blob FROM grp_state WHERE group_id = ?1",
                    [group.as_bytes().as_slice()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            Ok(row.map(|(epoch, blob)| GroupState {
                epoch: Epoch::new(epoch),
                state: SealedSnapshot::from_bytes(blob),
            }))
        })
    }

    fn put_subscription(
        &self,
        group: &GroupId,
        subscription: StoredSubscription,
        idle_expires_at: UnixSeconds,
    ) -> Result<(), DeliveryError> {
        self.transact(|transaction| {
            set_idle(transaction, group, idle_expires_at)?;
            put_subscription_row(transaction, group, &subscription)
        })
    }

    fn remove_subscription(
        &self,
        group: &GroupId,
        subscription: &SubscriptionId,
    ) -> Result<(), DeliveryError> {
        self.transact(|transaction| {
            head_of(transaction, group)?;
            transaction.execute(
                "DELETE FROM grp_sub WHERE group_id = ?1 AND sub_id = ?2",
                params![
                    group.as_bytes().as_slice(),
                    subscription.as_bytes().as_slice()
                ],
            )?;
            Ok(())
        })
    }

    fn live_subscriptions(
        &self,
        group: &GroupId,
        now: UnixSeconds,
    ) -> Result<Vec<StoredSubscription>, DeliveryError> {
        self.transact(|transaction| {
            head_of(transaction, group)?;
            let mut statement = transaction.prepare(
                "SELECT subscription, expires_at FROM grp_sub
                 WHERE group_id = ?1 AND expires_at > ?2 ORDER BY sub_id",
            )?;
            let rows = statement
                .query_map(params![group.as_bytes().as_slice(), now.get()], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, u64>(1)?))
                })?;
            rows.map(|row| {
                let (encoded, expires_at) = row?;
                Ok(StoredSubscription {
                    subscription: Subscription::decode(&encoded).map_err(DeliveryError::Corrupt)?,
                    expires_at: UnixSeconds::new(expires_at),
                })
            })
            .collect()
        })
    }

    fn delete_group(&self, group: &GroupId) -> Result<(), DeliveryError> {
        self.transact(|transaction| {
            let id = group.as_bytes().as_slice();
            let deleted = transaction.execute("DELETE FROM grp WHERE group_id = ?1", [id])?;
            if deleted == 0 {
                return Err(DeliveryError::UnknownGroup);
            }
            delete_children(transaction, id)
        })
    }

    fn expire(&self, now: UnixSeconds) -> Result<Vec<GroupId>, DeliveryError> {
        self.transact(|transaction| {
            let expired: Vec<Vec<u8>> = {
                let mut statement = transaction.prepare(
                    "SELECT group_id FROM grp WHERE idle_expires_at <= ?1 ORDER BY group_id",
                )?;
                let rows = statement.query_map([now.get()], |row| row.get(0))?;
                rows.collect::<Result<_, _>>()?
            };
            for id in &expired {
                transaction.execute("DELETE FROM grp WHERE group_id = ?1", [id.as_slice()])?;
                delete_children(transaction, id)?;
            }
            transaction.execute("DELETE FROM grp_sub WHERE expires_at <= ?1", [now.get()])?;
            expired.into_iter().map(group_id_from_column).collect()
        })
    }
}

fn delete_children(transaction: &Transaction<'_>, id: &[u8]) -> Result<(), DeliveryError> {
    transaction.execute("DELETE FROM grp_msg WHERE group_id = ?1", [id])?;
    transaction.execute("DELETE FROM grp_sub WHERE group_id = ?1", [id])?;
    transaction.execute("DELETE FROM grp_state WHERE group_id = ?1", [id])?;
    Ok(())
}
