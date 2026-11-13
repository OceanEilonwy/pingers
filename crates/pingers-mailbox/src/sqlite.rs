// SPDX-License-Identifier: AGPL-3.0-only
//! Mailboxes in one SQLite file (feature `sqlite`), so a self-hosted server keeps knocks and
//! Welcomes across restarts.

use std::path::Path;

use pingers_proto::{
    KeyConfigHash, MailboxId, MailboxMessage, Padded, PushHandle, RelayId, RelayRef, RelayUrl,
    SealedPush, SealedPushBlob, SequenceNumber, Urgency,
};
use pingers_time::UnixSeconds;
use rusqlite::{Connection, OptionalExtension as _, Row, params};

use crate::StorageError;
use crate::storage::{MailboxStorage, Removal, StoredMailbox};

/// The schema, version 1 (design C.1 `mailbox`, `mailbox_msg`). `secure_delete` overwrites
/// deleted rows, so a deleted mailbox is gone from the file too.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mailbox (
    mailbox_id     BLOB PRIMARY KEY NOT NULL,
    expires_at     INTEGER NOT NULL,
    last_seq       INTEGER NOT NULL,
    relay_id       INTEGER,
    relay_url      TEXT,
    relay_key_hash BLOB,
    push_blob      BLOB,
    urgency        INTEGER,
    handle         BLOB
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS mailbox_expiry ON mailbox (expires_at);
CREATE TABLE IF NOT EXISTS mailbox_msg (
    mailbox_id BLOB NOT NULL REFERENCES mailbox (mailbox_id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    body       BLOB NOT NULL,
    PRIMARY KEY (mailbox_id, seq)
) WITHOUT ROWID;
PRAGMA user_version = 1;
";

/// [`MailboxStorage`] in SQLite.
pub struct SqliteStorage {
    connection: Connection,
}

impl SqliteStorage {
    /// Opens or creates the database at `path`.
    ///
    /// # Errors
    ///
    /// [`StorageError::Sqlite`] if it cannot be opened or migrated.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        Self::prepare(Connection::open(path)?)
    }

    /// A database that lives only as long as the value, for tests.
    ///
    /// # Errors
    ///
    /// [`StorageError::Sqlite`] if it cannot be created.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(connection: Connection) -> Result<Self, StorageError> {
        connection.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON; PRAGMA journal_mode = WAL;",
        )?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self { connection })
    }
}

fn to_sql_integer(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_overflow| StorageError::Corrupt)
}

fn from_sql_integer(value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_negative| StorageError::Corrupt)
}

fn fixed<const N: usize>(bytes: &[u8]) -> Result<[u8; N], StorageError> {
    bytes.try_into().map_err(|_length| StorageError::Corrupt)
}

/// The push columns of a mailbox row, as stored.
struct PushColumns {
    relay_id: Option<i64>,
    relay_url: Option<String>,
    relay_key_hash: Option<Vec<u8>>,
    blob: Option<Vec<u8>>,
    urgency: Option<i64>,
    handle: Option<Vec<u8>>,
}

impl PushColumns {
    fn from_push(push: Option<&SealedPush>) -> Self {
        let Some(push) = push else {
            return Self {
                relay_id: None,
                relay_url: None,
                relay_key_hash: None,
                blob: None,
                urgency: None,
                handle: None,
            };
        };
        let (relay_id, relay_url, relay_key_hash) = match &push.relay {
            RelayRef::Listed(id) => (Some(i64::from(id.get())), None, None),
            RelayRef::Custom {
                url,
                key_config_hash,
            } => (
                None,
                Some(url.as_str().to_owned()),
                Some(key_config_hash.as_bytes().to_vec()),
            ),
        };
        Self {
            relay_id,
            relay_url,
            relay_key_hash,
            blob: Some(push.blob.as_bytes().to_vec()),
            urgency: Some(match push.urgency {
                Urgency::Alert => 0,
                Urgency::Quiet => 1,
            }),
            handle: Some(push.handle.as_bytes().to_vec()),
        }
    }

