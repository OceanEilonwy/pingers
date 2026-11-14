// SPDX-License-Identifier: AGPL-3.0-only
//! `pp_token`, `rln_identity`, `rln_counter` and the RLN tree tables.

use pingers_anon::{LeafValue, MESSAGE_LIMIT, MessageId, RlnSecret};
use pingers_proto::{RlnScope, RootEpoch};
use pingers_time::Bucket;
use rusqlite::{OptionalExtension as _, params};
use zeroize::Zeroizing;

use crate::codec::{bucket, narrow, read_array, read_secret};
use crate::{Session, StoreError, TokenKeyId, TreeLevel, TreeNodeHash, TreePosition, UnspentToken};

impl Session {
    /// Unspent Privacy Pass tokens, shared with background handlers (`pp_token` table).
    #[must_use]
    pub fn privacy_pass_tokens(&self) -> PrivacyPassTokens<'_> {
        PrivacyPassTokens { session: self }
    }

    /// This device's RLN identity (`rln_identity` table).
    #[must_use]
    pub fn rln_identity(&self) -> RlnIdentities<'_> {
        RlnIdentities { session: self }
    }

    /// RLN message ids spent per scope and bucket (`rln_counter` table).
    #[must_use]
    pub fn rln_counters(&self) -> RlnCounters<'_> {
        RlnCounters { session: self }
    }

    /// The local copy of the RLN membership tree (`rln_tree_leaf`, `rln_tree_node`).
    #[must_use]
    pub fn rln_tree(&self) -> RlnTree<'_> {
        RlnTree { session: self }
    }
}

/// An unspent token and the issuer key it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldToken {
    /// The token.
    pub token: UnspentToken,
    /// Its issuer key.
    pub key_id: TokenKeyId,
}

/// Repository for the `pp_token` table.
pub struct PrivacyPassTokens<'session> {
    session: &'session Session,
}

impl PrivacyPassTokens<'_> {
    /// Adds a batch of tokens issued under `key_id`, all or none.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn add(&self, tokens: &[UnspentToken], key_id: &TokenKeyId) -> Result<(), StoreError> {
        self.session.transaction(|session| {
            session.with(|connection| {
                let mut statement =
                    connection.prepare("INSERT INTO pp_token (token, key_id) VALUES (?1, ?2)")?;
                for token in tokens {
                    statement.execute(params![token.as_bytes(), key_id.as_bytes()])?;
                }
                Ok(())
            })
        })
    }

    /// Removes and returns the oldest token, so it can never be spent twice even if the
    /// request then fails.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn take(&self) -> Result<Option<HeldToken>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "DELETE FROM pp_token WHERE token_id = \
                     (SELECT min(token_id) FROM pp_token) RETURNING token, key_id",
                    [],
                    |row| {
                        Ok((|| {
                            Ok(HeldToken {
                                token: UnspentToken::from_bytes(row.get(0)?),
                                key_id: TokenKeyId::from_bytes(read_array(
                                    row,
                                    1,
                                    "pp_token.key_id",
                                )?),
                            })
                        })())
                    },
                )
                .optional()?
                .transpose()
        })
    }

    /// How many tokens are left.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the query fails.
    pub fn count(&self) -> Result<u32, StoreError> {
        self.session.with(|connection| {
            narrow(
                connection.query_row("SELECT count(*) FROM pp_token", [], |row| row.get(0))?,
                "pp_token count",
            )
        })
    }

    /// Deletes every token whose issuer key is not in `keys` (the keys the issuer still lists);
    /// returns how many.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn retain_keys(&self, keys: &[TokenKeyId]) -> Result<usize, StoreError> {
        self.session.transaction(|session| {
            session.with(|connection| {
                let mut doomed = Vec::new();
                {
                    let mut statement =
                        connection.prepare("SELECT DISTINCT key_id FROM pp_token")?;
                    let mut rows = statement.query([])?;
                    while let Some(row) = rows.next()? {
                        let held: Vec<u8> = row.get(0)?;
                        if !keys
                            .iter()
                            .any(|key| key.as_bytes().as_slice() == held.as_slice())
                        {
                            doomed.push(held);
                        }
                    }
                }
                let mut deleted = 0_usize;
                for key in doomed {
                    deleted = deleted.saturating_add(
                        connection.execute("DELETE FROM pp_token WHERE key_id = ?1", [key])?,
                    );
                }
                Ok(deleted)
            })
        })
    }

    /// Deletes every token of a retired issuer key; returns how many.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete_for_key(&self, key_id: &TokenKeyId) -> Result<usize, StoreError> {
        self.session.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM pp_token WHERE key_id = ?1",
                [key_id.as_bytes()],
            )?)
        })
    }
}

