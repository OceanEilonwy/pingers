// SPDX-License-Identifier: AGPL-3.0-only
//! `rln_tree_state`, `pp_allowance` and `prover_self_test` (schema version 5): what the main app
//! keeps so that a proof in a wake reads one row instead of rebuilding the tree, so that a token
//! refill never asks for more than the day's allowance has left, and so that the prover is
//! tested once per set of parameters (design B.4.8).

use core::time::Duration;

use pingers_anon::MembershipRoot;
use pingers_proto::RootEpoch;
use pingers_time::UnixSeconds;
use rusqlite::{OptionalExtension as _, params};

use crate::codec::{from_int, narrow, read_array, read_time, time, to_int};
use crate::server::Scope;
use crate::{ServerSession, Session, StoreError, TreePosition};

/// Levels of the membership tree, and so siblings in a Merkle path (design B.4.8: depth 20).
pub const PATH_LENGTH: usize = 20;

impl ServerSession<'_> {
    /// Which hourly root the local copy of this server's tree reproduces, and this device's
    /// path in it (`rln_tree_state` table).
    #[must_use]
    pub fn rln_tree_state(&self) -> RlnTreeStates<'_> {
        RlnTreeStates {
            session: self.scope(),
        }
    }

    /// This server's daily token limit and what this account was issued today
    /// (`pp_allowance` table).
    #[must_use]
    pub fn token_allowance(&self) -> TokenAllowances<'_> {
        TokenAllowances {
            session: self.scope(),
        }
    }
}

impl Session {
    /// The last prover self-test (`prover_self_test` table): device-wide, as it tests this
    /// device's prover.
    #[must_use]
    pub fn prover_self_test(&self) -> ProverSelfTests<'_> {
        ProverSelfTests { session: self }
    }
}

/// This device's Merkle path: the siblings from the leaf level up, each an encoded field
/// element (pingers-anon's `MerklePath`, kept opaque so the store does not link the prover).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredPath {
    /// The leaf the path is for.
    pub position: TreePosition,
    /// The siblings, leaf level first.
    pub siblings: [[u8; 32]; PATH_LENGTH],
}

/// What the local membership tree copy is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeState {
    /// The hourly root epoch whose root the copy reproduces.
    pub root_epoch: RootEpoch,
    /// That root.
    pub root: MembershipRoot,
    /// Leaves in the copy (live or zeroed).
    pub leaf_count: u32,
    /// This device's path to `root`, or `None` if the copy holds no leaf of this device.
    pub path: Option<StoredPath>,
}

/// Repository for `rln_tree_state`: one row per server.
pub struct RlnTreeStates<'session> {
    session: Scope<'session>,
}

impl RlnTreeStates<'_> {
    /// Records the state, replacing the previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn save(&self, state: &TreeState) -> Result<(), StoreError> {
        let path = state.path.map(|path| path.siblings.concat());
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO rln_tree_state \
                 (server_key, root_epoch, root, leaf_count, path_position, path) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    self.session.key(),
                    state.root_epoch.get(),
                    state.root.as_bytes(),
                    state.leaf_count,
                    state.path.map(|path| path.position.get()),
                    path,
                ],
            )?;
            Ok(())
        })
    }

    /// The state, if the tree was ever refreshed.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn load(&self) -> Result<Option<TreeState>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT root_epoch, root, leaf_count, path_position, path \
                     FROM rln_tree_state WHERE server_key = ?1",
                    [self.session.key()],
                    |row| {
                        Ok((|| {
                            let position: Option<i64> = row.get(3)?;
                            let path: Option<Vec<u8>> = row.get(4)?;
                            let path = match (position, path) {
                                (Some(position), Some(bytes)) => Some(StoredPath {
                                    position: TreePosition::new(narrow(
                                        position,
                                        "rln_tree_state.path_position",
                                    )?),
                                    siblings: siblings(&bytes)?,
                                }),
                                (None, None) => None,
                                (Some(_), None) | (None, Some(_)) => {
                                    return Err(StoreError::Corrupt {
                                        field: "rln_tree_state.path",
                                    });
                                }
                            };
                            Ok(TreeState {
                                root_epoch: RootEpoch::new(narrow(
                                    row.get(0)?,
                                    "rln_tree_state.root_epoch",
                                )?),
                                root: MembershipRoot::from_bytes(read_array(
                                    row,
                                    1,
                                    "rln_tree_state.root",
                                )?),
                                leaf_count: narrow(row.get(2)?, "rln_tree_state.leaf_count")?,
                                path,
                            })
                        })())
                    },
                )
                .optional()?
                .transpose()
        })
    }

    /// Forgets the state (the local copy is being rebuilt from scratch).
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn clear(&self) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM rln_tree_state WHERE server_key = ?1",
                [self.session.key()],
            )?;
            Ok(())
        })
    }
}

