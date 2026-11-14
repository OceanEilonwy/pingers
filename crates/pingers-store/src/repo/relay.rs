// SPDX-License-Identifier: AGPL-3.0-only
//! `push_relay`, `fallback` and `settings`.

use pingers_crypto::PushRelayPublicKey;
use pingers_proto::{KeyConfigHash, RelayId, RelayUrl};
use pingers_time::UnixSeconds;
use rusqlite::{OptionalExtension as _, Row, params};

use crate::codec::{narrow, read_array, read_time, time};
use crate::repo::expect_one;
use crate::repo::party::LocationVisibility;
use crate::server::Scope;
use crate::{FallbackReason, OperatorName, ServerSession, Session, StoreError};

impl ServerSession<'_> {
    /// The push relays this server lists that this device seals tokens to (`push_relay`).
    #[must_use]
    pub fn push_relays(&self) -> PushRelays<'_> {
        PushRelays {
            session: self.scope(),
        }
    }

    /// Privacy fallbacks active for this server, shown in "What your server sees"
    /// (`fallback` table).
    #[must_use]
    pub fn fallbacks(&self) -> Fallbacks<'_> {
        Fallbacks {
            session: self.scope(),
        }
    }
}

impl Session {
    /// Device settings (`settings` table).
    #[must_use]
    pub fn settings(&self) -> Settings<'_> {
        Settings { session: self }
    }
}

/// Where a push relay entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RelaySource {
    /// The server's advertised list.
    Server,
    /// The user's own override.
    Override,
}

/// A push relay (design C.2 `push_relay`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRelay {
    /// Its id in the server's list.
    pub id: RelayId,
    /// Where it is.
    pub url: RelayUrl,
    /// Its pinned HPKE key, from the key config.
    pub key: PushRelayPublicKey,
    /// The hash of the key config it was pinned from.
    pub key_config_hash: KeyConfigHash,
    /// Who runs it.
    pub operator: OperatorName,
    /// Server list or user override.
    pub source: RelaySource,
}

/// Repository for the `push_relay` table.
pub struct PushRelays<'session> {
    session: Scope<'session>,
}

const fn source_code(source: RelaySource) -> i64 {
    match source {
        RelaySource::Server => 0,
        RelaySource::Override => 1,
    }
}

fn relay_from_row(row: &Row<'_>) -> Result<PushRelay, StoreError> {
    Ok(PushRelay {
        id: RelayId::new(narrow(row.get(0)?, "push_relay.relay_id")?),
        url: RelayUrl::new(row.get(1)?).map_err(|_| StoreError::Corrupt {
            field: "push_relay.url",
        })?,
        key: PushRelayPublicKey::new(
            narrow(row.get(2)?, "push_relay.key_id")?,
            read_array(row, 3, "push_relay.public_key")?,
        ),
        key_config_hash: KeyConfigHash::from_bytes(read_array(
            row,
            4,
            "push_relay.key_config_hash",
        )?),
        operator: OperatorName::new(row.get(5)?).map_err(|_| StoreError::Corrupt {
            field: "push_relay.operator",
        })?,
        source: match row.get::<_, i64>(6)? {
            0 => RelaySource::Server,
            1 => RelaySource::Override,
            _ => {
                return Err(StoreError::Corrupt {
                    field: "push_relay.source",
                });
            }
        },
    })
}

impl PushRelays<'_> {
    /// Stores a relay, replacing the entry with the same id.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn put(&self, relay: &PushRelay) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO push_relay (relay_id, url, key_id, public_key, \
                 key_config_hash, operator, source, server_key) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    relay.id.get(),
                    relay.url.as_str(),
                    relay.key.key_id(),
                    relay.key.public_key(),
                    relay.key_config_hash.as_bytes(),
                    relay.operator.as_str(),
                    source_code(relay.source),
                    self.session.key(),
                ],
            )?;
            Ok(())
        })
    }

    /// Every relay, by id.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self) -> Result<Vec<PushRelay>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT relay_id, url, key_id, public_key, key_config_hash, operator, source \
                 FROM push_relay WHERE server_key = ?1 ORDER BY relay_id",
            )?;
            let rows = statement.query_map([self.session.key()], |row| Ok(relay_from_row(row)))?;
            rows.map(|row| row?).collect()
        })
    }

    /// Deletes every relay from `source`, for example before storing a fresh server list;
    /// returns how many.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete_from(&self, source: RelaySource) -> Result<usize, StoreError> {
        self.session.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM push_relay WHERE source = ?1 AND server_key = ?2",
                params![source_code(source), self.session.key()],
            )?)
        })
    }
}

