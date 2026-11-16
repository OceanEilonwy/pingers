// SPDX-License-Identifier: AGPL-3.0-only
//! Versioned schema migrations.
//!
//! The schema version is SQLite's `user_version`. Migration `n` takes a database at version
//! `n - 1` to version `n`; each runs in its own transaction together with the version bump, so
//! a crash leaves the database at the last completed version. Migrations are append-only: once
//! released, a migration's SQL never changes, because devices already ran it.

use rusqlite::Connection;

use crate::StoreError;

/// One schema step.
pub(crate) struct Migration {
    /// The version this migration produces.
    pub(crate) version: u32,
    /// The SQL that takes `version - 1` to `version`.
    pub(crate) sql: &'static str,
}

/// Every migration, in order, versions 1, 2, 3, ... without gaps.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("migrations/v1.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("migrations/v2.sql"),
    },
    Migration {
        version: 3,
        sql: include_str!("migrations/v3.sql"),
    },
    Migration {
        version: 4,
        sql: include_str!("migrations/v4.sql"),
    },
    Migration {
        version: 5,
        sql: include_str!("migrations/v5.sql"),
    },
    Migration {
        version: 6,
        sql: include_str!("migrations/v6.sql"),
    },
    Migration {
        version: 7,
        sql: include_str!("migrations/v7.sql"),
    },
    Migration {
        version: 8,
        sql: include_str!("migrations/v8.sql"),
    },
];

/// The version a fully migrated database has.
pub(crate) const LATEST: u32 = match MIGRATIONS.last() {
    Some(last) => last.version,
    None => 0,
};

pub(crate) fn version(connection: &Connection) -> Result<u32, StoreError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    u32::try_from(version).map_err(|_| StoreError::Corrupt {
        field: "user_version",
    })
}

/// Brings the database to [`LATEST`].
pub(crate) fn migrate(connection: &Connection) -> Result<(), StoreError> {
    migrate_to(connection, LATEST)
}

/// Applies the migrations after the current version, up to and including `target`.
pub(crate) fn migrate_to(connection: &Connection, target: u32) -> Result<(), StoreError> {
    let current = version(connection)?;
    if current > LATEST {
        return Err(StoreError::NewerSchema {
            found: current,
            supported: LATEST,
        });
    }
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version > current && migration.version <= target)
    {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(migration.sql)?;
        transaction.pragma_update(None, "user_version", migration.version)?;
        transaction.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::Database;
    use crate::test_support::{TestDir, TestError, key, schema};

    #[test]
    fn versions_start_at_one_without_gaps() {
        let versions: Vec<u32> = MIGRATIONS
            .iter()
            .map(|migration| migration.version)
            .collect();
        let expected: Vec<u32> = (1..=LATEST).collect();
        assert_eq!(versions, expected);
    }

    /// The schema reached from scratch, for comparison.
    fn fresh_schema(directory: &TestDir) -> Result<Vec<String>, TestError> {
        let path = directory.file("fresh.db");
        let database = Database::open(&path, key(1))?;
        let session = database.session()?;
        Ok(session.with(schema)?)
    }

    #[test]
    fn every_past_version_migrates_to_the_latest_schema() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let expected = fresh_schema(&directory)?;
        for start in 0..=LATEST {
            let path = directory.file(&format!("from-{start}.db"));
            {
                let connection = crate::database::open_keyed(&path, &key(1))?;
                migrate_to(&connection, start)?;
                assert_eq!(version(&connection)?, start);
            }
            let database = Database::open(&path, key(1))?;
            let session = database.session()?;
            let (reached, tables) =
                session.with(|connection| Ok((version(connection)?, schema(connection)?)))?;
            assert_eq!(reached, LATEST, "starting from version {start}");
            assert_eq!(tables, expected, "starting from version {start}");
        }
        Ok(())
    }

    #[test]
    fn opening_twice_does_not_reapply_migrations() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("twice.db");
        drop(Database::open(&path, key(1))?);
        let database = Database::open(&path, key(1))?;
        let reached = database.session()?.with(version)?;
        assert_eq!(reached, LATEST);
        Ok(())
    }

    #[test]
    fn a_newer_schema_is_refused() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("newer.db");
        drop(Database::open(&path, key(1))?);
        {
            let connection = crate::database::open_keyed(&path, &key(1))?;
            connection.pragma_update(None, "user_version", LATEST + 1)?;
        }
        let result = Database::open(&path, key(1));
        assert!(matches!(
            result,
            Err(StoreError::NewerSchema { found, supported })
                if found == LATEST + 1 && supported == LATEST
        ));
        Ok(())
    }
}
