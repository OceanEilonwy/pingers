// SPDX-License-Identifier: AGPL-3.0-only
//! Opening the database, sessions under the cross-process lock, and transactions.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::Connection;

use crate::lock::ProcessLock;
use crate::{DatabaseKey, StoreError, migrations};

/// An encrypted Pingers database on disk: where it is and the key that opens it.
///
/// Holds no connection and no lock. Every entry point of the core (a wake, a push, a user
/// action) takes a [`Session`], which holds the cross-process lock for its whole lifetime, so
/// the app and its background services never interleave MLS state transitions (design B.7).
pub struct Database {
    path: PathBuf,
    lock_path: PathBuf,
    key: DatabaseKey,
}

impl Database {
    /// Opens (creating if absent) the database at `path` with `key`, and migrates it to the
    /// current schema under the lock.
    ///
    /// # Errors
    ///
    /// [`StoreError::WrongKey`] if `key` does not open an existing file;
    /// [`StoreError::NewerSchema`] if a newer app wrote it; [`StoreError::Lock`] or
    /// [`StoreError::Database`] on I/O failures.
    pub fn open(path: &Path, key: DatabaseKey) -> Result<Self, StoreError> {
        let database = Self {
            path: path.to_path_buf(),
            lock_path: ProcessLock::path_for(path),
            key,
        };
        let session = database.session()?;
        session.with(|connection| {
            connection.pragma_update(None, "journal_mode", "WAL")?;
            migrations::migrate(connection)
        })?;
        Ok(database)
    }

    /// Takes the store's lock, waiting for any other session (in this or another process) to
    /// finish, and connects.
    ///
    /// # Errors
    ///
    /// [`StoreError::Lock`] if the lock file fails; [`StoreError::WrongKey`] if the file was
    /// replaced by one this key does not open; [`StoreError::Database`] otherwise.
    pub fn session(&self) -> Result<Session, StoreError> {
        Session::connect(
            ProcessLock::acquire(&self.lock_path)?,
            &self.path,
            &self.key,
        )
    }

    /// Like [`Database::session`] but does not wait: for a background handler that should
    /// give up rather than queue behind the app.
    ///
    /// # Errors
    ///
    /// [`StoreError::Busy`] if another session holds the lock; otherwise as
    /// [`Database::session`].
    pub fn try_session(&self) -> Result<Session, StoreError> {
        Session::connect(
            ProcessLock::try_acquire(&self.lock_path)?,
            &self.path,
            &self.key,
        )
    }
}

/// Exclusive access to the store: its own SQLCipher connection under the cross-process lock.
///
/// Repositories are reached through accessor methods (`session.parties()`, ...). Each
/// repository call is atomic on its own; group calls that must commit together with
/// [`Session::transaction`]. Dropping the session closes the connection and then releases the
/// lock (field order matters).
pub struct Session {
    connection: Mutex<Connection>,
    _lock: ProcessLock,
}

/// Held while a connection is opened and keyed, so no connection is keyed before SQLCipher
/// has finished initialising.
///
/// SQLite marks itself initialised and only then runs SQLCipher's initialisation, outside its
/// own mutex. A second thread opening a database in that window gets a connection at once and
/// its `PRAGMA key` fails ("An error occurred with PRAGMA key"), because SQLCipher is not ready
/// yet. The first open in a process runs the whole initialisation inside `Connection::open`, so
/// opening under this lock makes every other open wait for it. In the app this is a push wake
/// and the UI opening the store at the same moment when the process starts. The store is the
/// only SQLite user in the client process, so guarding its opens is enough. Keying is cheap
/// (a raw key, no KDF), so holding the lock for it costs nothing measurable.
static OPENING: Mutex<()> = Mutex::new(());

/// Opens `path` and keys the connection, under [`OPENING`].
pub(crate) fn open_keyed(path: &Path, key: &DatabaseKey) -> Result<Connection, StoreError> {
    // The lock guards no data, so a panic while it was held leaves nothing inconsistent.
    let _opening = OPENING.lock().unwrap_or_else(PoisonError::into_inner);
    // Read-write, create if absent; no SQLite mutex because the session's own Mutex
    // already serialises use of the connection.
    let connection = Connection::open(path)?;
    // SQLCipher logs decryption failures to stderr (logcat on Android) by default; the
    // app logs nothing (style guide §9).
    connection.execute_batch("PRAGMA cipher_log_level = NONE;")?;
    connection.execute_batch(&key.duplicate().pragma())?;
    Ok(connection)
}

