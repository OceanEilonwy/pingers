// SPDX-License-Identifier: AGPL-3.0-only
//! `mls_kv`: the pingers-mls storage trait.

use std::sync::PoisonError;

use pingers_mls::{MlsStorage, StorageError};
use rusqlite::OptionalExtension as _;
use rusqlite::types::ValueRef;
use zeroize::Zeroizing;

use crate::{Session, StoreError};

/// Deletes every `mls_kv` row whose key starts with `?1`. `substr` on a BLOB compares bytes,
/// so this matches exactly those keys, with no LIKE wildcards or collation to worry about.
pub(crate) const DELETE_PREFIX: &str = "DELETE FROM mls_kv WHERE substr(key, 1, length(?1)) = ?1";

impl Session {
    /// The `mls_kv` keys that start with `prefix`, in byte order. The `MlsStorage` trait cannot
    /// list keys; this exists so a crate that kept key-value records before the store had tables
    /// for them can find and move them. The keys are that crate's own byte layout.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the read fails.
    pub fn kv_keys_with_prefix(&self, prefix: &[u8]) -> Result<Vec<Vec<u8>>, StoreError> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT key FROM mls_kv WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key",
            )?;
            let keys = statement.query_map([prefix], |row| row.get(0))?;
            Ok(keys.collect::<Result<_, _>>()?)
        })
    }

    /// The store error behind the first [`StorageError`] this session's [`MlsStorage`] returned
    /// since the last call, if any.
    ///
    /// The trait's error kinds carry no source, so an MLS operation that reaches a broken
    /// database fails with `MlsError::Storage`, while a repository call on the same database
    /// fails with the [`StoreError`] itself. A caller that stops on a storage failure takes the
    /// original here, so it reports the same cause whichever path reached the database first.
    pub fn take_mls_failure(&self) -> Option<StoreError> {
        self.mls_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Keeps `error` if it is the first since [`Session::take_mls_failure`], and returns its
    /// trait kind.
    fn mls_failed(&self, error: StoreError) -> StorageError {
        let kind = storage_error(&error);
        let mut first = self
            .mls_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if first.is_none() {
            *first = Some(error);
        }
        kind
    }
}

/// OpenMLS state as opaque bytes (design C.2 `mls_*`). pingers-mls chooses the key layout.
///
/// Each call is atomic. Inside [`Session::transaction`], MLS writes commit or roll back
/// together with the repository writes of the same operation (for example a commit and the
/// roster change it implies).
impl MlsStorage for Session {
    fn read(&self, key: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>, StorageError> {
        self.with(|connection| {
            connection
                .query_row("SELECT value FROM mls_kv WHERE key = ?1", [key], |row| {
                    // Copied straight from SQLite's buffer into zeroed-on-drop memory.
                    Ok(match row.get_ref(0)? {
                        ValueRef::Blob(bytes) => Ok(Zeroizing::new(bytes.to_vec())),
                        ValueRef::Null
                        | ValueRef::Integer(_)
                        | ValueRef::Real(_)
                        | ValueRef::Text(_) => Err(StoreError::Corrupt {
                            field: "mls_kv.value",
                        }),
                    })
                })
                .optional()?
                .transpose()
        })
        .map_err(|error| self.mls_failed(error))
    }

    fn write(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO mls_kv (key, value) VALUES (?1, ?2)",
                [key, value],
            )?;
            Ok(())
        })
        .map_err(|error| self.mls_failed(error))
    }

    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        self.with(|connection| {
            connection.execute("DELETE FROM mls_kv WHERE key = ?1", [key])?;
            Ok(())
        })
        .map_err(|error| self.mls_failed(error))
    }

    fn delete_prefix(&self, prefix: &[u8]) -> Result<(), StorageError> {
        self.with(|connection| {
            connection.execute(DELETE_PREFIX, [prefix])?;
            Ok(())
        })
        .map_err(|error| self.mls_failed(error))
    }
}

/// The trait's error kinds carry no sources (pingers-mls's decision), so detail is lost here
/// by design: corrupt stored bytes are `Corrupt`; a database that can't be used right now
/// (locked, busy, wrong key, newer schema, poisoned lock) is `Unavailable`; a database that was
/// reached but refused the operation is `Failed`.
fn storage_error(error: &StoreError) -> StorageError {
    match error {
        StoreError::Corrupt { .. } => StorageError::Corrupt,
        StoreError::Database(database) if is_busy(database) => StorageError::Unavailable,
        StoreError::Database(_)
        | StoreError::OutOfRange { .. }
        | StoreError::NotFound { .. }
        | StoreError::InvalidText { .. }
        | StoreError::UnknownServer
        | StoreError::ServerMismatch
        | StoreError::Internal => StorageError::Failed,
        StoreError::WrongKey
        | StoreError::Busy
        | StoreError::Lock(_)
        | StoreError::NewerSchema { .. }
        | StoreError::Poisoned => StorageError::Unavailable,
    }
}

