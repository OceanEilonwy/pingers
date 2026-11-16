// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-parties' per-Party state: `party_config` and `party_relay`, `roster_epoch` and
//! `roster_entry`, `member_subscription`, `pending_commit` and `profile_waiting`. Every row
//! goes with its Party (`ON DELETE CASCADE`).

use pingers_crypto::AccountPublicKey;
use pingers_mls::{Roster, RosterEntry};
use pingers_proto::{Epoch, LeafIndex, PartyId, RelayUrl, ServerId, SubscriptionId};
use rusqlite::{OptionalExtension as _, params};

use crate::codec::{from_int, narrow, read_array, to_int};
use crate::{Session, StoreError};

impl Session {
    /// A Party's server, suite and relays (`party_config`, `party_relay`).
    #[must_use]
    pub fn party_configs(&self) -> PartyConfigs<'_> {
        PartyConfigs { session: self }
    }

    /// Rosters of recent epochs (`roster_epoch`, `roster_entry`).
    #[must_use]
    pub fn rosters(&self) -> Rosters<'_> {
        Rosters { session: self }
    }

    /// Members' subscription ids in their Party groups (`member_subscription`).
    #[must_use]
    pub fn member_subscriptions(&self) -> MemberSubscriptions<'_> {
        MemberSubscriptions { session: self }
    }

    /// This device's commits waiting for the server (`pending_commit`).
    #[must_use]
    pub fn pending_commits(&self) -> PendingCommits<'_> {
        PendingCommits { session: self }
    }

    /// Parties where this device's changed profile waits for a commit (`profile_waiting`).
    #[must_use]
    pub fn profiles_waiting(&self) -> ProfilesWaiting<'_> {
        ProfilesWaiting { session: self }
    }
}

/// A Party's MLS ciphersuite (design B.8), fixed at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartySuite {
    /// Suite 0x0003, the default.
    Classical,
    /// The X-Wing suite ("quantum-resistant Party").
    QuantumResistant,
}

/// Which OHTTP relays a Party's anonymous requests use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayList {
    /// The relays the server lists.
    ServerDefault,
    /// This device's choice, in order of preference.
    Custom(Vec<RelayUrl>),
}

/// A Party's server, suite and relays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyConfig {
    /// The server hosting the Party's group and mailboxes.
    pub server: ServerId,
    /// The Party's suite.
    pub suite: PartySuite,
    /// Its relays.
    pub relays: RelayList,
}

/// Repository for `party_config` and `party_relay`.
pub struct PartyConfigs<'session> {
    session: &'session Session,
}