fn siblings(bytes: &[u8]) -> Result<[[u8; 32]; PATH_LENGTH], StoreError> {
    let corrupt = StoreError::Corrupt {
        field: "rln_tree_state.path",
    };
    let (chunks, rest) = bytes.as_chunks::<32>();
    if !rest.is_empty() {
        return Err(corrupt);
    }
    <[[u8; 32]; PATH_LENGTH]>::try_from(chunks).map_err(|_| corrupt)
}

/// The account's token allowance as this device knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenLedger {
    /// Tokens the server issues per account per day (`Params.privacy_pass_daily_limit`).
    pub daily_limit: u16,
    /// The server's UTC day (days since the Unix epoch) `issued` counts.
    pub day: u64,
    /// Tokens issued to the account on `day`.
    pub issued: u16,
}

/// Repository for `pp_allowance`: one row per server.
pub struct TokenAllowances<'session> {
    session: Scope<'session>,
}

impl TokenAllowances<'_> {
    /// Records the ledger, replacing the previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] for a day past `i64::MAX`; [`StoreError::Database`].
    pub fn save(&self, ledger: &TokenLedger) -> Result<(), StoreError> {
        let day = to_int(ledger.day, "pp_allowance.day")?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO pp_allowance (server_key, daily_limit, day, issued) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![self.session.key(), ledger.daily_limit, day, ledger.issued],
            )?;
            Ok(())
        })
    }

    /// The ledger, if one was ever recorded.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn load(&self) -> Result<Option<TokenLedger>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT daily_limit, day, issued FROM pp_allowance WHERE server_key = ?1",
                    [self.session.key()],
                    |row| {
                        Ok((|| {
                            Ok(TokenLedger {
                                daily_limit: narrow(row.get(0)?, "pp_allowance.daily_limit")?,
                                day: from_int(row.get(1)?, "pp_allowance.day")?,
                                issued: narrow(row.get(2)?, "pp_allowance.issued")?,
                            })
                        })())
                    },
                )
                .optional()?
                .transpose()
        })
    }
}

/// The outcome of a prover self-test, as stored (pingers-anon's `SelfTestVerdict`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProverVerdict {
    /// Proofs verify and the 95th percentile is within the limit.
    Passed,
    /// Proofs verify but are too slow.
    TooSlow,
    /// The embedded key is not the ceremony's.
    WrongParameters,
    /// A proof did not verify, or the verifier accepted one it must refuse.
    ProofRejected,
}

impl ProverVerdict {
    const fn to_column(self) -> i64 {
        match self {
            Self::Passed => 0,
            Self::TooSlow => 1,
            Self::WrongParameters => 2,
            Self::ProofRejected => 3,
        }
    }

    const fn from_column(value: i64) -> Result<Self, StoreError> {
        match value {
            0 => Ok(Self::Passed),
            1 => Ok(Self::TooSlow),
            2 => Ok(Self::WrongParameters),
            3 => Ok(Self::ProofRejected),
            _ => Err(StoreError::Corrupt {
                field: "prover_self_test.verdict",
            }),
        }
    }
}

/// A recorded prover self-test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfTestRecord {
    /// pingers-anon's `PARAMETERS_ID` when the test ran.
    pub parameters: String,
    /// The verdict.
    pub verdict: ProverVerdict,
    /// The first proof, including loading the key.
    pub first: Duration,
    /// The median timed proof.
    pub median: Duration,
    /// The 95th percentile.
    pub p95: Duration,
    /// How many proofs were timed.
    pub timed: u8,
    /// When the test ran.
    pub tested_at: UnixSeconds,
}

/// Repository for `prover_self_test`: one row.
pub struct ProverSelfTests<'session> {
    session: &'session Session,
}

fn millis(duration: Duration, field: &'static str) -> Result<i64, StoreError> {
    let millis =
        u64::try_from(duration.as_millis()).map_err(|_| StoreError::OutOfRange { field })?;
    to_int(millis, field)
}

fn read_millis(value: i64, field: &'static str) -> Result<Duration, StoreError> {
    Ok(Duration::from_millis(from_int(value, field)?))
}