    fn into_push(self) -> Result<Option<SealedPush>, StorageError> {
        let (Some(blob), Some(urgency), Some(handle)) = (self.blob, self.urgency, self.handle)
        else {
            return Ok(None);
        };
        let relay = match (self.relay_id, self.relay_url, self.relay_key_hash) {
            (Some(id), None, None) => RelayRef::Listed(RelayId::new(
                u16::try_from(id).map_err(|_range| StorageError::Corrupt)?,
            )),
            (None, Some(url), Some(hash)) => RelayRef::Custom {
                url: RelayUrl::new(url).map_err(|_scheme| StorageError::Corrupt)?,
                key_config_hash: KeyConfigHash::from_bytes(fixed(&hash)?),
            },
            _ => return Err(StorageError::Corrupt),
        };
        Ok(Some(SealedPush {
            relay,
            blob: SealedPushBlob::from_bytes(fixed(&blob)?),
            urgency: match urgency {
                0 => Urgency::Alert,
                1 => Urgency::Quiet,
                _ => return Err(StorageError::Corrupt),
            },
            handle: PushHandle::from_bytes(fixed(&handle)?),
        }))
    }
}

fn mailbox_from_row(row: &Row<'_>) -> rusqlite::Result<(Vec<u8>, i64, PushColumns)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        PushColumns {
            relay_id: row.get(2)?,
            relay_url: row.get(3)?,
            relay_key_hash: row.get(4)?,
            blob: row.get(5)?,
            urgency: row.get(6)?,
            handle: row.get(7)?,
        },
    ))
}