/// This device's RLN identity (design C.2 `rln_identity`). Never restored from a backup.
pub struct RlnIdentity {
    /// The secret.
    pub secret: RlnSecret,
    /// Its leaf in the membership tree.
    pub leaf: TreePosition,
    /// The first root epoch whose root contains the leaf.
    pub live_from: RootEpoch,
}

/// Repository for the `rln_identity` table: one row.
pub struct RlnIdentities<'session> {
    session: &'session Session,
}

/// The stored columns, before the secret is validated by pingers-anon.
struct RlnIdentityRow {
    secret: Zeroizing<[u8; 32]>,
    leaf: TreePosition,
    live_from: RootEpoch,
}

impl RlnIdentities<'_> {
    /// Stores the identity, replacing any previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn save(&self, identity: &RlnIdentity) -> Result<(), StoreError> {
        self.save_row(&RlnIdentityRow {
            secret: Zeroizing::new(*identity.secret.expose_bytes()),
            leaf: identity.leaf,
            live_from: identity.live_from,
        })
    }

    /// The identity, if registered.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode, including a secret
    /// pingers-anon rejects.
    pub fn load(&self) -> Result<Option<RlnIdentity>, StoreError> {
        self.load_row()?
            .map(|row| {
                Ok(RlnIdentity {
                    secret: RlnSecret::from_bytes(row.secret).map_err(|_| StoreError::Corrupt {
                        field: "rln_identity.sk",
                    })?,
                    leaf: row.leaf,
                    live_from: row.live_from,
                })
            })
            .transpose()
    }

    fn save_row(&self, row: &RlnIdentityRow) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO rln_identity (singleton, sk, leaf_idx, \
                 live_from_root_epoch) VALUES (1, ?1, ?2, ?3)",
                params![row.secret.as_slice(), row.leaf.get(), row.live_from.get(),],
            )?;
            Ok(())
        })
    }

    fn load_row(&self) -> Result<Option<RlnIdentityRow>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT sk, leaf_idx, live_from_root_epoch FROM rln_identity",
                    [],
                    |row| {
                        Ok((|| {
                            Ok(RlnIdentityRow {
                                secret: read_secret(row, 0, "rln_identity.sk")?,
                                leaf: TreePosition::new(narrow(
                                    row.get(1)?,
                                    "rln_identity.leaf_idx",
                                )?),
                                live_from: RootEpoch::new(narrow(
                                    row.get(2)?,
                                    "rln_identity.live_from_root_epoch",
                                )?),
                            })
                        })())
                    },
                )
                .optional()?
                .transpose()
        })
    }
}

/// Repository for the `rln_counter` table.
pub struct RlnCounters<'session> {
    session: &'session Session,
}

impl RlnCounters<'_> {
    /// Reserves the next message id of `scope` in `bucket` and records it as spent before
    /// returning it, so a crash after sending can never reuse it (design B.4.8). `None` when
    /// all [`MESSAGE_LIMIT`] ids of the bucket are spent.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if the bucket does not fit; [`StoreError::Database`] if the
    /// write fails.
    pub fn reserve(&self, scope: RlnScope, at: Bucket) -> Result<Option<MessageId>, StoreError> {
        self.session.transaction(|session| {
            session.with(|connection| {
                let bucket_index = bucket(at, "rln_counter.bucket")?;
                let next: u8 = connection
                    .query_row(
                        "SELECT next_message_id FROM rln_counter WHERE scope = ?1 AND bucket = ?2",
                        params![scope.to_wire(), bucket_index],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .map_or(Ok(0), |next| narrow(next, "rln_counter.next_message_id"))?;
                let Ok(id) = MessageId::new(next) else {
                    return Ok(None);
                };
                connection.execute(
                    "INSERT OR REPLACE INTO rln_counter (scope, bucket, next_message_id) \
                     VALUES (?1, ?2, ?3)",
                    params![
                        scope.to_wire(),
                        bucket_index,
                        next.saturating_add(1).min(MESSAGE_LIMIT)
                    ],
                )?;
                Ok(Some(id))
            })
        })
    }

    /// Deletes counters of buckets before `oldest_kept`; returns how many.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete_before(&self, oldest_kept: Bucket) -> Result<usize, StoreError> {
        self.session.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM rln_counter WHERE bucket < ?1",
                [bucket(oldest_kept, "rln_counter.bucket")?],
            )?)
        })
    }
}

/// A leaf of the local membership tree copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeLeaf {
    /// Where.
    pub position: TreePosition,
    /// The leaf (zero for a removed device).
    pub value: LeafValue,
}

/// Repository for the RLN tree tables: the leaf log and cached interior nodes.
pub struct RlnTree<'session> {
    session: &'session Session,
}

