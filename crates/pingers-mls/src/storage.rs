// SPDX-License-Identifier: AGPL-3.0-only
//! The storage trait pingers-store implements for OpenMLS state.

use zeroize::Zeroizing;

/// Why the storage backend failed. A kind, not the backend's own error: the backend keeps its
/// detailed errors (and logs them, if at all) at its own API, and nothing here may carry stored
/// bytes or a backend's message. A missing key is not an error: [`MlsStorage::read`] returns
/// `Ok(None)`, and a missing group surfaces as `MlsError::NotAMember`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// The database could not be reached or is locked by another process; retrying later may
    /// succeed.
    #[error("storage unavailable")]
    Unavailable,
    /// The database was reached but the operation failed (I/O error, full disk, constraint
    /// violation, aborted transaction).
    #[error("storage operation failed")]
    Failed,
    /// Stored bytes could not be decoded.
    #[error("stored MLS state is corrupt")]
    Corrupt,
}

/// A key-value store for OpenMLS state (Party and Meetup groups alike, design C.2 `mls_*`).
///
/// pingers-mls adapts this to OpenMLS's `StorageProvider`, choosing the key layout; the store
/// only persists opaque bytes. Values contain secrets and come back zeroised on drop. Every row
/// of one group starts with that group's prefix ([`crate::group_storage_prefix`]), so
/// [`MlsStorage::delete_prefix`] wipes a group.
///
/// Methods take `&self` because OpenMLS calls its provider through shared references; the
/// implementation uses interior mutability and must make each call atomic. Callers group a
/// whole MLS operation into one store transaction at a higher layer (one cross-process lock
/// around the core's entry points, design B.7).
pub trait MlsStorage: Send + Sync {
    /// The value stored under `key`, if any.
    ///
    /// # Errors
    ///
    /// [`StorageError`] if the backend fails.
    fn read(&self, key: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>, StorageError>;

    /// Stores `value` under `key`, replacing any previous value.
    ///
    /// # Errors
    ///
    /// [`StorageError`] if the backend fails.
    fn write(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError>;

    /// Removes the value under `key`; removing a missing key is not an error.
    ///
    /// # Errors
    ///
    /// [`StorageError`] if the backend fails.
    fn delete(&self, key: &[u8]) -> Result<(), StorageError>;

    /// Removes every value whose key starts with `prefix` (byte-wise); matching nothing is not
    /// an error. `MlsContext::delete_group` calls it with the group's prefix, and the store may
    /// call it itself with [`crate::group_storage_prefix`] when a Meetup ends (design B.5).
    ///
    /// # Errors
    ///
    /// [`StorageError`] if the backend fails.
    fn delete_prefix(&self, prefix: &[u8]) -> Result<(), StorageError>;
}