impl PartyConfigs<'_> {
    /// Stores a Party's configuration, replacing the previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if the Party does not exist; [`StoreError::ServerMismatch`] if
    /// `config` names another server than the one the Party lives on; [`StoreError::Database`]
    /// if the write fails.
    pub fn put(&self, party: &PartyId, config: &PartyConfig) -> Result<(), StoreError> {
        let suite: i64 = match config.suite {
            PartySuite::Classical => 0,
            PartySuite::QuantumResistant => 1,
        };
        let (custom, relays): (i64, &[RelayUrl]) = match &config.relays {
            RelayList::ServerDefault => (0, &[]),
            RelayList::Custom(relays) => (1, relays),
        };
        self.session.with(|connection| {
            // The Party's server is kept once, in its `party` row (schema version 8); the
            // configuration must agree with it.
            let lives_on: Option<String> = connection
                .query_row(
                    "SELECT server.origin FROM party JOIN server USING (server_key) \
                     WHERE party.party_id = ?1",
                    [party.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?;
            match lives_on {
                None => return Err(StoreError::NotFound { table: "party" }),
                Some(origin) if origin != config.server.as_str() => {
                    return Err(StoreError::ServerMismatch);
                }
                Some(_) => {}
            }
            connection.execute(
                "INSERT OR REPLACE INTO party_config (party_id, suite, custom_relays) \
                 VALUES (?1, ?2, ?3)",
                params![party.as_bytes(), suite, custom],
            )?;
            connection.execute(
                "DELETE FROM party_relay WHERE party_id = ?1",
                [party.as_bytes()],
            )?;
            for (position, relay) in relays.iter().enumerate() {
                let position = to_int(
                    u64::try_from(position).map_err(|_| StoreError::OutOfRange {
                        field: "party_relay.position",
                    })?,
                    "party_relay.position",
                )?;
                connection.execute(
                    "INSERT INTO party_relay (party_id, position, url) VALUES (?1, ?2, ?3)",
                    params![party.as_bytes(), position, relay.as_str()],
                )?;
            }
            Ok(())
        })
    }

    /// A Party's configuration, if stored.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, party: &PartyId) -> Result<Option<PartyConfig>, StoreError> {
        self.session.with(|connection| {
            let row = connection
                .query_row(
                    "SELECT server.origin, party_config.suite, party_config.custom_relays \
                     FROM party_config JOIN party USING (party_id) JOIN server USING (server_key) \
                     WHERE party_config.party_id = ?1",
                    [party.as_bytes()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((server, suite, custom)) = row else {
                return Ok(None);
            };
            let server = ServerId::new(server).map_err(|_| StoreError::Corrupt {
                field: "server.origin",
            })?;
            let suite = match suite {
                0 => PartySuite::Classical,
                1 => PartySuite::QuantumResistant,
                _ => {
                    return Err(StoreError::Corrupt {
                        field: "party_config.suite",
                    });
                }
            };
            let relays = match custom {
                0 => RelayList::ServerDefault,
                1 => {
                    let mut statement = connection.prepare(
                        "SELECT url FROM party_relay WHERE party_id = ?1 ORDER BY position",
                    )?;
                    let urls = statement.query_map([party.as_bytes()], |row| row.get(0))?;
                    let mut relays = Vec::new();
                    for url in urls {
                        relays.push(RelayUrl::new(url?).map_err(|_| StoreError::Corrupt {
                            field: "party_relay.url",
                        })?);
                    }
                    RelayList::Custom(relays)
                }
                _ => {
                    return Err(StoreError::Corrupt {
                        field: "party_config.custom_relays",
                    });
                }
            };
            Ok(Some(PartyConfig {
                server,
                suite,
                relays,
            }))
        })
    }
}

/// Repository for `roster_epoch` and `roster_entry`.
pub struct Rosters<'session> {
    session: &'session Session,
}

impl Rosters<'_> {
    /// Records the roster at `epoch`, replacing one recorded for the same epoch.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] for an epoch past the column's range;
    /// [`StoreError::Database`] if the Party does not exist.
    pub fn record(&self, party: &PartyId, epoch: Epoch, roster: &Roster) -> Result<(), StoreError> {
        let epoch = to_int(epoch.get(), "roster_epoch.epoch")?;
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM roster_epoch WHERE party_id = ?1 AND epoch = ?2",
                params![party.as_bytes(), epoch],
            )?;
            connection.execute(
                "INSERT INTO roster_epoch (party_id, epoch) VALUES (?1, ?2)",
                params![party.as_bytes(), epoch],
            )?;
            for entry in roster.entries() {
                connection.execute(
                    "INSERT INTO roster_entry (party_id, epoch, leaf_index, sig_pk) \
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        party.as_bytes(),
                        epoch,
                        entry.leaf.get(),
                        entry.signature_key.as_bytes()
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// The epochs with a recorded roster, newest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored epoch is negative.
    pub fn epochs(&self, party: &PartyId) -> Result<Vec<Epoch>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT epoch FROM roster_epoch WHERE party_id = ?1 ORDER BY epoch DESC",
            )?;
            let rows = statement.query_map([party.as_bytes()], |row| row.get::<_, i64>(0))?;
            rows.map(|epoch| Ok(Epoch::new(from_int(epoch?, "roster_epoch.epoch")?)))
                .collect()
        })
    }

    /// The roster at `epoch`, in leaf order, if recorded.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn at(&self, party: &PartyId, epoch: Epoch) -> Result<Option<Roster>, StoreError> {
        let epoch = to_int(epoch.get(), "roster_epoch.epoch")?;
        self.session.with(|connection| {
            let known = connection
                .query_row(
                    "SELECT 1 FROM roster_epoch WHERE party_id = ?1 AND epoch = ?2",
                    params![party.as_bytes(), epoch],
                    |_| Ok(()),
                )
                .optional()?;
            if known.is_none() {
                return Ok(None);
            }
            let mut statement = connection.prepare(
                "SELECT leaf_index, sig_pk FROM roster_entry \
                 WHERE party_id = ?1 AND epoch = ?2 ORDER BY leaf_index",
            )?;
            let rows = statement.query_map(params![party.as_bytes(), epoch], |row| {
                Ok((|| {
                    Ok::<_, StoreError>(RosterEntry {
                        leaf: LeafIndex::new(narrow(row.get(0)?, "roster_entry.leaf_index")?),
                        signature_key: AccountPublicKey::from_bytes(read_array(
                            row,
                            1,
                            "roster_entry.sig_pk",
                        )?),
                    })
                })())
            })?;
            let entries = rows.map(|entry| entry?).collect::<Result<Vec<_>, _>>()?;
            Ok(Some(Roster::new(entries)))
        })
    }

    /// Keeps the `count` newest epochs' rosters and deletes the rest.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] for a count past the column's range;
    /// [`StoreError::Database`] if the delete fails.
    pub fn retain_newest(&self, party: &PartyId, count: usize) -> Result<(), StoreError> {
        let count = to_int(
            u64::try_from(count).map_err(|_| StoreError::OutOfRange {
                field: "roster_epoch count",
            })?,
            "roster_epoch count",
        )?;
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM roster_epoch WHERE party_id = ?1 AND epoch NOT IN \
                 (SELECT epoch FROM roster_epoch WHERE party_id = ?1 \
                  ORDER BY epoch DESC LIMIT ?2)",
                params![party.as_bytes(), count],
            )?;
            Ok(())
        })
    }
}