/// A privacy fallback the app may have to use (design C.2 `fallback`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FallbackKind {
    /// Privacy Pass instead of membership proofs.
    PrivacyPass,
    /// A direct connection instead of the OHTTP relay.
    Direct,
    /// A different knock route.
    KnockRoute,
    /// The server and a relay are run by the same operator.
    SameOperator,
}

impl FallbackKind {
    const fn code(self) -> i64 {
        match self {
            Self::PrivacyPass => 0,
            Self::Direct => 1,
            Self::KnockRoute => 2,
            Self::SameOperator => 3,
        }
    }
}

/// A fallback that is active, why, and since when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveFallback {
    /// Which fallback.
    pub kind: FallbackKind,
    /// Why.
    pub reason: FallbackReason,
    /// Since when.
    pub since: UnixSeconds,
}

/// Repository for the `fallback` table: at most one row per kind.
pub struct Fallbacks<'session> {
    session: Scope<'session>,
}

impl Fallbacks<'_> {
    /// Marks a fallback active, replacing its previous reason and time.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn activate(&self, fallback: &ActiveFallback) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO fallback (kind, reason, since, server_key) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    fallback.kind.code(),
                    fallback.reason.as_str(),
                    time(fallback.since, "fallback.since")?,
                    self.session.key(),
                ],
            )?;
            Ok(())
        })
    }

    /// Marks a fallback inactive.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if it was not active.
    pub fn clear(&self, kind: FallbackKind) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "DELETE FROM fallback WHERE kind = ?1 AND server_key = ?2",
                    params![kind.code(), self.session.key()],
                )?,
                "fallback",
            )
        })
    }

    /// The active fallbacks, in kind order.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self) -> Result<Vec<ActiveFallback>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT kind, reason, since FROM fallback WHERE server_key = ?1 ORDER BY kind",
            )?;
            let rows = statement.query_map([self.session.key()], |row| {
                Ok((|| {
                    Ok(ActiveFallback {
                        kind: match row.get::<_, i64>(0)? {
                            0 => FallbackKind::PrivacyPass,
                            1 => FallbackKind::Direct,
                            2 => FallbackKind::KnockRoute,
                            3 => FallbackKind::SameOperator,
                            _ => {
                                return Err(StoreError::Corrupt {
                                    field: "fallback.kind",
                                });
                            }
                        },
                        reason: FallbackReason::new(row.get(1)?).map_err(|_| {
                            StoreError::Corrupt {
                                field: "fallback.reason",
                            }
                        })?,
                        since: read_time(row, 2, "fallback.since")?,
                    })
                })())
            })?;
            rows.map(|row| row?).collect()
        })
    }
}

/// Whether this device seals push tokens at all (design B.7, "No wake-up pushes").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WakePushes {
    /// Seal tokens so the push relay can wake the device (the default).
    Enabled,
    /// Seal no token anywhere; read mailboxes and logs when the app wakes by itself.
    Disabled,
}

/// Repository for the `settings` table, with one typed accessor per setting. Keys and value
/// encodings are private to this module.
pub struct Settings<'session> {
    session: &'session Session,
}

const LOCATION_VISIBILITY: &str = "location_visibility";
const WAKE_PUSHES: &str = "wake_pushes";

