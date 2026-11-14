// SPDX-License-Identifier: AGPL-3.0-only
//! `spot`, `presence` and `exclusion_zone`.

use pingers_crypto::AccountPublicKey;
use pingers_geo::{Fence, PresenceSnapshot, PresenceState};
use pingers_proto::{Lamport, PartyId, SpotId};
use rusqlite::{OptionalExtension as _, Row, params};

use crate::codec::{
    decode_fence, encode_fence, from_int, read_array, read_optional_time, read_time, time, to_int,
};
use crate::repo::expect_one;
use crate::{Session, SpotName, StoreError, ZoneId, ZoneName};

impl Session {
    /// Spots (`spot` table).
    #[must_use]
    pub fn spots(&self) -> Spots<'_> {
        Spots { session: self }
    }

    /// Per-Spot presence state (`presence` table).
    #[must_use]
    pub fn presence(&self) -> Presence<'_> {
        Presence { session: self }
    }

    /// Exclusion zones (`exclusion_zone` table).
    #[must_use]
    pub fn exclusion_zones(&self) -> ExclusionZones<'_> {
        ExclusionZones { session: self }
    }
}

/// Whether a Spot exists or has been deleted. Deleted Spots stay as tombstones so a late edit
/// with a lower Lamport clock cannot resurrect them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpotStatus {
    /// In use.
    Active,
    /// Deleted by a member.
    Deleted,
}

/// A Spot (design C.2 `spot`): a Party's named place.
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    /// The Spot's id.
    pub id: SpotId,
    /// The Party it belongs to.
    pub party: PartyId,
    /// Its name.
    pub name: SpotName,
    /// Its circle or polygon.
    pub fence: Fence,
    /// The member who created it.
    pub created_by: AccountPublicKey,
    /// The Lamport clock of its last edit (last writer wins).
    pub lamport: Lamport,
    /// Active or a tombstone.
    pub status: SpotStatus,
}

/// Repository for the `spot` table.
pub struct Spots<'session> {
    session: &'session Session,
}

const SPOT_COLUMNS: &str = "spot_id, party_id, name, geometry, created_by, lamport, deleted";

fn spot_from_row(row: &Row<'_>) -> Result<Spot, StoreError> {
    let geometry: Vec<u8> = row.get(3)?;
    Ok(Spot {
        id: SpotId::from_bytes(read_array(row, 0, "spot.spot_id")?),
        party: PartyId::from_bytes(read_array(row, 1, "spot.party_id")?),
        name: SpotName::new(row.get(2)?).map_err(|_| StoreError::Corrupt { field: "spot.name" })?,
        fence: decode_fence(&geometry, "spot.geometry")?,
        created_by: AccountPublicKey::from_bytes(read_array(row, 4, "spot.created_by")?),
        lamport: Lamport::new(from_int(row.get(5)?, "spot.lamport")?),
        status: match row.get::<_, i64>(6)? {
            0 => SpotStatus::Active,
            1 => SpotStatus::Deleted,
            _ => {
                return Err(StoreError::Corrupt {
                    field: "spot.deleted",
                });
            }
        },
    })
}