/// One member's subscription id in a Party group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemberSubscription {
    /// The member's account key.
    pub key: AccountPublicKey,
    /// Their subscription.
    pub subscription: SubscriptionId,
}

/// Repository for `member_subscription`.
pub struct MemberSubscriptions<'session> {
    session: &'session Session,
}

impl MemberSubscriptions<'_> {
    /// Sets a member's subscription, or with `None` forgets it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party does not exist or the write fails.
    pub fn set(
        &self,
        party: &PartyId,
        key: &AccountPublicKey,
        subscription: Option<SubscriptionId>,
    ) -> Result<(), StoreError> {
        self.session.with(|connection| {
            match subscription {
                Some(subscription) => connection.execute(
                    "INSERT OR REPLACE INTO member_subscription (party_id, sig_pk, sub_id) \
                     VALUES (?1, ?2, ?3)",
                    params![party.as_bytes(), key.as_bytes(), subscription.as_bytes()],
                )?,
                None => connection.execute(
                    "DELETE FROM member_subscription WHERE party_id = ?1 AND sig_pk = ?2",
                    params![party.as_bytes(), key.as_bytes()],
                )?,
            };
            Ok(())
        })
    }

    /// A member's subscription, if known.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(
        &self,
        party: &PartyId,
        key: &AccountPublicKey,
    ) -> Result<Option<SubscriptionId>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT sub_id FROM member_subscription WHERE party_id = ?1 AND sig_pk = ?2",
                    params![party.as_bytes(), key.as_bytes()],
                    |row| {
                        Ok(read_array(row, 0, "member_subscription.sub_id")
                            .map(SubscriptionId::from_bytes))
                    },
                )
                .optional()?
                .transpose()
        })
    }

    /// Every known subscription of the Party's members.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self, party: &PartyId) -> Result<Vec<MemberSubscription>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT sig_pk, sub_id FROM member_subscription WHERE party_id = ?1 \
                 ORDER BY sig_pk",
            )?;
            let rows = statement.query_map([party.as_bytes()], |row| {
                Ok((|| {
                    Ok::<_, StoreError>(MemberSubscription {
                        key: AccountPublicKey::from_bytes(read_array(
                            row,
                            0,
                            "member_subscription.sig_pk",
                        )?),
                        subscription: SubscriptionId::from_bytes(read_array(
                            row,
                            1,
                            "member_subscription.sub_id",
                        )?),
                    })
                })())
            })?;
            rows.map(|entry| entry?).collect()
        })
    }
}

/// Repository for `pending_commit`.
pub struct PendingCommits<'session> {
    session: &'session Session,
}