impl MailboxStorage for SqliteStorage {
    fn insert_mailbox(&mut self, mailbox: &StoredMailbox) -> Result<(), StorageError> {
        let push = PushColumns::from_push(mailbox.push.as_ref());
        let inserted = self.connection.execute(
            "INSERT OR IGNORE INTO mailbox (mailbox_id, expires_at, last_seq, relay_id, relay_url,
                 relay_key_hash, push_blob, urgency, handle)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                mailbox.id.as_bytes().as_slice(),
                to_sql_integer(mailbox.expires_at.get())?,
                push.relay_id,
                push.relay_url,
                push.relay_key_hash,
                push.blob,
                push.urgency,
                push.handle,
            ],
        )?;
        if inserted == 0 {
            return Err(StorageError::Duplicate);
        }
        Ok(())
    }

    fn mailbox(&self, id: &MailboxId) -> Result<Option<StoredMailbox>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT mailbox_id, expires_at, relay_id, relay_url, relay_key_hash, push_blob,
                        urgency, handle
                 FROM mailbox WHERE mailbox_id = ?1",
                params![id.as_bytes().as_slice()],
                mailbox_from_row,
            )
            .optional()?;
        let Some((stored_id, expires_at, push)) = row else {
            return Ok(None);
        };
        Ok(Some(StoredMailbox {
            id: MailboxId::from_bytes(fixed(&stored_id)?),
            push: push.into_push()?,
            expires_at: UnixSeconds::new(from_sql_integer(expires_at)?),
        }))
    }

    fn delete_mailbox(&mut self, id: &MailboxId) -> Result<Removal, StorageError> {
        let deleted = self.connection.execute(
            "DELETE FROM mailbox WHERE mailbox_id = ?1",
            params![id.as_bytes().as_slice()],
        )?;
        Ok(if deleted == 0 {
            Removal::Absent
        } else {
            Removal::Removed
        })
    }

    fn clear_push(&mut self, id: &MailboxId) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE mailbox SET relay_id = NULL, relay_url = NULL, relay_key_hash = NULL,
                 push_blob = NULL, urgency = NULL, handle = NULL
             WHERE mailbox_id = ?1",
            params![id.as_bytes().as_slice()],
        )?;
        Ok(())
    }

    fn append_message(
        &mut self,
        id: &MailboxId,
        body: &Padded,
    ) -> Result<SequenceNumber, StorageError> {
        let transaction = self.connection.transaction()?;
        let last: Option<i64> = transaction
            .query_row(
                "SELECT last_seq FROM mailbox WHERE mailbox_id = ?1",
                params![id.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .optional()?;
        let last = last.ok_or(StorageError::NotFound)?;
        let sequence = SequenceNumber::new(from_sql_integer(last)?)
            .checked_next()
            .ok_or(StorageError::SequenceExhausted)?;
        let stored_sequence =
            i64::try_from(sequence.get()).map_err(|_overflow| StorageError::SequenceExhausted)?;
        transaction.execute(
            "UPDATE mailbox SET last_seq = ?2 WHERE mailbox_id = ?1",
            params![id.as_bytes().as_slice(), stored_sequence],
        )?;
        transaction.execute(
            "INSERT INTO mailbox_msg (mailbox_id, seq, body) VALUES (?1, ?2, ?3)",
            params![id.as_bytes().as_slice(), stored_sequence, body.as_bytes()],
        )?;
        transaction.commit()?;
        Ok(sequence)
    }

    fn message_count(&self, id: &MailboxId) -> Result<usize, StorageError> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM mailbox_msg WHERE mailbox_id = ?1",
            params![id.as_bytes().as_slice()],
            |row| row.get(0),
        )?;
        usize::try_from(count).map_err(|_negative| StorageError::Corrupt)
    }

    fn messages_after(
        &self,
        id: &MailboxId,
        after: Option<SequenceNumber>,
    ) -> Result<Vec<MailboxMessage>, StorageError> {
        let after = to_sql_integer(after.map_or(0, SequenceNumber::get))?;
        let mut statement = self.connection.prepare(
            "SELECT seq, body FROM mailbox_msg WHERE mailbox_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = statement.query_map(params![id.as_bytes().as_slice(), after], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        rows.map(|row| {
            let (sequence, body) = row?;
            Ok(MailboxMessage {
                sequence: SequenceNumber::new(from_sql_integer(sequence)?),
                body: Padded::from_wire(body).map_err(|_length| StorageError::Corrupt)?,
            })
        })
        .collect()
    }

    fn delete_expired(&mut self, now: UnixSeconds) -> Result<(), StorageError> {
        self.connection.execute(
            "DELETE FROM mailbox WHERE expires_at <= ?1",
            params![to_sql_integer(now.get())?],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::SizeClass;

    use super::*;
    use crate::storage::contract::{ContractError, check_all, mailbox};

    #[test]
    fn sqlite_storage_meets_the_contract() -> Result<(), ContractError> {
        check_all(SqliteStorage::open_in_memory)
    }

    #[test]
    fn mailboxes_survive_reopening_the_file() -> Result<(), ContractError> {
        let directory =
            std::env::temp_dir().join(format!("pingers-mailbox-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).map_err(|_io| StorageError::Corrupt)?;
        let path = directory.join("mailboxes.sqlite");
        let stored = mailbox(1, 100, None);
        {
            let mut storage = SqliteStorage::open(&path)?;
            storage.insert_mailbox(&stored)?;
            storage.append_message(&stored.id, &Padded::pad(b"knock", SizeClass::Bytes1024)?)?;
        }
        let mut storage = SqliteStorage::open(&path)?;
        assert_eq!(storage.mailbox(&stored.id)?, Some(stored.clone()));
        assert_eq!(
            storage.append_message(&stored.id, &Padded::pad(b"again", SizeClass::Bytes1024)?)?,
            SequenceNumber::new(2)
        );
        drop(storage);
        std::fs::remove_dir_all(&directory).map_err(|_io| StorageError::Corrupt)?;
        Ok(())
    }

    #[test]
    fn corrupt_push_columns_are_reported() -> Result<(), ContractError> {
        let mut storage = SqliteStorage::open_in_memory()?;
        let stored = mailbox(1, 100, None);
        storage.insert_mailbox(&stored)?;
        storage
            .connection
            .execute(
                "UPDATE mailbox SET push_blob = x'00', urgency = 0, handle = x'0000000000000000',
                 relay_id = 1",
                [],
            )
            .map_err(StorageError::from)?;
        assert!(matches!(
            storage.mailbox(&stored.id),
            Err(StorageError::Corrupt)
        ));
        Ok(())
    }
}