impl Spots<'_> {
    /// Stores a Spot, replacing any previous version with the same id. Which version wins is
    /// the caller's decision (pingers-parties compares Lamport clocks).
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party does not exist; [`StoreError::OutOfRange`] if the
    /// Lamport clock or vertex count does not fit.
    pub fn put(&self, spot: &Spot) -> Result<(), StoreError> {
        let geometry = encode_fence(&spot.fence)?;
        self.session.with(|connection| {
            connection.execute(
                &format!(
                    // An upsert, not REPLACE: REPLACE deletes the row first, and the delete
                    // would cascade to the Spot's presence state on every edit.
                    "INSERT INTO spot ({SPOT_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                     ON CONFLICT (spot_id) DO UPDATE SET party_id = excluded.party_id, \
                     name = excluded.name, geometry = excluded.geometry, \
                     created_by = excluded.created_by, lamport = excluded.lamport, \
                     deleted = excluded.deleted"
                ),
                params![
                    spot.id.as_bytes(),
                    spot.party.as_bytes(),
                    spot.name.as_str(),
                    geometry,
                    spot.created_by.as_bytes(),
                    to_int(spot.lamport.get(), "spot.lamport")?,
                    match spot.status {
                        SpotStatus::Active => 0,
                        SpotStatus::Deleted => 1,
                    },
                ],
            )?;
            Ok(())
        })
    }

    /// The Spot with this id, tombstone or not.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, id: &SpotId) -> Result<Option<Spot>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!("SELECT {SPOT_COLUMNS} FROM spot WHERE spot_id = ?1"),
                    [id.as_bytes()],
                    |row| Ok(spot_from_row(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// The Party's Spots, including tombstones, by id.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self, party: &PartyId) -> Result<Vec<Spot>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {SPOT_COLUMNS} FROM spot WHERE party_id = ?1 ORDER BY spot_id"
            ))?;
            let rows = statement.query_map([party.as_bytes()], |row| Ok(spot_from_row(row)))?;
            rows.map(|row| row?).collect()
        })
    }
}

/// One Spot's stored presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpotPresenceRecord {
    /// The Spot.
    pub spot: SpotId,
    /// Its state machine snapshot.
    pub snapshot: PresenceSnapshot,
}

/// Repository for the `presence` table: the Spot state machine's snapshot per Spot, so it
/// survives a restart. No coordinates: only the state, since when, and when the last fix was
/// processed (design C.2).
pub struct Presence<'session> {
    session: &'session Session,
}

const fn state_code(state: PresenceState) -> i64 {
    match state {
        PresenceState::Outside => 0,
        PresenceState::Candidate => 1,
        PresenceState::Inside => 2,
        PresenceState::Suppressed => 3,
    }
}

fn presence_from_row(row: &Row<'_>) -> Result<SpotPresenceRecord, StoreError> {
    Ok(SpotPresenceRecord {
        spot: SpotId::from_bytes(read_array(row, 0, "presence.spot_id")?),
        snapshot: PresenceSnapshot {
            state: match row.get::<_, i64>(1)? {
                0 => PresenceState::Outside,
                1 => PresenceState::Candidate,
                2 => PresenceState::Inside,
                3 => PresenceState::Suppressed,
                _ => {
                    return Err(StoreError::Corrupt {
                        field: "presence.state",
                    });
                }
            },
            since: read_time(row, 2, "presence.since")?,
            last_fix_at: read_optional_time(row, 3, "presence.last_fix_at")?,
            confirmed_at: read_optional_time(row, 4, "presence.confirmed_at")?,
        },
    })
}

impl Presence<'_> {
    /// Stores a Spot's snapshot, replacing the previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Spot does not exist.
    pub fn save(&self, spot: &SpotId, snapshot: &PresenceSnapshot) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO presence \
                 (spot_id, state, since, last_fix_at, confirmed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    spot.as_bytes(),
                    state_code(snapshot.state),
                    time(snapshot.since, "presence.since")?,
                    snapshot
                        .last_fix_at
                        .map(|at| time(at, "presence.last_fix_at"))
                        .transpose()?,
                    snapshot
                        .confirmed_at
                        .map(|at| time(at, "presence.confirmed_at"))
                        .transpose()?,
                ],
            )?;
            Ok(())
        })
    }

    /// A Spot's snapshot, if one was saved.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn load(&self, spot: &SpotId) -> Result<Option<PresenceSnapshot>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT spot_id, state, since, last_fix_at, confirmed_at FROM presence \
                     WHERE spot_id = ?1",
                    [spot.as_bytes()],
                    |row| Ok(presence_from_row(row)),
                )
                .optional()?
                .transpose()
                .map(|record| record.map(|record| record.snapshot))
        })
    }

    /// Every saved snapshot, by Spot id.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self) -> Result<Vec<SpotPresenceRecord>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT spot_id, state, since, last_fix_at, confirmed_at FROM presence \
                 ORDER BY spot_id",
            )?;
            let rows = statement.query_map([], |row| Ok(presence_from_row(row)))?;
            rows.map(|row| row?).collect()
        })
    }

    /// Forgets a Spot's snapshot (for example when the Spot is deleted); forgetting an absent
    /// one is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn clear(&self, spot: &SpotId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute("DELETE FROM presence WHERE spot_id = ?1", [spot.as_bytes()])?;
            Ok(())
        })
    }
}