impl PendingCommits<'_> {
    /// Records the Party's in-flight commit (pingers-parties' encoding), replacing any.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party does not exist or the write fails.
    pub fn put(&self, party: &PartyId, body: &[u8]) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO pending_commit (party_id, body) VALUES (?1, ?2)",
                params![party.as_bytes(), body],
            )?;
            Ok(())
        })
    }

    /// The Party's in-flight commit, if any.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the read fails.
    pub fn get(&self, party: &PartyId) -> Result<Option<Vec<u8>>, StoreError> {
        self.session.with(|connection| {
            Ok(connection
                .query_row(
                    "SELECT body FROM pending_commit WHERE party_id = ?1",
                    [party.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    /// Forgets the Party's in-flight commit; forgetting none is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete(&self, party: &PartyId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM pending_commit WHERE party_id = ?1",
                [party.as_bytes()],
            )?;
            Ok(())
        })
    }
}

/// Repository for `profile_waiting`.
pub struct ProfilesWaiting<'session> {
    session: &'session Session,
}

impl ProfilesWaiting<'_> {
    /// Marks the Party's profile as waiting.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party does not exist.
    pub fn mark(&self, party: &PartyId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR IGNORE INTO profile_waiting (party_id) VALUES (?1)",
                [party.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// Clears the mark; clearing none is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn clear(&self, party: &PartyId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM profile_waiting WHERE party_id = ?1",
                [party.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// Whether the Party's profile waits.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the read fails.
    pub fn is_waiting(&self, party: &PartyId) -> Result<bool, StoreError> {
        self.session.with(|connection| {
            Ok(connection
                .query_row(
                    "SELECT 1 FROM profile_waiting WHERE party_id = ?1",
                    [party.as_bytes()],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::{Fixture, TestError, party};

    fn roster(keys: &[u8]) -> Roster {
        Roster::new(
            keys.iter()
                .zip(0_u32..)
                .map(|(fill, leaf)| RosterEntry {
                    leaf: LeafIndex::new(leaf),
                    signature_key: AccountPublicKey::from_bytes([*fill; 32]),
                })
                .collect(),
        )
    }

    #[test]
    fn configs_round_trip_with_relays_in_order() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        assert_eq!(session.party_configs().get(&owner.id)?, None);
        // A Party lives on one server, kept in its row: a configuration naming another is refused.
        let elsewhere = PartyConfig {
            server: ServerId::new("https://elsewhere.example".into())?,
            suite: PartySuite::Classical,
            relays: RelayList::ServerDefault,
        };
        assert!(matches!(
            session.party_configs().put(&owner.id, &elsewhere),
            Err(StoreError::ServerMismatch)
        ));
        assert_eq!(session.party_configs().get(&owner.id)?, None);
        let custom = PartyConfig {
            server: owner.server.clone(),
            suite: PartySuite::QuantumResistant,
            relays: RelayList::Custom(vec![
                RelayUrl::new("https://b.example".into())?,
                RelayUrl::new("https://a.example".into())?,
            ]),
        };
        session.party_configs().put(&owner.id, &custom)?;
        assert_eq!(
            session.party_configs().get(&owner.id)?,
            Some(custom.clone())
        );
        let default = PartyConfig {
            suite: PartySuite::Classical,
            relays: RelayList::ServerDefault,
            ..custom
        };
        session.party_configs().put(&owner.id, &default)?;
        assert_eq!(session.party_configs().get(&owner.id)?, Some(default));
        assert_eq!(fixture.rows("party_relay")?, 0);
        Ok(())
    }

    #[test]
    fn rosters_list_newest_first_and_prune() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        let rosters = session.rosters();
        for (epoch, keys) in [(0, &[1_u8][..]), (2, &[1, 2]), (1, &[1, 3])] {
            rosters.record(&owner.id, Epoch::new(epoch), &roster(keys))?;
        }
        assert_eq!(
            rosters.epochs(&owner.id)?,
            vec![Epoch::new(2), Epoch::new(1), Epoch::new(0)]
        );
        assert_eq!(rosters.at(&owner.id, Epoch::new(1))?, Some(roster(&[1, 3])));
        // Recording an epoch again replaces its roster; an empty roster is still a roster.
        rosters.record(&owner.id, Epoch::new(1), &roster(&[4]))?;
        assert_eq!(rosters.at(&owner.id, Epoch::new(1))?, Some(roster(&[4])));
        rosters.record(&owner.id, Epoch::new(3), &Roster::default())?;
        assert_eq!(
            rosters.at(&owner.id, Epoch::new(3))?,
            Some(Roster::default())
        );
        rosters.retain_newest(&owner.id, 2)?;
        assert_eq!(
            rosters.epochs(&owner.id)?,
            vec![Epoch::new(3), Epoch::new(2)]
        );
        assert_eq!(rosters.at(&owner.id, Epoch::new(1))?, None);
        assert_eq!(fixture.rows("roster_entry")?, 2);
        Ok(())
    }

    #[test]
    fn subscriptions_are_set_replaced_and_forgotten() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        let subscriptions = session.member_subscriptions();
        let (alice, bob) = (
            AccountPublicKey::from_bytes([1; 32]),
            AccountPublicKey::from_bytes([2; 32]),
        );
        subscriptions.set(&owner.id, &bob, Some(SubscriptionId::from_bytes([5; 16])))?;
        subscriptions.set(&owner.id, &alice, Some(SubscriptionId::from_bytes([6; 16])))?;
        subscriptions.set(&owner.id, &bob, Some(SubscriptionId::from_bytes([7; 16])))?;
        assert_eq!(
            subscriptions.get(&owner.id, &bob)?,
            Some(SubscriptionId::from_bytes([7; 16]))
        );
        let listed: Vec<AccountPublicKey> = subscriptions
            .list(&owner.id)?
            .iter()
            .map(|entry| entry.key)
            .collect();
        assert_eq!(listed, vec![alice, bob]);
        subscriptions.set(&owner.id, &bob, None)?;
        assert_eq!(subscriptions.get(&owner.id, &bob)?, None);
        assert_eq!(subscriptions.list(&owner.id)?.len(), 1);
        Ok(())
    }

    #[test]
    fn pending_commits_and_profile_marks_come_and_go() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        assert_eq!(session.pending_commits().get(&owner.id)?, None);
        session.pending_commits().put(&owner.id, &[1, 2])?;
        session.pending_commits().put(&owner.id, &[3])?;
        assert_eq!(session.pending_commits().get(&owner.id)?, Some(vec![3]));
        session.pending_commits().delete(&owner.id)?;
        session.pending_commits().delete(&owner.id)?;
        assert_eq!(session.pending_commits().get(&owner.id)?, None);
        assert!(!session.profiles_waiting().is_waiting(&owner.id)?);
        session.profiles_waiting().mark(&owner.id)?;
        session.profiles_waiting().mark(&owner.id)?;
        assert!(session.profiles_waiting().is_waiting(&owner.id)?);
        session.profiles_waiting().clear(&owner.id)?;
        assert!(!session.profiles_waiting().is_waiting(&owner.id)?);
        Ok(())
    }

    #[test]
    fn every_row_goes_with_its_party() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let (gone, kept) = (party(1)?, party(2)?);
        for owner in [&gone, &kept] {
            session.parties().insert(owner)?;
            session.party_configs().put(
                &owner.id,
                &PartyConfig {
                    server: owner.server.clone(),
                    suite: PartySuite::Classical,
                    relays: RelayList::Custom(vec![RelayUrl::new("https://r.example".into())?]),
                },
            )?;
            session
                .rosters()
                .record(&owner.id, Epoch::new(0), &roster(&[1]))?;
            session.member_subscriptions().set(
                &owner.id,
                &AccountPublicKey::from_bytes([1; 32]),
                Some(SubscriptionId::from_bytes([1; 16])),
            )?;
            session.pending_commits().put(&owner.id, &[1])?;
            session.profiles_waiting().mark(&owner.id)?;
        }
        session.parties().delete(&gone.id)?;
        for table in [
            "party_config",
            "party_relay",
            "roster_epoch",
            "roster_entry",
            "member_subscription",
            "pending_commit",
            "profile_waiting",
        ] {
            assert_eq!(fixture.rows(table)?, 1, "{table}");
        }
        // Writing for a Party that does not exist fails.
        assert!(session.pending_commits().put(&gone.id, &[1]).is_err());
        Ok(())
    }
}
