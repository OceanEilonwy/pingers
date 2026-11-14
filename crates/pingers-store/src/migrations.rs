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
    // Rebuilding a table (create, copy, drop, rename) must not fire the ON DELETE actions of
    // the rows that point at it, so foreign keys are off while migrating (SQLite's documented
    // procedure; the pragma cannot change inside a transaction) and every migration is checked
    // with `foreign_key_check` before it commits.
    connection.pragma_update(None, "foreign_keys", "OFF")?;
    let migrated = apply(connection, current, target);
    connection.pragma_update(None, "foreign_keys", "ON")?;
    migrated
}

fn apply(connection: &Connection, current: u32, target: u32) -> Result<(), StoreError> {
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version > current && migration.version <= target)
    {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(migration.sql)?;
        let dangling = transaction
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some();
        if dangling {
            return Err(StoreError::Corrupt {
                field: "foreign keys after migration",
            });
        }
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
                let connection = Connection::open(&path)?;
                connection.execute_batch(&key(1).pragma())?;
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

    /// A device's data as the single-server schema (version 4) held it.
    const SINGLE_SERVER_DATA: &str = "
        INSERT INTO identity (singleton, account_id, sig_sk, server_url, server_pin)
            VALUES (1, zeroblob(16), x'0101010101010101010101010101010101010101010101010101010101010101',
                    'https://built-in.example', x'0202020202020202020202020202020202020202020202020202020202020202');
        INSERT INTO party (party_id, mls_group_id, name, my_leaf, created_at)
            VALUES (x'03030303030303030303030303030303', zeroblob(32), 'Climbing', 0, 1000);
        INSERT INTO member (party_id, leaf_index, sig_pk, display_name, verified)
            VALUES (x'03030303030303030303030303030303', 0, zeroblob(32), 'Ana', 0);
        INSERT INTO spot (spot_id, party_id, name, geometry, created_by, lamport, deleted)
            VALUES (zeroblob(16), x'03030303030303030303030303030303', 'Wall', x'00', zeroblob(32), 1, 0);
        INSERT INTO account_registration (singleton, state, code) VALUES (1, 1, 'letmein');
        INSERT INTO account_bearer (singleton, bearer, expires_at) VALUES (1, 'bearer', 5000);
        INSERT INTO pp_token (token, key_id) VALUES (x'04', zeroblob(32));
        INSERT INTO pp_token (token, key_id) VALUES (x'05', zeroblob(32));
        INSERT INTO rln_tree_leaf (position, value) VALUES (7, zeroblob(32));
        INSERT INTO request_queue (group_id, keep_until, request) VALUES (NULL, 9000, x'06');
        INSERT INTO board_upload (singleton, lease_id, revoke_key) VALUES (1, zeroblob(16), zeroblob(32));
    ";

    #[test]
    fn single_server_data_moves_to_the_built_in_server() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("single-server.db");
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(&key(1).pragma())?;
            migrate_to(&connection, 4)?;
            connection.execute_batch(SINGLE_SERVER_DATA)?;
        }
        let database = Database::open(&path, key(1))?;
        let session = database.session()?;
        let built_in = pingers_proto::ServerId::new("https://built-in.example".to_owned())?;

        let servers = session.servers().list()?;
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers.first().map(|record| (&record.server, record.trust)),
            Some((&built_in, crate::ServerTrust::BuiltIn))
        );
        assert_eq!(
            session.servers().server_pin(&built_in)?,
            Some(pingers_proto::KeyConfigHash::from_bytes([2; 32]))
        );
        assert!(session.identity().load()?.is_some());

        // Rebuilding `party` must not have cascaded away its members and Spots.
        let parties = session.parties().list()?;
        assert_eq!(parties.len(), 1);
        let party = parties
            .first()
            .ok_or(StoreError::NotFound { table: "party" })?;
        assert_eq!(party.server, built_in);
        assert_eq!(session.members().list(&party.id)?.len(), 1);
        let spots: i64 = session.with(|connection| {
            Ok(connection.query_row("SELECT count(*) FROM spot", [], |row| row.get(0))?)
        })?;
        assert_eq!(spots, 1);

        let server = session.server(&built_in)?;
        assert!(matches!(
            server.account().registration()?,
            crate::AccountRegistration::Registered { code: Some(code) } if code.expose() == "letmein"
        ));
        assert!(server.account().bearer()?.is_some());
        assert_eq!(server.privacy_pass_tokens().count()?, 2);
        assert_eq!(server.rln_tree().leaves()?.len(), 1);
        assert_eq!(server.request_queue().count()?, 1);
        assert!(server.board_upload().last()?.is_some());
        Ok(())
    }

    #[test]
    fn a_device_that_never_registered_migrates_to_no_server() -> Result<(), TestError> {
        let directory = TestDir::new()?;
        let path = directory.file("fresh-install.db");
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(&key(1).pragma())?;
            migrate_to(&connection, 4)?;
            connection
                .execute_batch("INSERT INTO settings (k, v) VALUES ('display_name', x'00');")?;
        }
        let database = Database::open(&path, key(1))?;
        let session = database.session()?;
        assert!(session.servers().list()?.is_empty());
        assert!(session.identity().load()?.is_none());
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
            let connection = Connection::open(&path)?;
            connection.execute_batch(&key(1).pragma())?;
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