impl Session {
    fn connect(lock: ProcessLock, path: &Path, key: &DatabaseKey) -> Result<Self, StoreError> {
        let connection = open_keyed(path, key)?;
        // The first read fails with "not a database" if the key is wrong.
        connection.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
        // Freed pages are overwritten with zeros, so deleted keys and messages do not linger
        // in free pages of the file (design A.4: past Meetups are not exposed).
        connection.pragma_update(None, "secure_delete", "ON")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        // Temporary tables and indices stay in memory, never in plaintext temp files.
        connection.pragma_update(None, "temp_store", "MEMORY")?;
        // A commit is on disk before it returns: the app may be killed at any instant.
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self {
            connection: Mutex::new(connection),
            _lock: lock,
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection.lock().map_err(|_| StoreError::Poisoned)
    }

    /// Runs `operation` on the connection.
    pub(crate) fn with<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let connection = self.connection()?;
        operation(&connection)
    }

    /// Runs `body` as one atomic unit: everything it writes through this session (repositories
    /// and the MLS storage alike) commits together if it returns `Ok`, and is rolled back if it
    /// returns `Err` or panics. Transactions nest (savepoints).
    ///
    /// # Errors
    ///
    /// Whatever `body` returns, or [`StoreError`] (converted into `E`) if the transaction
    /// cannot be started or committed.
    pub fn transaction<T, E: From<StoreError>>(
        &self,
        body: impl FnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, E> {
        self.with(|connection| Ok(connection.execute_batch("SAVEPOINT pingers")?))?;
        let mut savepoint = Savepoint {
            session: self,
            open: true,
        };
        let outcome = body(self);
        if outcome.is_ok() {
            self.with(|connection| Ok(connection.execute_batch("RELEASE pingers")?))?;
            savepoint.open = false;
        }
        drop(savepoint);
        outcome
    }

    /// Moves everything in the write-ahead log into the database file and truncates the log,
    /// so rows just deleted (with `secure_delete`, overwritten) no longer exist in the log
    /// either. Called after a Meetup ends.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the checkpoint fails.
    pub fn checkpoint(&self) -> Result<(), StoreError> {
        self.with(|connection| {
            connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            Ok(())
        })
    }
}

/// Rolls an open savepoint back when dropped: on an `Err` from the body, and on a panic.
struct Savepoint<'session> {
    session: &'session Session,
    open: bool,
}