impl Settings<'_> {
    fn read(&self, key: &str) -> Result<Option<i64>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row("SELECT v FROM settings WHERE k = ?1", [key], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .optional()?
                .map(|value| match value.as_slice() {
                    [code] => Ok(i64::from(*code)),
                    _ => Err(StoreError::Corrupt {
                        field: "settings.v",
                    }),
                })
                .transpose()
        })
    }

    fn write(&self, key: &str, code: u8) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO settings (k, v) VALUES (?1, ?2)",
                params![key, [code]],
            )?;
            Ok(())
        })
    }

    /// The device's location visibility; [`LocationVisibility::AskEachTime`] until set.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if the stored value does not decode.
    pub fn location_visibility(&self) -> Result<LocationVisibility, StoreError> {
        self.read(LOCATION_VISIBILITY)?
            .map_or(Ok(LocationVisibility::AskEachTime), |code| {
                LocationVisibility::from_code(code, "settings.location_visibility")
            })
    }

    /// Sets the device's location visibility.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn set_location_visibility(
        &self,
        visibility: LocationVisibility,
    ) -> Result<(), StoreError> {
        self.write(LOCATION_VISIBILITY, visibility.code())
    }

    /// Whether wake-up pushes are on; [`WakePushes::Enabled`] until set.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if the stored value does not decode.
    pub fn wake_pushes(&self) -> Result<WakePushes, StoreError> {
        match self.read(WAKE_PUSHES)? {
            None | Some(0) => Ok(WakePushes::Enabled),
            Some(1) => Ok(WakePushes::Disabled),
            Some(_) => Err(StoreError::Corrupt {
                field: "settings.wake_pushes",
            }),
        }
    }

    /// Turns wake-up pushes on or off.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn set_wake_pushes(&self, setting: WakePushes) -> Result<(), StoreError> {
        let code = match setting {
            WakePushes::Enabled => 0,
            WakePushes::Disabled => 1,
        };
        self.write(WAKE_PUSHES, code)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::{Fixture, TestError};

    fn relay(id: u16, source: RelaySource) -> Result<PushRelay, TestError> {
        let fill = u8::try_from(id).unwrap_or(0);
        Ok(PushRelay {
            id: RelayId::new(id),
            url: RelayUrl::new(format!("https://relay{id}.example"))?,
            key: PushRelayPublicKey::new(fill, [fill; 32]),
            key_config_hash: KeyConfigHash::from_bytes([fill; 32]),
            operator: OperatorName::new(format!("Operator {id}"))?,
            source,
        })
    }

    #[test]
    fn relays_round_trip_and_are_replaced_by_source() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let relays = server.push_relays();
        let server = relay(2, RelaySource::Server)?;
        let custom = relay(1, RelaySource::Override)?;
        relays.put(&server)?;
        relays.put(&custom)?;
        assert_eq!(relays.list()?, vec![custom.clone(), server.clone()]);
        let rekeyed = PushRelay {
            key: PushRelayPublicKey::new(9, [9; 32]),
            ..server
        };
        relays.put(&rekeyed)?;
        assert_eq!(relays.list()?, vec![custom.clone(), rekeyed]);
        assert_eq!(relays.delete_from(RelaySource::Server)?, 1);
        assert_eq!(relays.list()?, vec![custom]);
        Ok(())
    }

    #[test]
    fn fallbacks_activate_and_clear() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let fallbacks = server.fallbacks();
        let kinds = [
            FallbackKind::SameOperator,
            FallbackKind::KnockRoute,
            FallbackKind::Direct,
            FallbackKind::PrivacyPass,
        ];
        for (offset, kind) in (0_u64..).zip(kinds) {
            fallbacks.activate(&ActiveFallback {
                kind,
                reason: FallbackReason::new("relay-unreachable".into())?,
                since: UnixSeconds::new(10 + offset),
            })?;
        }
        let listed = fallbacks.list()?;
        let listed_kinds: Vec<_> = listed.iter().map(|fallback| fallback.kind).collect();
        assert_eq!(
            listed_kinds,
            vec![
                FallbackKind::PrivacyPass,
                FallbackKind::Direct,
                FallbackKind::KnockRoute,
                FallbackKind::SameOperator
            ]
        );
        assert_eq!(
            listed.first().map(|fallback| fallback.since),
            Some(UnixSeconds::new(13))
        );
        let updated = ActiveFallback {
            kind: FallbackKind::Direct,
            reason: FallbackReason::new("no-relay".into())?,
            since: UnixSeconds::new(99),
        };
        fallbacks.activate(&updated)?;
        fallbacks.clear(FallbackKind::PrivacyPass)?;
        assert_eq!(fallbacks.list()?.first(), Some(&updated));
        assert_eq!(fallbacks.list()?.len(), 3);
        assert!(matches!(
            fallbacks.clear(FallbackKind::PrivacyPass),
            Err(StoreError::NotFound { table: "fallback" })
        ));
        Ok(())
    }

    #[test]
    fn settings_default_until_set() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let settings = fixture.session.settings();
        assert_eq!(
            settings.location_visibility()?,
            LocationVisibility::AskEachTime
        );
        assert_eq!(settings.wake_pushes()?, WakePushes::Enabled);
        for visibility in [
            LocationVisibility::Everyone,
            LocationVisibility::Nobody,
            LocationVisibility::AskEachTime,
        ] {
            settings.set_location_visibility(visibility)?;
            assert_eq!(settings.location_visibility()?, visibility);
        }
        settings.set_wake_pushes(WakePushes::Disabled)?;
        assert_eq!(settings.wake_pushes()?, WakePushes::Disabled);
        settings.set_wake_pushes(WakePushes::Enabled)?;
        assert_eq!(settings.wake_pushes()?, WakePushes::Enabled);
        // The settings are independent keys.
        settings.set_location_visibility(LocationVisibility::Nobody)?;
        assert_eq!(settings.wake_pushes()?, WakePushes::Enabled);
        Ok(())
    }

    #[test]
    fn unknown_setting_values_are_corrupt() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        fixture.session.with(|connection| {
            connection.execute(
                "INSERT INTO settings (k, v) VALUES ('wake_pushes', x'07'), \
                 ('location_visibility', x'0000')",
                [],
            )?;
            Ok(())
        })?;
        assert!(fixture.session.settings().wake_pushes().is_err());
        assert!(fixture.session.settings().location_visibility().is_err());
        Ok(())
    }
}
