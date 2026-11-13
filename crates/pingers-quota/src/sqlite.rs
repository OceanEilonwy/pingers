// SPDX-License-Identifier: AGPL-3.0-only
//! Both quota stores in one SQLite file (design C.1: `rln_leaf`, `rln_root`, `pp_key`,
//! `pp_spent`, `pp_quota`).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use pingers_anon::{IssuerKey, LeafValue};
use pingers_proto::{AccountId, HourlyRoot, MembershipIndex, RootEpoch};
use pingers_time::UnixSeconds;
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use zeroize::Zeroizing;

use crate::QuotaError;
use crate::store::{
    Allowance, Issuance, IssuanceDay, IssuerKeyRecord, MembershipStore, PrivacyPassStore, Spend,
    StoredLeaf, TokenNonce, Zeroing,
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS rln_leaf (
    idx         INTEGER PRIMARY KEY,
    leaf        BLOB NOT NULL,
    account_id  BLOB NOT NULL,
    live_from   INTEGER NOT NULL,
    zeroed_from INTEGER
) STRICT;
CREATE TABLE IF NOT EXISTS rln_root (
    root_epoch INTEGER PRIMARY KEY,
    root       BLOB NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS pp_key (
    key_id       BLOB PRIMARY KEY,
    issuer_sk    BLOB NOT NULL,
    not_before   INTEGER NOT NULL,
    issue_until  INTEGER NOT NULL,
    redeem_until INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS pp_spent (
    nonce      BLOB PRIMARY KEY,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS pp_quota (
    account_id BLOB NOT NULL,
    day        INTEGER NOT NULL,
    issued     INTEGER NOT NULL,
    PRIMARY KEY (account_id, day)
) STRICT;
";

/// The membership tree's leaves and roots and the Privacy Pass issuer's state in SQLite. One
/// connection behind a lock; every trait method is one immediate transaction.
pub struct SqliteQuotaStore {
    connection: Mutex<Connection>,
}

impl SqliteQuotaStore {
    /// Opens or creates the database at `path` and its tables.
    ///
    /// # Errors
    ///
    /// [`QuotaError::Sqlite`] if it cannot be opened or the schema cannot be created.
    pub fn open(path: &Path) -> Result<Self, QuotaError> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        Self::with_connection(connection)
    }

    /// A database that lives only as long as the store.
    ///
    /// # Errors
    ///
    /// [`QuotaError::Sqlite`] if the schema cannot be created.
    pub fn open_in_memory() -> Result<Self, QuotaError> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(connection: Connection) -> Result<Self, QuotaError> {
        connection.execute_batch(SCHEMA)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, QuotaError> {
        self.connection.lock().map_err(|_| QuotaError::Poisoned)
    }

    fn transact<R>(
        &self,
        action: impl FnOnce(&Transaction<'_>) -> Result<R, QuotaError>,
    ) -> Result<R, QuotaError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = action(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }
}

fn fixed<const N: usize>(bytes: Vec<u8>, field: &'static str) -> Result<[u8; N], QuotaError> {
    bytes
        .try_into()
        .map_err(|_| QuotaError::CorruptField { field })
}

fn zero_row(transaction: &Transaction<'_>, zeroing: Zeroing) -> Result<(), QuotaError> {
    let changed = transaction.execute(
        "UPDATE rln_leaf SET zeroed_from = ?2 WHERE idx = ?1",
        params![zeroing.index.get(), zeroing.from.get()],
    )?;
    if changed == 0 {
        return Err(QuotaError::CorruptField { field: "rln_leaf" });
    }
    Ok(())
}

impl MembershipStore for SqliteQuotaStore {
    fn leaves(&self) -> Result<Vec<StoredLeaf>, QuotaError> {
        self.transact(|transaction| {
            let mut statement = transaction.prepare(
                "SELECT idx, leaf, account_id, live_from, zeroed_from FROM rln_leaf ORDER BY idx",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, Option<u32>>(4)?,
                ))
            })?;
            rows.map(|row| {
                let (index, leaf, account, live_from, zeroed_from) = row?;
                Ok(StoredLeaf {
                    index: MembershipIndex::new(index),
                    value: LeafValue::from_bytes(fixed(leaf, "leaf")?),
                    account: AccountId::from_bytes(fixed(account, "account_id")?),
                    live_from: RootEpoch::new(live_from),
                    zeroed_from: zeroed_from.map(RootEpoch::new),
                })
            })
            .collect()
        })
    }

    fn register(&self, leaf: &StoredLeaf, replaces: Option<Zeroing>) -> Result<(), QuotaError> {
        self.transact(|transaction| {
            if let Some(zeroing) = replaces {
                zero_row(transaction, zeroing)?;
            }
            transaction.execute(
                "INSERT INTO rln_leaf (idx, leaf, account_id, live_from, zeroed_from)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    leaf.index.get(),
                    leaf.value.as_bytes().as_slice(),
                    leaf.account.as_bytes().as_slice(),
                    leaf.live_from.get(),
                    leaf.zeroed_from.map(RootEpoch::get)
                ],
            )?;
            Ok(())
        })
    }

    fn zero(&self, zeroing: Zeroing) -> Result<(), QuotaError> {
        self.transact(|transaction| zero_row(transaction, zeroing))
    }

    fn recent_roots(&self) -> Result<Vec<HourlyRoot>, QuotaError> {
        self.transact(|transaction| {
            let mut statement = transaction.prepare(
                "SELECT root_epoch, root FROM rln_root ORDER BY root_epoch DESC LIMIT 2",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            let mut roots = rows
                .map(|row| {
                    let (epoch, root) = row?;
                    Ok(HourlyRoot {
                        epoch: RootEpoch::new(epoch),
                        root: fixed(root, "root")?,
                    })
                })
                .collect::<Result<Vec<_>, QuotaError>>()?;
            roots.reverse();
            Ok(roots)
        })
    }

    fn insert_root(&self, root: HourlyRoot) -> Result<(), QuotaError> {
        self.transact(|transaction| {
            transaction.execute(
                "INSERT INTO rln_root (root_epoch, root) VALUES (?1, ?2)",
                params![root.epoch.get(), root.root.as_slice()],
            )?;
            Ok(())
        })
    }
}

impl PrivacyPassStore for SqliteQuotaStore {
    fn keys(&self) -> Result<Vec<IssuerKeyRecord>, QuotaError> {
        self.transact(|transaction| {
            let mut statement = transaction.prepare(
                "SELECT issuer_sk, not_before, issue_until, redeem_until FROM pp_key
                 ORDER BY not_before",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    Zeroizing::new(row.get::<_, Vec<u8>>(0)?),
                    row.get::<_, u64>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            })?;
            rows.map(|row| {
                let (secret, not_before, issue_until, redeem_until) = row?;
                Ok(IssuerKeyRecord {
                    key: IssuerKey::from_bytes(secret)?,
                    not_before: UnixSeconds::new(not_before),
                    issue_until: UnixSeconds::new(issue_until),
                    redeem_until: UnixSeconds::new(redeem_until),
                })
            })
            .collect()
        })
    }

    fn insert_key(&self, key: &IssuerKeyRecord) -> Result<(), QuotaError> {
        self.transact(|transaction| {
            transaction.execute(
                "INSERT INTO pp_key (key_id, issuer_sk, not_before, issue_until, redeem_until)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    key.key.key_id().as_slice(),
                    key.key.expose_bytes(),
                    key.not_before.get(),
                    key.issue_until.get(),
                    key.redeem_until.get()
                ],
            )?;
            Ok(())
        })
    }

    fn record_issuance(&self, issuance: Issuance, limit: u16) -> Result<Allowance, QuotaError> {
        self.transact(|transaction| {
            let account = issuance.account.as_bytes().as_slice();
            let issued: u16 = transaction
                .query_row(
                    "SELECT issued FROM pp_quota WHERE account_id = ?1 AND day = ?2",
                    params![account, issuance.day.get()],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
            match issued.checked_add(issuance.tokens) {
                Some(total) if total <= limit => {
                    transaction.execute(
                        "INSERT OR REPLACE INTO pp_quota (account_id, day, issued)
                         VALUES (?1, ?2, ?3)",
                        params![account, issuance.day.get(), total],
                    )?;
                    Ok(Allowance::Granted)
                }
                Some(_) | None => Ok(Allowance::Exceeded),
            }
        })
    }

    fn spend(&self, nonce: TokenNonce, expires_at: UnixSeconds) -> Result<Spend, QuotaError> {
        self.transact(|transaction| {
            let inserted = transaction.execute(
                "INSERT OR IGNORE INTO pp_spent (nonce, expires_at) VALUES (?1, ?2)",
                params![nonce.as_bytes().as_slice(), expires_at.get()],
            )?;
            Ok(if inserted == 0 {
                Spend::AlreadySpent
            } else {
                Spend::Fresh
            })
        })
    }

    fn prune(&self, now: UnixSeconds) -> Result<(), QuotaError> {
        self.transact(|transaction| {
            transaction.execute("DELETE FROM pp_key WHERE redeem_until <= ?1", [now.get()])?;
            transaction.execute("DELETE FROM pp_spent WHERE expires_at <= ?1", [now.get()])?;
            transaction.execute(
                "DELETE FROM pp_quota WHERE day < ?1",
                [IssuanceDay::containing(now).get()],
            )?;
            Ok(())
        })
    }
}