impl Drop for Savepoint<'_> {
    fn drop(&mut self) {
        if self.open {
            // Nothing to report to: if the rollback itself fails the connection is unusable,
            // and closing it (dropping the session) rolls the transaction back anyway.
            let _ = self.session.with(|connection| {
                Ok(connection.execute_batch("ROLLBACK TO pingers; RELEASE pingers")?)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::sync::mpsc;
    use std::thread;

    use super::*;
    use crate::test_support::{TestDir, TestError, key};

    fn count(session: &Session) -> Result<i64, StoreError> {
        session.with(|connection| {
            Ok(connection.query_row("SELECT count(*) FROM board_seen", [], |row| row.get(0))?)
        })
    }

    fn insert(session: &Session, b0: i64) -> Result<(), StoreError> {
        session.with(|connection| {
            connection.execute(
                "INSERT INTO board_seen (b0, segment_sha256) VALUES (?1, zeroblob(32))",
                [b0],
            )?;
            Ok(())
        })
    }

    #[test]
    fn wrong_key_fails_to_open() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("store.db");
        drop(Database::open(&path, key(1))?);
        assert!(matches!(
            Database::open(&path, key(2)),
            Err(StoreError::WrongKey)
        ));
        Ok(())
    }

    #[test]
    fn file_is_not_readable_without_the_key() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("store.db");
        drop(Database::open(&path, key(1))?);
        let plain = Connection::open(&path)?;
        let result = plain.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        });
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn sessions_enable_secure_delete_and_foreign_keys() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let database = Database::open(&directory.file("store.db"), key(1))?;
        let session = database.session()?;
        let pragmas = session.with(|connection| {
            let read = |name: &str| -> Result<i64, StoreError> {
                Ok(connection.pragma_query_value(None, name, |row| row.get(0))?)
            };
            Ok([
                read("secure_delete")?,
                read("foreign_keys")?,
                read("temp_store")?,
                read("synchronous")?,
            ])
        })?;
        // secure_delete on, foreign_keys on, temp_store MEMORY (2), synchronous FULL (2).
        assert_eq!(pragmas, [1, 1, 2, 2]);
        let journal: String = session.with(|connection| {
            Ok(connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?)
        })?;
        assert_eq!(journal, "wal");
        Ok(())
    }

    #[test]
    fn try_session_is_busy_while_another_session_is_open() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let database = Database::open(&directory.file("store.db"), key(1))?;
        let held = database.session()?;
        assert!(matches!(database.try_session(), Err(StoreError::Busy)));
        drop(held);
        assert!(database.try_session().is_ok());
        Ok(())
    }

    #[test]
    fn opening_waits_for_the_lock() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("store.db");
        drop(Database::open(&path, key(1))?);
        // A second handle to the same file, as a background service would have.
        let database = Database::open(&path, key(1))?;
        let held = database.session()?;
        let (opened_sender, opened) = mpsc::channel();
        let waiter = {
            let path = path.clone();
            thread::spawn(move || -> Result<(), StoreError> {
                let other = Database::open(&path, key(1))?;
                let session = other.session()?;
                let seen = count(&session)?;
                opened_sender.send(seen).ok();
                Ok(())
            })
        };
        // Written while the lock is held: the waiter can only see it if it waited.
        insert(&held, 1)?;
        insert(&held, 2)?;
        assert!(opened.try_recv().is_err(), "waiter got in while locked");
        drop(held);
        assert_eq!(opened.recv()?, 2);
        assert!(waiter.join().is_ok_and(|result| result.is_ok()));
        Ok(())
    }

    #[test]
    fn concurrent_sessions_are_serialised() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("store.db");
        drop(Database::open(&path, key(1))?);
        // Each thread reads the row count and writes count + 1 as a new key inside one
        // session. Without mutual exclusion two threads read the same count and the second
        // insert fails on the primary key.
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                thread::spawn(move || -> Result<(), StoreError> {
                    let database = Database::open(&path, key(1))?;
                    for _ in 0..10 {
                        let session = database.session()?;
                        let next = count(&session)?;
                        thread::yield_now();
                        insert(&session, next)?;
                    }
                    Ok(())
                })
            })
            .collect();
        for worker in workers {
            assert!(worker.join().is_ok_and(|result| result.is_ok()));
        }
        let database = Database::open(&path, key(1))?;
        assert_eq!(count(&database.session()?)?, 40);
        Ok(())
    }

    #[test]
    fn transaction_commits_on_ok() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let database = Database::open(&directory.file("store.db"), key(1))?;
        let session = database.session()?;
        session.transaction(|session| {
            insert(session, 1)?;
            insert(session, 2)
        })?;
        drop(session);
        assert_eq!(count(&database.session()?)?, 2);
        Ok(())
    }

    #[test]
    fn transaction_rolls_back_on_err() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let database = Database::open(&directory.file("store.db"), key(1))?;
        let session = database.session()?;
        let result: Result<(), StoreError> = session.transaction(|session| {
            insert(session, 1)?;
            insert(session, 1)
        });
        assert!(result.is_err());
        assert_eq!(count(&session)?, 0);
        // The session stays usable after the rollback.
        insert(&session, 3)?;
        assert_eq!(count(&session)?, 1);
        Ok(())
    }

    #[test]
    fn nested_rollback_keeps_the_outer_work() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let database = Database::open(&directory.file("store.db"), key(1))?;
        let session = database.session()?;
        session.transaction(|session| {
            insert(session, 1)?;
            let inner: Result<(), StoreError> = session.transaction(|session| {
                insert(session, 2)?;
                Err(StoreError::Busy)
            });
            assert!(inner.is_err());
            Ok::<_, StoreError>(())
        })?;
        assert_eq!(count(&session)?, 1);
        Ok(())
    }

    #[test]
    #[expect(clippy::panic, reason = "the panic stands in for the process dying")]
    fn a_panic_mid_transaction_leaves_nothing() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("store.db");
        let database = Database::open(&path, key(1))?;
        let session = database.session()?;
        // A panic stands in for the process dying halfway through an MLS operation.
        let crashed = thread::spawn(move || {
            let _: Result<(), StoreError> = session.transaction(|session| {
                insert(session, 1)?;
                std::panic::panic_any("killed mid-transaction")
            });
        })
        .join();
        assert!(crashed.is_err());
        let reopened = Database::open(&path, key(1))?;
        assert_eq!(count(&reopened.session()?)?, 0);
        Ok(())
    }

    #[test]
    fn committed_work_survives_reopening() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("store.db");
        {
            let database = Database::open(&path, key(1))?;
            insert(&database.session()?, 9)?;
        }
        let database = Database::open(&path, key(1))?;
        assert_eq!(count(&database.session()?)?, 1);
        Ok(())
    }

    #[test]
    fn checkpoint_empties_the_log() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("store.db");
        let database = Database::open(&path, key(1))?;
        let session = database.session()?;
        insert(&session, 1)?;
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        assert!(std::fs::metadata(&wal)?.len() > 0);
        session.checkpoint()?;
        assert_eq!(std::fs::metadata(&wal)?.len(), 0);
        Ok(())
    }
}