impl ProverSelfTests<'_> {
    /// Records a test, replacing the previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] for a time that does not fit; [`StoreError::Database`].
    pub fn save(&self, record: &SelfTestRecord) -> Result<(), StoreError> {
        let first = millis(record.first, "prover_self_test.first_ms")?;
        let median = millis(record.median, "prover_self_test.median_ms")?;
        let p95 = millis(record.p95, "prover_self_test.p95_ms")?;
        let tested_at = time(record.tested_at, "prover_self_test.tested_at")?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO prover_self_test \
                 (singleton, parameters, verdict, first_ms, median_ms, p95_ms, timed, tested_at) \
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    record.parameters,
                    record.verdict.to_column(),
                    first,
                    median,
                    p95,
                    record.timed,
                    tested_at,
                ],
            )?;
            Ok(())
        })
    }

    /// The last test, if any ran.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn load(&self) -> Result<Option<SelfTestRecord>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT parameters, verdict, first_ms, median_ms, p95_ms, timed, tested_at \
                     FROM prover_self_test",
                    [],
                    |row| {
                        Ok((|| {
                            Ok(SelfTestRecord {
                                parameters: row.get(0)?,
                                verdict: ProverVerdict::from_column(row.get(1)?)?,
                                first: read_millis(row.get(2)?, "prover_self_test.first_ms")?,
                                median: read_millis(row.get(3)?, "prover_self_test.median_ms")?,
                                p95: read_millis(row.get(4)?, "prover_self_test.p95_ms")?,
                                timed: narrow(row.get(5)?, "prover_self_test.timed")?,
                                tested_at: read_time(row, 6, "prover_self_test.tested_at")?,
                            })
                        })())
                    },
                )
                .optional()?
                .transpose()
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::{Fixture, TestError};

    fn path() -> StoredPath {
        let mut siblings = [[0_u8; 32]; PATH_LENGTH];
        for (sibling, fill) in siblings.iter_mut().zip(1_u8..) {
            *sibling = [fill; 32];
        }
        StoredPath {
            position: TreePosition::new(7),
            siblings,
        }
    }

    #[test]
    fn tree_state_round_trips_with_and_without_a_path() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let states = server.rln_tree_state();
        assert_eq!(states.load()?, None);
        let mut state = TreeState {
            root_epoch: RootEpoch::new(490_000),
            root: MembershipRoot::from_bytes([9; 32]),
            leaf_count: 12,
            path: Some(path()),
        };
        states.save(&state)?;
        assert_eq!(states.load()?, Some(state));
        state.path = None;
        state.leaf_count = 13;
        states.save(&state)?;
        assert_eq!(states.load()?, Some(state));
        states.clear()?;
        assert_eq!(states.load()?, None);
        Ok(())
    }

    #[test]
    fn a_path_of_the_wrong_length_is_refused() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let refused = fixture.session.with(|connection| {
            Ok(connection.execute(
                "INSERT INTO rln_tree_state (server_key, root_epoch, root, leaf_count, \
                 path_position, path) VALUES (1, 1, ?1, 1, 0, ?2)",
                params![[0_u8; 32].as_slice(), [0_u8; 639].as_slice()],
            )?)
        });
        assert!(refused.is_err());
        Ok(())
    }

    #[test]
    fn the_token_ledger_round_trips() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let allowances = server.token_allowance();
        assert_eq!(allowances.load()?, None);
        let ledger = TokenLedger {
            daily_limit: 300,
            day: 20_406,
            issued: 281,
        };
        allowances.save(&ledger)?;
        assert_eq!(allowances.load()?, Some(ledger));
        // Another server keeps its own allowance.
        fixture
            .session
            .servers()
            .put(&crate::test_support::server_record("https://other.test")?)?;
        let other = fixture.session.server(&pingers_proto::ServerId::new(
            "https://other.test".to_owned(),
        )?)?;
        assert_eq!(other.token_allowance().load()?, None);
        assert_eq!(other.rln_tree_state().load()?, None);
        Ok(())
    }

    #[test]
    fn a_self_test_record_round_trips_in_milliseconds() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let tests = fixture.session.prover_self_test();
        assert_eq!(tests.load()?, None);
        for verdict in [
            ProverVerdict::Passed,
            ProverVerdict::TooSlow,
            ProverVerdict::WrongParameters,
            ProverVerdict::ProofRejected,
        ] {
            let record = SelfTestRecord {
                parameters: "rln-test".to_owned(),
                verdict,
                first: Duration::from_millis(850),
                median: Duration::from_millis(372),
                p95: Duration::from_millis(455),
                timed: 20,
                tested_at: UnixSeconds::new(1_763_000_000),
            };
            tests.save(&record)?;
            assert_eq!(tests.load()?, Some(record));
        }
        Ok(())
    }
}