impl RlnTree<'_> {
    /// Sets the leaf at a position (new registration or zeroing).
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn set_leaf(&self, leaf: &TreeLeaf) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO rln_tree_leaf (position, value) VALUES (?1, ?2)",
                params![leaf.position.get(), leaf.value.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// Every leaf, by position.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn leaves(&self) -> Result<Vec<TreeLeaf>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection
                .prepare("SELECT position, value FROM rln_tree_leaf ORDER BY position")?;
            let rows = statement.query_map([], |row| {
                Ok((|| {
                    Ok(TreeLeaf {
                        position: TreePosition::new(narrow(row.get(0)?, "rln_tree_leaf.position")?),
                        value: LeafValue::from_bytes(read_array(row, 1, "rln_tree_leaf.value")?),
                    })
                })())
            })?;
            rows.map(|row| row?).collect()
        })
    }

    /// Caches an interior node.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn put_node(
        &self,
        level: TreeLevel,
        position: TreePosition,
        hash: &TreeNodeHash,
    ) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO rln_tree_node (level, position, value) VALUES (?1, ?2, ?3)",
                params![level.get(), position.get(), hash.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// A cached interior node.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn node(
        &self,
        level: TreeLevel,
        position: TreePosition,
    ) -> Result<Option<TreeNodeHash>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT value FROM rln_tree_node WHERE level = ?1 AND position = ?2",
                    params![level.get(), position.get()],
                    |row| {
                        Ok(read_array(row, 0, "rln_tree_node.value").map(TreeNodeHash::from_bytes))
                    },
                )
                .optional()?
                .transpose()
        })
    }

    /// Drops every cached interior node (after leaves change, before recomputing).
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn clear_nodes(&self) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute("DELETE FROM rln_tree_node", [])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::{Fixture, TestError};

    fn token(fill: u8) -> UnspentToken {
        UnspentToken::from_bytes(vec![fill; 40])
    }

    #[test]
    fn tokens_are_taken_oldest_first_and_only_once() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let tokens = fixture.session.privacy_pass_tokens();
        let old_key = TokenKeyId::from_bytes([1; 32]);
        let new_key = TokenKeyId::from_bytes([2; 32]);
        assert_eq!(tokens.take()?, None);
        tokens.add(&[token(1), token(2)], &old_key)?;
        tokens.add(&[token(3)], &new_key)?;
        assert_eq!(tokens.count()?, 3);
        assert_eq!(
            tokens.take()?,
            Some(HeldToken {
                token: token(1),
                key_id: old_key
            })
        );
        assert_eq!(tokens.count()?, 2);
        assert_eq!(tokens.delete_for_key(&old_key)?, 1);
        assert_eq!(tokens.take()?.map(|held| held.key_id), Some(new_key));
        assert_eq!(tokens.count()?, 0);
        Ok(())
    }

    #[test]
    fn only_tokens_of_listed_keys_are_kept() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let tokens = fixture.session.privacy_pass_tokens();
        let retired = TokenKeyId::from_bytes([1; 32]);
        let current = TokenKeyId::from_bytes([2; 32]);
        let next = TokenKeyId::from_bytes([3; 32]);
        tokens.add(&[token(1), token(2)], &retired)?;
        tokens.add(&[token(3)], &current)?;
        tokens.add(&[token(4)], &next)?;
        assert_eq!(tokens.retain_keys(&[current, next])?, 2);
        assert_eq!(tokens.count()?, 2);
        assert_eq!(tokens.retain_keys(&[current, next])?, 0);
        assert_eq!(tokens.take()?.map(|held| held.key_id), Some(current));
        assert_eq!(tokens.retain_keys(&[])?, 1);
        assert_eq!(tokens.count()?, 0);
        Ok(())
    }

    #[test]
    fn rln_identity_round_trips_through_pingers_anon() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let identities = fixture.session.rln_identity();
        identities.save(&RlnIdentity {
            secret: RlnSecret::from_bytes(Zeroizing::new([6; 32]))?,
            leaf: TreePosition::new(12),
            live_from: RootEpoch::new(34),
        })?;
        let loaded = identities.load()?.ok_or(StoreError::NotFound {
            table: "rln_identity",
        })?;
        assert_eq!(*loaded.secret.expose_bytes(), [6; 32]);
        assert_eq!(loaded.leaf, TreePosition::new(12));
        assert_eq!(loaded.live_from, RootEpoch::new(34));
        Ok(())
    }

    #[test]
    fn a_stored_secret_pingers_anon_rejects_is_corrupt() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let identities = fixture.session.rln_identity();
        // 0xff..ff is not a canonical BN254 scalar.
        identities.save_row(&RlnIdentityRow {
            secret: Zeroizing::new([0xff; 32]),
            leaf: TreePosition::new(1),
            live_from: RootEpoch::new(2),
        })?;
        assert!(matches!(
            identities.load(),
            Err(StoreError::Corrupt {
                field: "rln_identity.sk"
            })
        ));
        Ok(())
    }

    #[test]
    fn rln_identity_columns_round_trip() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let identities = fixture.session.rln_identity();
        assert!(identities.load_row()?.is_none());
        assert!(identities.load()?.is_none());
        identities.save_row(&RlnIdentityRow {
            secret: Zeroizing::new([6; 32]),
            leaf: TreePosition::new(12),
            live_from: RootEpoch::new(34),
        })?;
        let row = identities.load_row()?.ok_or(StoreError::NotFound {
            table: "rln_identity",
        })?;
        assert_eq!(*row.secret, [6; 32]);
        assert_eq!(row.leaf, TreePosition::new(12));
        assert_eq!(row.live_from, RootEpoch::new(34));
        identities.save_row(&RlnIdentityRow {
            secret: Zeroizing::new([7; 32]),
            leaf: TreePosition::new(1),
            live_from: RootEpoch::new(2),
        })?;
        assert_eq!(fixture.rows("rln_identity")?, 1);
        Ok(())
    }

    #[test]
    fn a_stored_root_epoch_beyond_the_wire_range_is_corrupt() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let identities = fixture.session.rln_identity();
        identities.save_row(&RlnIdentityRow {
            secret: Zeroizing::new([6; 32]),
            leaf: TreePosition::new(12),
            live_from: RootEpoch::new(u32::MAX),
        })?;
        assert_eq!(
            identities.load_row()?.map(|row| row.live_from),
            Some(RootEpoch::new(u32::MAX))
        );
        fixture.session.with(|connection| {
            connection.execute(
                "UPDATE rln_identity SET live_from_root_epoch = ?1",
                [i64::from(u32::MAX).saturating_add(1)],
            )?;
            Ok(())
        })?;
        assert!(matches!(
            identities.load_row(),
            Err(StoreError::Corrupt {
                field: "rln_identity.live_from_root_epoch"
            })
        ));
        Ok(())
    }

    #[test]
    fn message_ids_are_reserved_once_per_scope_and_bucket() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let counters = fixture.session.rln_counters();
        let now = Bucket::from_index(100).map_err(|_| StoreError::Busy)?;
        let next = Bucket::from_index(101).map_err(|_| StoreError::Busy)?;
        let mut reserved = Vec::new();
        while let Some(id) = counters.reserve(RlnScope::Board, now)? {
            reserved.push(id.get());
        }
        assert_eq!(reserved, (0..MESSAGE_LIMIT).collect::<Vec<_>>());
        assert_eq!(counters.reserve(RlnScope::Board, now)?, None);
        assert_eq!(
            counters.reserve(RlnScope::Mailbox, now)?,
            Some(MessageId::new(0)?)
        );
        assert_eq!(
            counters.reserve(RlnScope::Board, next)?,
            Some(MessageId::new(0)?)
        );
        assert_eq!(counters.delete_before(next)?, 2);
        assert_eq!(
            counters.reserve(RlnScope::Board, next)?,
            Some(MessageId::new(1)?)
        );
        Ok(())
    }

    #[test]
    fn tree_leaves_and_nodes_round_trip() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let tree = fixture.session.rln_tree();
        let second = TreeLeaf {
            position: TreePosition::new(2),
            value: LeafValue::from_bytes([2; 32]),
        };
        let first = TreeLeaf {
            position: TreePosition::new(0),
            value: LeafValue::from_bytes([1; 32]),
        };
        tree.set_leaf(&second)?;
        tree.set_leaf(&first)?;
        let zeroed = TreeLeaf {
            value: LeafValue::from_bytes([0; 32]),
            ..second
        };
        tree.set_leaf(&zeroed)?;
        assert_eq!(tree.leaves()?, vec![first, zeroed]);
        let hash = TreeNodeHash::from_bytes([5; 32]);
        tree.put_node(TreeLevel::new(3), TreePosition::new(1), &hash)?;
        assert_eq!(
            tree.node(TreeLevel::new(3), TreePosition::new(1))?,
            Some(hash)
        );
        assert_eq!(tree.node(TreeLevel::new(3), TreePosition::new(0))?, None);
        assert_eq!(tree.node(TreeLevel::new(2), TreePosition::new(1))?, None);
        tree.clear_nodes()?;
        assert_eq!(tree.node(TreeLevel::new(3), TreePosition::new(1))?, None);
        assert_eq!(tree.leaves()?.len(), 2);
        Ok(())
    }
}