/// `SQLITE_BUSY` or `SQLITE_LOCKED`: another connection holds the database; retry later.
fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::{Fixture, TestError};

    #[test]
    fn keys_are_listed_by_prefix_in_byte_order() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let storage: &dyn MlsStorage = &fixture.session;
        for key in [&b"a/2"[..], b"a/1", b"a", b"b/1", b"a%"] {
            storage.write(key, b"x")?;
        }
        assert_eq!(
            fixture.session.kv_keys_with_prefix(b"a/")?,
            vec![b"a/1".to_vec(), b"a/2".to_vec()]
        );
        assert!(fixture.session.kv_keys_with_prefix(b"c")?.is_empty());
        Ok(())
    }

    #[test]
    fn read_write_delete_round_trip() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let storage: &dyn MlsStorage = &fixture.session;
        assert!(storage.read(b"group/1")?.is_none());
        storage.write(b"group/1", b"state one")?;
        storage.write(b"group/2", b"state two")?;
        assert_eq!(
            storage.read(b"group/1")?.as_deref().map(Vec::as_slice),
            Some(&b"state one"[..])
        );
        storage.write(b"group/1", b"replaced")?;
        assert_eq!(
            storage.read(b"group/1")?.as_deref().map(Vec::as_slice),
            Some(&b"replaced"[..])
        );
        storage.delete(b"group/1")?;
        assert!(storage.read(b"group/1")?.is_none());
        // Deleting a missing key is not an error, and other keys are untouched.
        storage.delete(b"group/1")?;
        assert!(storage.read(b"group/2")?.is_some());
        Ok(())
    }

    #[test]
    fn busy_and_locked_databases_are_unavailable_not_failed() {
        let sqlite = |code| {
            StoreError::Database(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ))
        };
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            assert_eq!(storage_error(&sqlite(code)), StorageError::Unavailable);
        }
        assert_eq!(
            storage_error(&sqlite(rusqlite::ffi::SQLITE_CONSTRAINT)),
            StorageError::Failed
        );
    }

    #[test]
    fn empty_values_are_stored_not_absent() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        fixture.session.write(b"key", b"")?;
        assert_eq!(
            fixture.session.read(b"key")?.map(|value| value.len()),
            Some(0)
        );
        Ok(())
    }

    #[test]
    fn mls_writes_join_the_surrounding_transaction() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let result: Result<(), StoreError> = fixture.session.transaction(|session| {
            session
                .write(b"epoch", b"5")
                .map_err(|_| StoreError::Busy)?;
            Err(StoreError::Busy)
        });
        assert!(result.is_err());
        assert!(fixture.session.read(b"epoch")?.is_none());
        Ok(())
    }

    #[test]
    fn the_first_failure_behind_a_trait_error_is_kept_until_taken() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let storage: &dyn MlsStorage = &fixture.session;
        storage.write(b"group/1", b"state")?;
        assert!(fixture.session.take_mls_failure().is_none());
        let sql = |statement: &'static str| {
            fixture
                .session
                .with(|connection| Ok(connection.execute_batch(statement)?))
        };
        let message = |failure: Option<StoreError>| match failure {
            Some(StoreError::Database(rusqlite::Error::SqliteFailure(_, Some(message)))) => message,
            _ => String::new(),
        };
        // Reads fail on a missing table, then writes on a constraint.
        sql("DROP TABLE mls_kv")?;
        assert_eq!(storage.read(b"group/1"), Err(StorageError::Failed));
        sql("CREATE TABLE mls_kv (key BLOB PRIMARY KEY, value BLOB CHECK (0))")?;
        assert_eq!(storage.write(b"group/1", b"x"), Err(StorageError::Failed));
        // The first failure is the one kept, and taking it clears it.
        assert!(message(fixture.session.take_mls_failure()).contains("no such table"));
        assert!(fixture.session.take_mls_failure().is_none());
        assert_eq!(storage.write(b"group/1", b"x"), Err(StorageError::Failed));
        assert!(message(fixture.session.take_mls_failure()).contains("CHECK constraint"));
        Ok(())
    }

    #[test]
    fn delete_prefix_removes_exactly_the_keys_under_it() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let storage: &dyn MlsStorage = &fixture.session;
        storage.write(b"group/1/epoch", b"a")?;
        storage.write(b"group/1/tree", b"b")?;
        storage.write(b"group/10/tree", b"c")?;
        storage.write(b"group/2/tree", b"d")?;
        // `%` and `_` are LIKE wildcards; a prefix containing them must still match literally.
        storage.write(b"g%/x", b"e")?;
        storage.write(b"gz/x", b"f")?;
        storage.delete_prefix(b"group/1/")?;
        assert!(storage.read(b"group/1/epoch")?.is_none());
        assert!(storage.read(b"group/1/tree")?.is_none());
        assert!(storage.read(b"group/10/tree")?.is_some());
        assert!(storage.read(b"group/2/tree")?.is_some());
        storage.delete_prefix(b"g%")?;
        assert!(storage.read(b"g%/x")?.is_none());
        assert!(storage.read(b"gz/x")?.is_some());
        // Matching nothing is not an error.
        storage.delete_prefix(b"nothing/")?;
        Ok(())
    }

    #[test]
    fn errors_map_to_trait_kinds() {
        assert_eq!(
            storage_error(&StoreError::Corrupt { field: "x" }),
            StorageError::Corrupt
        );
        assert_eq!(
            storage_error(&StoreError::OutOfRange { field: "x" }),
            StorageError::Failed
        );
        assert_eq!(storage_error(&StoreError::Busy), StorageError::Unavailable);
        assert_eq!(
            storage_error(&StoreError::Poisoned),
            StorageError::Unavailable
        );
    }
}
