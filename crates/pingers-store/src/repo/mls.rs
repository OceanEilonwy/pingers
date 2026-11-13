// SPDX-License-Identifier: AGPL-3.0-only
//! `mls_kv`: the pingers-mls storage trait.

use pingers_mls::{MlsStorage, StorageError};
use rusqlite::OptionalExtension as _;
use rusqlite::types::ValueRef;
use zeroize::Zeroizing;

use crate::{Session, StoreError};

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
        .map_err(|error| storage_error(&error))
    }

    fn write(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO mls_kv (key, value) VALUES (?1, ?2)",
                [key, value],
            )?;
            Ok(())
        })
        .map_err(|error| storage_error(&error))
    }

    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        self.with(|connection| {
            connection.execute("DELETE FROM mls_kv WHERE key = ?1", [key])?;
            Ok(())
        })
        .map_err(|error| storage_error(&error))
    }
}

/// The trait's error kinds carry no sources (pingers-mls's decision), so detail is lost here
/// by design: corrupt stored bytes are `Corrupt`, everything else `Unavailable`.
fn storage_error(error: &StoreError) -> StorageError {
    match error {
        StoreError::Corrupt { .. } => StorageError::Corrupt,
        StoreError::WrongKey
        | StoreError::Busy
        | StoreError::Lock(_)
        | StoreError::Database(_)
        | StoreError::NewerSchema { .. }
        | StoreError::OutOfRange { .. }
        | StoreError::NotFound { .. }
        | StoreError::InvalidText { .. }
        | StoreError::Poisoned => StorageError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::{Fixture, TestError};

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
    fn errors_map_to_trait_kinds() {
        assert_eq!(
            storage_error(&StoreError::Corrupt { field: "x" }),
            StorageError::Corrupt
        );
        assert_eq!(storage_error(&StoreError::Busy), StorageError::Unavailable);
        assert_eq!(
            storage_error(&StoreError::Poisoned),
            StorageError::Unavailable
        );
    }
}
