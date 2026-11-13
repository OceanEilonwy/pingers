// SPDX-License-Identifier: AGPL-3.0-only
//! The cross-process advisory lock (design B.7: "one cross-process file lock around the
//! core's entry points").

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use crate::StoreError;

/// An exclusive advisory lock on `<database>-lock`, held until dropped.
///
/// `File::lock` is `flock(2)` on Unix: locks taken through separate opens conflict even within
/// one process, so two sessions in two threads serialise exactly like the app and a background
/// service in two processes. The lock is released when the file is closed, including when the
/// process dies, so a crash never leaves the store locked.
#[derive(Debug)]
pub(crate) struct ProcessLock {
    _file: File,
}

impl ProcessLock {
    /// The lock file beside a database file.
    pub(crate) fn path_for(database: &Path) -> PathBuf {
        let mut name = database.as_os_str().to_owned();
        name.push("-lock");
        PathBuf::from(name)
    }

    /// Waits for the lock.
    pub(crate) fn acquire(path: &Path) -> Result<Self, StoreError> {
        let file = open(path)?;
        file.lock().map_err(StoreError::Lock)?;
        Ok(Self { _file: file })
    }

    /// Takes the lock if it is free, or returns [`StoreError::Busy`].
    pub(crate) fn try_acquire(path: &Path) -> Result<Self, StoreError> {
        let file = open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(StoreError::Busy),
            Err(TryLockError::Error(error)) => Err(StoreError::Lock(error)),
        }
    }
}

fn open(path: &Path) -> Result<File, StoreError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(StoreError::Lock)
}
