// SPDX-License-Identifier: AGPL-3.0-only
//! Shared helpers for this crate's unit tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_geo::{Circle, Coordinate, Fence, Meters};
use rusqlite::Connection;
use zeroize::Zeroizing;

use pingers_proto::{GroupId, LeafIndex, PartyId};
use pingers_time::UnixSeconds;

use crate::{Database, DatabaseKey, Party, PartyName, Session, StoreError};

/// Everything a test can fail with.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TestError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Geo(#[from] pingers_geo::GeoError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error(transparent)]
    Anon(#[from] pingers_anon::AnonError),
    #[error(transparent)]
    Receive(#[from] std::sync::mpsc::RecvError),
    #[error(transparent)]
    Mls(#[from] pingers_mls::StorageError),
}

/// A fresh directory under the system temporary directory, removed on drop.
pub(crate) struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub(crate) fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pingers-store-test-{}-{unique}",
            std::process::id()
        ));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub(crate) fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.path));
    }
}

/// A database key whose bytes are all `fill`.
pub(crate) fn key(fill: u8) -> DatabaseKey {
    DatabaseKey::from_bytes(Zeroizing::new([fill; 32]))
}

/// A freshly migrated database and an open session on it.
pub(crate) struct Fixture {
    // Field order: the session closes before the directory is removed.
    pub(crate) session: Session,
    _database: Database,
    _directory: TestDir,
}

impl Fixture {
    pub(crate) fn new() -> Result<Self, TestError> {
        let directory = TestDir::new()?;
        let database = Database::open(&directory.file("store.db"), key(7))?;
        let session = database.session()?;
        Ok(Self {
            session,
            _database: database,
            _directory: directory,
        })
    }

    /// Rows in `table`.
    pub(crate) fn rows(&self, table: &str) -> Result<i64, StoreError> {
        let sql = format!("SELECT count(*) FROM {table}");
        self.session
            .with(|connection| Ok(connection.query_row(&sql, [], |row| row.get(0))?))
    }
}

/// The `CREATE` statements of every table and index, sorted, for schema comparisons.
pub(crate) fn schema(connection: &Connection) -> Result<Vec<String>, StoreError> {
    let mut statement = connection
        .prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type, name")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// A circular fence.
pub(crate) fn circle(latitude: f64, longitude: f64, radius: f64) -> Result<Fence, TestError> {
    Ok(Fence::Circle(Circle::new(
        Coordinate::new(latitude, longitude)?,
        Meters::new(radius)?,
    )?))
}

/// A Party whose ids are all `fill`.
pub(crate) fn party(fill: u8) -> Result<Party, TestError> {
    Ok(Party {
        id: PartyId::from_bytes([fill; 16]),
        group: GroupId::from_bytes([fill; 32]),
        name: PartyName::new(format!("Party {fill}"))?,
        own_leaf: LeafIndex::new(u32::from(fill)),
        created_at: UnixSeconds::new(1_000 + u64::from(fill)),
        location_visibility: None,
    })
}