/// An exclusion zone (design B.11): local only, never shared.
#[derive(Clone, Debug, PartialEq)]
pub struct ExclusionZone {
    /// The zone's id.
    pub id: ZoneId,
    /// Its name.
    pub name: ZoneName,
    /// Its circle or polygon.
    pub fence: Fence,
}

/// Repository for the `exclusion_zone` table.
pub struct ExclusionZones<'session> {
    session: &'session Session,
}

impl ExclusionZones<'_> {
    /// Stores a zone, replacing any previous version with the same id.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if a polygon has more than 65,535 vertices;
    /// [`StoreError::Database`] if the write fails.
    pub fn put(&self, zone: &ExclusionZone) -> Result<(), StoreError> {
        let geometry = encode_fence(&zone.fence)?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO exclusion_zone (zone_id, name, geometry) \
                 VALUES (?1, ?2, ?3)",
                params![zone.id.as_bytes(), zone.name.as_str(), geometry],
            )?;
            Ok(())
        })
    }

    /// Every zone, by id.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self) -> Result<Vec<ExclusionZone>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection
                .prepare("SELECT zone_id, name, geometry FROM exclusion_zone ORDER BY zone_id")?;
            let rows = statement.query_map([], |row| {
                Ok((|| {
                    let geometry: Vec<u8> = row.get(2)?;
                    Ok(ExclusionZone {
                        id: ZoneId::from_bytes(read_array(row, 0, "exclusion_zone.zone_id")?),
                        name: ZoneName::new(row.get(1)?).map_err(|_| StoreError::Corrupt {
                            field: "exclusion_zone.name",
                        })?,
                        fence: decode_fence(&geometry, "exclusion_zone.geometry")?,
                    })
                })())
            })?;
            rows.map(|row| row?).collect()
        })
    }

    /// Deletes a zone.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such zone.
    pub fn delete(&self, id: &ZoneId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "DELETE FROM exclusion_zone WHERE zone_id = ?1",
                    [id.as_bytes()],
                )?,
                "exclusion_zone",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_time::UnixSeconds;

    use super::*;
    use crate::test_support::{Fixture, TestError, circle, party};

    pub(crate) fn spot(party: &PartyId, fill: u8) -> Result<Spot, TestError> {
        Ok(Spot {
            id: SpotId::from_bytes([fill; 16]),
            party: *party,
            name: SpotName::new(format!("Spot {fill}"))?,
            fence: circle(48.0 + f64::from(fill), 2.0, 150.0)?,
            created_by: AccountPublicKey::from_bytes([fill; 32]),
            lamport: Lamport::new(u64::from(fill)),
            status: SpotStatus::Active,
        })
    }

    #[test]
    fn spots_round_trip_per_party() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let first = party(1)?;
        let second = party(2)?;
        session.parties().insert(&first)?;
        session.parties().insert(&second)?;
        let mut tombstone = spot(&first.id, 4)?;
        tombstone.status = SpotStatus::Deleted;
        let active = spot(&first.id, 3)?;
        let elsewhere = spot(&second.id, 5)?;
        for each in [&tombstone, &active, &elsewhere] {
            session.spots().put(each)?;
        }
        assert_eq!(
            session.spots().list(&first.id)?,
            vec![active.clone(), tombstone]
        );
        assert_eq!(session.spots().get(&active.id)?, Some(active.clone()));
        assert_eq!(session.spots().get(&SpotId::from_bytes([0; 16]))?, None);
        let edited = Spot {
            lamport: Lamport::new(99),
            name: SpotName::new("Edited".into())?,
            ..active
        };
        session.spots().put(&edited)?;
        assert_eq!(session.spots().get(&edited.id)?, Some(edited));
        Ok(())
    }

    #[test]
    fn a_spot_lamport_past_i64_is_refused() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let owner = party(1)?;
        fixture.session.parties().insert(&owner)?;
        let mut far = spot(&owner.id, 1)?;
        far.lamport = Lamport::new(u64::MAX);
        assert!(matches!(
            fixture.session.spots().put(&far),
            Err(StoreError::OutOfRange {
                field: "spot.lamport"
            })
        ));
        Ok(())
    }

    #[test]
    fn presence_round_trips_and_follows_its_spot() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        let home = spot(&owner.id, 1)?;
        let work = spot(&owner.id, 2)?;
        session.spots().put(&home)?;
        session.spots().put(&work)?;
        let inside = PresenceSnapshot {
            state: PresenceState::Inside,
            since: UnixSeconds::new(100),
            last_fix_at: Some(UnixSeconds::new(130)),
            confirmed_at: Some(UnixSeconds::new(120)),
        };
        let states = [
            PresenceState::Outside,
            PresenceState::Candidate,
            PresenceState::Inside,
            PresenceState::Suppressed,
        ];
        for state in states {
            let snapshot = PresenceSnapshot {
                state,
                since: UnixSeconds::new(5),
                last_fix_at: None,
                confirmed_at: None,
            };
            session.presence().save(&work.id, &snapshot)?;
            assert_eq!(session.presence().load(&work.id)?, Some(snapshot));
        }
        session.presence().save(&home.id, &inside)?;
        assert_eq!(session.presence().load(&home.id)?, Some(inside));
        // Editing the Spot keeps its presence state.
        let mut edited = home.clone();
        edited.lamport = Lamport::new(home.lamport.get().saturating_add(1));
        session.spots().put(&edited)?;
        assert_eq!(session.presence().load(&home.id)?, Some(inside));
        assert_eq!(session.spots().get(&home.id)?, Some(edited));
        assert_eq!(session.presence().list()?.len(), 2);
        session.presence().clear(&work.id)?;
        assert_eq!(session.presence().load(&work.id)?, None);
        assert_eq!(
            session.presence().list()?,
            vec![SpotPresenceRecord {
                spot: home.id,
                snapshot: inside
            }]
        );
        session.parties().delete(&owner.id)?;
        assert_eq!(fixture.rows("presence")?, 0);
        assert_eq!(fixture.rows("spot")?, 0);
        Ok(())
    }

    #[test]
    fn zones_round_trip_and_delete() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let zones = fixture.session.exclusion_zones();
        let home = ExclusionZone {
            id: ZoneId::from_bytes([2; 16]),
            name: ZoneName::new("Home".into())?,
            fence: circle(51.5, -0.12, 300.0)?,
        };
        let office = ExclusionZone {
            id: ZoneId::from_bytes([1; 16]),
            name: ZoneName::new("Office".into())?,
            fence: circle(51.6, -0.1, 100.0)?,
        };
        zones.put(&home)?;
        zones.put(&office)?;
        assert_eq!(zones.list()?, vec![office.clone(), home.clone()]);
        zones.delete(&office.id)?;
        assert_eq!(zones.list()?, vec![home]);
        assert!(matches!(
            zones.delete(&office.id),
            Err(StoreError::NotFound {
                table: "exclusion_zone"
            })
        ));
        Ok(())
    }
}
