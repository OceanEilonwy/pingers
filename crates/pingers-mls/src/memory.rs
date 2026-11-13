// SPDX-License-Identifier: AGPL-3.0-only
//! An in-memory [`MlsStorage`] for tests (feature `test-util`).

use std::collections::BTreeMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::{MlsStorage, StorageError};

/// A [`MlsStorage`] in a map, for tests here and in other crates. Values are zeroised when
/// replaced, removed or dropped.
///
/// [`MemoryStorage::fail_with`] makes every later call fail, to test how callers handle a
/// broken backend.
#[derive(Default)]
pub struct MemoryStorage {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    values: BTreeMap<Vec<u8>, Zeroizing<Vec<u8>>>,
    failure: Option<StorageError>,
}

impl MemoryStorage {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of stored entries (0 if the lock is poisoned).
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.lock().map_or(0, |state| state.values.len())
    }

    /// Whether nothing is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether any stored key contains `needle`, for tests that check a group left no trace.
    #[must_use]
    pub fn contains_key_fragment(&self, needle: &[u8]) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.values.keys().any(|key| {
                !needle.is_empty() && key.windows(needle.len()).any(|part| part == needle)
            })
        })
    }

    /// The stored keys, lossily as text, to show what a test left behind.
    #[cfg(test)]
    pub(crate) fn key_names(&self) -> Vec<String> {
        self.state.lock().map_or_else(
            |_| Vec::new(),
            |state| {
                state
                    .values
                    .keys()
                    .map(|key| String::from_utf8_lossy(key).into_owned())
                    .collect()
            },
        )
    }

    /// From now on every call fails with `failure`; `None` heals the store.
    pub fn fail_with(&self, failure: Option<StorageError>) {
        if let Ok(mut state) = self.state.lock() {
            state.failure = failure;
        }
    }

    fn with_state<T>(&self, operation: impl FnOnce(&mut State) -> T) -> Result<T, StorageError> {
        let mut state = self.state.lock().map_err(|_| StorageError::Unavailable)?;
        match state.failure {
            Some(failure) => Err(failure),
            None => Ok(operation(&mut state)),
        }
    }
}

impl MlsStorage for MemoryStorage {
    fn read(&self, key: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>, StorageError> {
        self.with_state(|state| state.values.get(key).cloned())
    }

    fn write(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.with_state(|state| {
            state
                .values
                .insert(key.to_vec(), Zeroizing::new(value.to_vec()));
        })
    }

    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        self.with_state(|state| {
            state.values.remove(key);
        })
    }

    fn delete_prefix(&self, prefix: &[u8]) -> Result<(), StorageError> {
        self.with_state(|state| state.values.retain(|key, _| !key.starts_with(prefix)))
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn stores_reads_and_deletes() -> Result<(), StorageError> {
        let storage = MemoryStorage::new();
        assert!(storage.is_empty());
        storage.write(b"key", b"value")?;
        assert_eq!(
            storage.read(b"key")?.as_deref().map(Vec::as_slice),
            Some(&b"value"[..])
        );
        assert_eq!(storage.len(), 1);
        assert!(!storage.is_empty());
        assert!(storage.contains_key_fragment(b"ke"));
        assert!(!storage.contains_key_fragment(b"other"));
        assert!(!storage.contains_key_fragment(b""));
        storage.delete(b"key")?;
        storage.delete(b"missing")?;
        assert_eq!(storage.read(b"key")?, None);
        assert!(storage.is_empty());
        Ok(())
    }

    #[test]
    fn delete_prefix_removes_exactly_the_keys_under_it() -> Result<(), StorageError> {
        let storage = MemoryStorage::new();
        for key in [
            &b"group/a/1"[..],
            b"group/a/2",
            b"group/ab",
            b"group/",
            b"other",
        ] {
            storage.write(key, b"value")?;
        }
        storage.delete_prefix(b"group/a/")?;
        assert_eq!(storage.key_names(), vec!["group/", "group/ab", "other"]);
        storage.delete_prefix(b"nothing")?;
        assert_eq!(storage.len(), 3);
        Ok(())
    }

    #[test]
    fn injected_failure_fails_every_call_until_healed() -> Result<(), StorageError> {
        let storage = MemoryStorage::new();
        storage.fail_with(Some(StorageError::Unavailable));
        assert_eq!(
            storage.write(b"key", b"value"),
            Err(StorageError::Unavailable)
        );
        assert_eq!(storage.read(b"key"), Err(StorageError::Unavailable));
        assert_eq!(storage.delete(b"key"), Err(StorageError::Unavailable));
        storage.fail_with(None);
        storage.write(b"key", b"value")?;
        Ok(())
    }
}
