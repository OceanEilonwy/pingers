// SPDX-License-Identifier: AGPL-3.0-only
//! `meetup`, `meetup_msg`, `meetup_peer`, `meetup_self_lk`, `location_grant` and
//! `location_ask`: everything a live Meetup holds, all deleted when it ends.

use pingers_crypto::{GrantSecretKey, LocationKey};
use pingers_proto::{
    AccuracyMeters, ChatBody, ChatMessageId, E7Coordinate, GroupId, HeadingDegrees, LeafIndex,
    LocationGeneration, LocationPlaintext, LocationRequestId, PartyId, SpeedCentimetresPerSecond,
    SpotId,
};
use pingers_time::UnixSeconds;
use rusqlite::{OptionalExtension as _, Row, params};

use crate::codec::{narrow, read_array, read_optional_array, read_secret, read_time, time};
use crate::repo::expect_one;
use crate::{Session, StoreError};

impl Session {
    /// Live Meetups (`meetup` table). Ending one deletes everything below.
    #[must_use]
    pub fn meetups(&self) -> Meetups<'_> {
        Meetups { session: self }
    }

    /// Chat and its threads in live Meetups (`meetup_msg` table).
    #[must_use]
    pub fn meetup_messages(&self) -> MeetupMessages<'_> {
        MeetupMessages { session: self }
    }

    /// Other members of live Meetups (`meetup_peer` table).
    #[must_use]
    pub fn meetup_peers(&self) -> MeetupPeers<'_> {
        MeetupPeers { session: self }
    }

    /// This device's location keys per Meetup and generation (`meetup_self_lk` table).
    #[must_use]
    pub fn self_location_keys(&self) -> SelfLocationKeys<'_> {
        SelfLocationKeys { session: self }
    }

    /// Who this device shared its location with (`location_grant` table).
    #[must_use]
    pub fn location_grants(&self) -> LocationGrants<'_> {
        LocationGrants { session: self }
    }

    /// Requests to see an exact location, in and out (`location_ask` table).
    #[must_use]
    pub fn location_asks(&self) -> LocationAsks<'_> {
        LocationAsks { session: self }
    }
}

/// A live Meetup (design C.2 `meetup`).
pub struct Meetup {
    /// The Meetup's MLS group.
    pub group: GroupId,
    /// The Party it formed in.
    pub party: PartyId,
    /// The Spot it formed at.
    pub spot: SpotId,
    /// When this device joined it.
    pub started_at: UnixSeconds,
    /// This device's grant secret key for this Meetup (design B.6.1).
    pub grant_key: GrantSecretKey,
}

/// Repository for the `meetup` table.
pub struct Meetups<'session> {
    session: &'session Session,
}

fn meetup_from_row(row: &Row<'_>) -> Result<Meetup, StoreError> {
    Ok(Meetup {
        group: GroupId::from_bytes(read_array(row, 0, "meetup.group_id")?),
        party: PartyId::from_bytes(read_array(row, 1, "meetup.party_id")?),
        spot: SpotId::from_bytes(read_array(row, 2, "meetup.spot_id")?),
        started_at: read_time(row, 3, "meetup.started_at")?,
        grant_key: GrantSecretKey::from_bytes(read_secret(row, 4, "meetup.gk_sk")?),
    })
}

const MEETUP_COLUMNS: &str = "group_id, party_id, spot_id, started_at, gk_sk";

impl Meetups<'_> {
    /// Records a Meetup this device created or joined.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if it already exists or its Party does not.
    pub fn start(&self, meetup: &Meetup) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                &format!("INSERT INTO meetup ({MEETUP_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5)"),
                params![
                    meetup.group.as_bytes(),
                    meetup.party.as_bytes(),
                    meetup.spot.as_bytes(),
                    time(meetup.started_at, "meetup.started_at")?,
                    meetup.grant_key.expose_bytes(),
                ],
            )?;
            Ok(())
        })
    }

    /// The Meetup with this group.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, group: &GroupId) -> Result<Option<Meetup>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!("SELECT {MEETUP_COLUMNS} FROM meetup WHERE group_id = ?1"),
                    [group.as_bytes()],
                    |row| Ok(meetup_from_row(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// Every live Meetup, oldest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self) -> Result<Vec<Meetup>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {MEETUP_COLUMNS} FROM meetup ORDER BY started_at, group_id"
            ))?;
            let rows = statement.query_map([], |row| Ok(meetup_from_row(row)))?;
            rows.map(|row| row?).collect()
        })
    }

    /// Ends a Meetup: deletes its row and, by cascade, its chat, peers (with their location
    /// keys and last positions), this device's location and grant keys, grants and asks, and
    /// every MLS row of its group (epoch secrets, key pairs, roster), as pingers-mls's
    /// `delete_group` would. With `secure_delete` the freed pages are zeroed; outside a
    /// transaction the WAL is then checkpointed and truncated. Inside one, call
    /// [`Session::checkpoint`] after committing.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such Meetup; [`StoreError::Internal`] if the
    /// group's MLS storage prefix cannot be encoded.
    pub fn end(&self, group: &GroupId) -> Result<(), StoreError> {
        let mls_prefix =
            pingers_mls::group_storage_prefix(group).map_err(|_| StoreError::Internal)?;
        let outside_transaction = self.session.with(|connection| {
            expect_one(
                connection.execute("DELETE FROM meetup WHERE group_id = ?1", [group.as_bytes()])?,
                "meetup",
            )?;
            connection.execute(crate::repo::mls::DELETE_PREFIX, [mls_prefix.as_slice()])?;
            Ok(connection.is_autocommit())
        })?;
        if outside_transaction {
            self.session.checkpoint()?;
        }
        Ok(())
    }
}

/// A chat message in a live Meetup (design C.2 `meetup_msg`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredChat {
    /// The Meetup.
    pub group: GroupId,
    /// The message's id.
    pub id: ChatMessageId,
    /// The thread root it replies to, if any.
    pub parent: Option<ChatMessageId>,
    /// Who sent it.
    pub sender: LeafIndex,
    /// The text.
    pub body: ChatBody,
    /// When the sender wrote it.
    pub written_at: UnixSeconds,
}

/// Repository for the `meetup_msg` table.
pub struct MeetupMessages<'session> {
    session: &'session Session,
}

impl MeetupMessages<'_> {
    /// Stores a message; a message already stored (same group and id, for example delivered
    /// twice) is left unchanged.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Meetup does not exist.
    pub fn insert(&self, message: &StoredChat) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR IGNORE INTO meetup_msg (group_id, msg_id, parent, sender_leaf, body, \
                 ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    message.group.as_bytes(),
                    message.id.as_bytes(),
                    message.parent.as_ref().map(ChatMessageId::as_bytes),
                    message.sender.get(),
                    message.body.as_str(),
                    time(message.written_at, "meetup_msg.ts")?,
                ],
            )?;
            Ok(())
        })
    }

    /// The Meetup's messages in the order they were written.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self, group: &GroupId) -> Result<Vec<StoredChat>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT group_id, msg_id, parent, sender_leaf, body, ts FROM meetup_msg \
                 WHERE group_id = ?1 ORDER BY ts, msg_id",
            )?;
            let rows = statement.query_map([group.as_bytes()], |row| {
                Ok((|| {
                    Ok(StoredChat {
                        group: GroupId::from_bytes(read_array(row, 0, "meetup_msg.group_id")?),
                        id: ChatMessageId::from_bytes(read_array(row, 1, "meetup_msg.msg_id")?),
                        parent: read_optional_array(row, 2, "meetup_msg.parent")?
                            .map(ChatMessageId::from_bytes),
                        sender: LeafIndex::new(narrow(row.get(3)?, "meetup_msg.sender_leaf")?),
                        body: ChatBody::new(row.get(4)?).map_err(|_| StoreError::Corrupt {
                            field: "meetup_msg.body",
                        })?,
                        written_at: read_time(row, 5, "meetup_msg.ts")?,
                    })
                })())
            })?;
            rows.map(|row| row?).collect()
        })
    }
}

/// A peer's location key this device was granted (design B.6.1).
pub struct GrantedKey {
    /// The generation.
    pub generation: LocationGeneration,
    /// The peer's location key for it.
    pub key: LocationKey,
    /// When the grant lapses.
    pub expires_at: UnixSeconds,
}

/// Another member of a live Meetup, as this device knows them (design C.2 `meetup_peer`).
pub struct MeetupPeer {
    /// The Meetup.
    pub group: GroupId,
    /// Their leaf.
    pub leaf: LeafIndex,
    /// When a frame from them last arrived.
    pub last_seen: UnixSeconds,
    /// Their location key, if they granted it.
    pub key: Option<GrantedKey>,
    /// Their latest decrypted position. Only ever present while `key` is: the database
    /// refuses a position without a key.
    pub location: Option<LocationPlaintext>,
}

/// Repository for the `meetup_peer` table.
pub struct MeetupPeers<'session> {
    session: &'session Session,
}

const PEER_COLUMNS: &str = "group_id, leaf, last_seen, peer_lk, peer_lk_gen, \
     peer_lk_expires_at, location_lat_e7, location_lon_e7, location_accuracy_m, \
     location_heading_deg, location_speed_cms, location_taken_at";

fn peer_from_row(row: &Row<'_>) -> Result<MeetupPeer, StoreError> {
    let key = match row.get::<_, Option<i64>>(4)? {
        None => None,
        Some(generation) => Some(GrantedKey {
            generation: LocationGeneration::new(narrow(generation, "meetup_peer.peer_lk_gen")?),
            key: LocationKey::from_bytes(read_secret(row, 3, "meetup_peer.peer_lk")?),
            expires_at: read_time(row, 5, "meetup_peer.peer_lk_expires_at")?,
        }),
    };
    // Read as i64 and narrowed here: rusqlite's own out-of-range error would carry the
    // stored coordinate into `StoreError::Database` (style guide §9).
    let location = match row.get::<_, Option<i64>>(6)? {
        None => None,
        Some(latitude) => {
            let corrupt = |_| StoreError::Corrupt {
                field: "meetup_peer.location",
            };
            let latitude = narrow(latitude, "meetup_peer.location_lat_e7")?;
            let longitude = narrow(row.get::<_, i64>(7)?, "meetup_peer.location_lon_e7")?;
            Some(LocationPlaintext {
                position: E7Coordinate::new(latitude, longitude).map_err(corrupt)?,
                accuracy: AccuracyMeters::new(row.get(8)?),
                heading: row
                    .get::<_, Option<u16>>(9)?
                    .map(HeadingDegrees::new)
                    .transpose()
                    .map_err(corrupt)?,
                speed: row
                    .get::<_, Option<u16>>(10)?
                    .map(SpeedCentimetresPerSecond::new),
                taken_at: read_time(row, 11, "meetup_peer.location_taken_at")?,
            })
        }
    };
    Ok(MeetupPeer {
        group: GroupId::from_bytes(read_array(row, 0, "meetup_peer.group_id")?),
        leaf: LeafIndex::new(narrow(row.get(1)?, "meetup_peer.leaf")?),
        last_seen: read_time(row, 2, "meetup_peer.last_seen")?,
        key,
        location,
    })
}

impl MeetupPeers<'_> {
    /// Records that a frame from the peer arrived at `at`, adding the peer if new.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Meetup does not exist.
    pub fn seen(
        &self,
        group: &GroupId,
        leaf: LeafIndex,
        at: UnixSeconds,
    ) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT INTO meetup_peer (group_id, leaf, last_seen) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (group_id, leaf) DO UPDATE SET last_seen = excluded.last_seen",
                params![
                    group.as_bytes(),
                    leaf.get(),
                    time(at, "meetup_peer.last_seen")?
                ],
            )?;
            Ok(())
        })
    }

    /// Stores the peer's location key from a grant, replacing an older one.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if the peer has not been seen.
    pub fn set_key(
        &self,
        group: &GroupId,
        leaf: LeafIndex,
        granted: &GrantedKey,
    ) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE meetup_peer SET peer_lk = ?3, peer_lk_gen = ?4, \
                     peer_lk_expires_at = ?5 WHERE group_id = ?1 AND leaf = ?2",
                    params![
                        group.as_bytes(),
                        leaf.get(),
                        granted.key.expose_bytes(),
                        granted.generation.get(),
                        time(granted.expires_at, "meetup_peer.peer_lk_expires_at")?,
                    ],
                )?,
                "meetup_peer",
            )
        })
    }

    /// Forgets the peer's location key (expired or rotated away) and with it their last
    /// position.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if the peer has not been seen.
    pub fn clear_key(&self, group: &GroupId, leaf: LeafIndex) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE meetup_peer SET peer_lk = NULL, peer_lk_gen = NULL, \
                     peer_lk_expires_at = NULL, location_lat_e7 = NULL, location_lon_e7 = NULL, \
                     location_accuracy_m = NULL, location_heading_deg = NULL, \
                     location_speed_cms = NULL, location_taken_at = NULL \
                     WHERE group_id = ?1 AND leaf = ?2",
                    params![group.as_bytes(), leaf.get()],
                )?,
                "meetup_peer",
            )
        })
    }

    /// Replaces the peer's last position with `location` (overwritten, never appended).
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if the peer has not been seen; [`StoreError::Database`] if
    /// this device does not hold the peer's location key.
    pub fn set_location(
        &self,
        group: &GroupId,
        leaf: LeafIndex,
        location: &LocationPlaintext,
    ) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE meetup_peer SET location_lat_e7 = ?3, location_lon_e7 = ?4, \
                     location_accuracy_m = ?5, location_heading_deg = ?6, \
                     location_speed_cms = ?7, location_taken_at = ?8 \
                     WHERE group_id = ?1 AND leaf = ?2",
                    params![
                        group.as_bytes(),
                        leaf.get(),
                        location.position.latitude_e7(),
                        location.position.longitude_e7(),
                        location.accuracy.get(),
                        location.heading.map(HeadingDegrees::get),
                        location.speed.map(SpeedCentimetresPerSecond::get),
                        time(location.taken_at, "meetup_peer.location_taken_at")?,
                    ],
                )?,
                "meetup_peer",
            )
        })
    }

    /// The peer.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, group: &GroupId, leaf: LeafIndex) -> Result<Option<MeetupPeer>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!(
                        "SELECT {PEER_COLUMNS} FROM meetup_peer WHERE group_id = ?1 AND leaf = ?2"
                    ),
                    params![group.as_bytes(), leaf.get()],
                    |row| Ok(peer_from_row(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// The Meetup's peers by leaf.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self, group: &GroupId) -> Result<Vec<MeetupPeer>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {PEER_COLUMNS} FROM meetup_peer WHERE group_id = ?1 ORDER BY leaf"
            ))?;
            let rows = statement.query_map([group.as_bytes()], |row| Ok(peer_from_row(row)))?;
            rows.map(|row| row?).collect()
        })
    }

    /// Removes a peer who left the Meetup, with their key and position.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if the peer has not been seen.
    pub fn remove(&self, group: &GroupId, leaf: LeafIndex) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "DELETE FROM meetup_peer WHERE group_id = ?1 AND leaf = ?2",
                    params![group.as_bytes(), leaf.get()],
                )?,
                "meetup_peer",
            )
        })
    }
}

/// One generation of this device's location key in a Meetup.
pub struct SelfLocationKey {
    /// The generation.
    pub generation: LocationGeneration,
    /// The key.
    pub key: LocationKey,
}

/// Repository for the `meetup_self_lk` table.
pub struct SelfLocationKeys<'session> {
    session: &'session Session,
}

impl SelfLocationKeys<'_> {
    /// Stores a new generation of this device's location key.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Meetup does not exist or the generation is stored.
    pub fn insert(&self, group: &GroupId, key: &SelfLocationKey) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT INTO meetup_self_lk (group_id, lk_gen, lk) VALUES (?1, ?2, ?3)",
                params![
                    group.as_bytes(),
                    key.generation.get(),
                    key.key.expose_bytes()
                ],
            )?;
            Ok(())
        })
    }

    /// The newest generation, the one frames are sealed under.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn current(&self, group: &GroupId) -> Result<Option<SelfLocationKey>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT lk_gen, lk FROM meetup_self_lk WHERE group_id = ?1 \
                     ORDER BY lk_gen DESC LIMIT 1",
                    [group.as_bytes()],
                    |row| {
                        Ok((|| {
                            Ok(SelfLocationKey {
                                generation: LocationGeneration::new(narrow(
                                    row.get(0)?,
                                    "meetup_self_lk.lk_gen",
                                )?),
                                key: LocationKey::from_bytes(read_secret(
                                    row,
                                    1,
                                    "meetup_self_lk.lk",
                                )?),
                            })
                        })())
                    },
                )
                .optional()?
                .transpose()
        })
    }

    /// Deletes generations older than `oldest_kept` after a rotation; returns how many.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete_before(
        &self,
        group: &GroupId,
        oldest_kept: LocationGeneration,
    ) -> Result<usize, StoreError> {
        self.session.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM meetup_self_lk WHERE group_id = ?1 AND lk_gen < ?2",
                params![group.as_bytes(), oldest_kept.get()],
            )?)
        })
    }
}

/// How long a location grant lasts (design B.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GrantScope {
    /// Until the Meetup ends.
    ThisMeetup,
    /// For 15 minutes.
    FifteenMinutes,
}

/// A grant this device sent (design C.2 `location_grant`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutgoingGrant {
    /// The Meetup.
    pub group: GroupId,
    /// Who was granted.
    pub grantee: LeafIndex,
    /// The generation they hold.
    pub generation: LocationGeneration,
    /// The scope the user chose.
    pub scope: GrantScope,
    /// When it lapses.
    pub expires_at: UnixSeconds,
}

/// Repository for the `location_grant` table: one row per grantee.
pub struct LocationGrants<'session> {
    session: &'session Session,
}

impl LocationGrants<'_> {
    /// Records a grant, replacing the grantee's previous one (after a rotation).
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Meetup does not exist.
    pub fn put(&self, grant: &OutgoingGrant) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO location_grant (group_id, grantee_leaf, lk_gen, scope, \
                 expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    grant.group.as_bytes(),
                    grant.grantee.get(),
                    grant.generation.get(),
                    match grant.scope {
                        GrantScope::ThisMeetup => 0,
                        GrantScope::FifteenMinutes => 1,
                    },
                    time(grant.expires_at, "location_grant.expires_at")?,
                ],
            )?;
            Ok(())
        })
    }

    /// The Meetup's grants by grantee.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self, group: &GroupId) -> Result<Vec<OutgoingGrant>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT group_id, grantee_leaf, lk_gen, scope, expires_at FROM location_grant \
                 WHERE group_id = ?1 ORDER BY grantee_leaf",
            )?;
            let rows = statement.query_map([group.as_bytes()], |row| {
                Ok((|| {
                    Ok(OutgoingGrant {
                        group: GroupId::from_bytes(read_array(row, 0, "location_grant.group_id")?),
                        grantee: LeafIndex::new(narrow(
                            row.get(1)?,
                            "location_grant.grantee_leaf",
                        )?),
                        generation: LocationGeneration::new(narrow(
                            row.get(2)?,
                            "location_grant.lk_gen",
                        )?),
                        scope: match row.get::<_, i64>(3)? {
                            0 => GrantScope::ThisMeetup,
                            1 => GrantScope::FifteenMinutes,
                            _ => {
                                return Err(StoreError::Corrupt {
                                    field: "location_grant.scope",
                                });
                            }
                        },
                        expires_at: read_time(row, 4, "location_grant.expires_at")?,
                    })
                })())
            })?;
            rows.map(|row| row?).collect()
        })
    }

    /// Revokes a grantee (the caller then rotates the key).
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if the member holds no grant.
    pub fn revoke(&self, group: &GroupId, grantee: LeafIndex) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "DELETE FROM location_grant WHERE group_id = ?1 AND grantee_leaf = ?2",
                    params![group.as_bytes(), grantee.get()],
                )?,
                "location_grant",
            )
        })
    }

    /// Deletes the Meetup's grants that lapsed at or before `now`, returning the grantees so
    /// the caller can rotate.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn take_expired(
        &self,
        group: &GroupId,
        now: UnixSeconds,
    ) -> Result<Vec<LeafIndex>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "DELETE FROM location_grant WHERE group_id = ?1 AND expires_at <= ?2 \
                 RETURNING grantee_leaf",
            )?;
            let rows = statement
                .query_map(params![group.as_bytes(), time(now, "now")?], |row| {
                    row.get::<_, i64>(0)
                })?;
            let mut grantees = rows
                .map(|leaf| {
                    Ok(LeafIndex::new(narrow(
                        leaf?,
                        "location_grant.grantee_leaf",
                    )?))
                })
                .collect::<Result<Vec<_>, StoreError>>()?;
            grantees.sort();
            Ok(grantees)
        })
    }
}

/// Whether a location request was received or sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RequestDirection {
    /// Someone asked to see this device's position.
    Incoming,
    /// This device asked to see someone's position.
    Outgoing,
}

/// Where a location request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationRequestState {
    /// Waiting for an answer.
    Pending,
    /// Approved (a grant was sent or received).
    Approved,
    /// Expired, or declined: "Not now" looks the same as no answer (design B.6.1).
    Lapsed,
}

impl LocationRequestState {
    const fn code(self) -> i64 {
        match self {
            Self::Pending => 0,
            Self::Approved => 1,
            Self::Lapsed => 2,
        }
    }
}

/// A request to see an exact location (design C.2 `location_ask`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocationRequest {
    /// The Meetup.
    pub group: GroupId,
    /// The request's id.
    pub id: LocationRequestId,
    /// Received or sent.
    pub direction: RequestDirection,
    /// The other member: the asker if incoming, the target if outgoing.
    pub peer: LeafIndex,
    /// When it was asked.
    pub created_at: UnixSeconds,
    /// Where it stands.
    pub state: LocationRequestState,
}

/// Repository for the `location_ask` table.
pub struct LocationAsks<'session> {
    session: &'session Session,
}

const ASK_COLUMNS: &str = "group_id, req_id, direction, peer_leaf, created_at, state";

fn ask_from_row(row: &Row<'_>) -> Result<LocationRequest, StoreError> {
    Ok(LocationRequest {
        group: GroupId::from_bytes(read_array(row, 0, "location_ask.group_id")?),
        id: LocationRequestId::from_bytes(read_array(row, 1, "location_ask.req_id")?),
        direction: match row.get::<_, i64>(2)? {
            0 => RequestDirection::Incoming,
            1 => RequestDirection::Outgoing,
            _ => {
                return Err(StoreError::Corrupt {
                    field: "location_ask.direction",
                });
            }
        },
        peer: LeafIndex::new(narrow(row.get(3)?, "location_ask.peer_leaf")?),
        created_at: read_time(row, 4, "location_ask.created_at")?,
        state: match row.get::<_, i64>(5)? {
            0 => LocationRequestState::Pending,
            1 => LocationRequestState::Approved,
            2 => LocationRequestState::Lapsed,
            _ => {
                return Err(StoreError::Corrupt {
                    field: "location_ask.state",
                });
            }
        },
    })
}

impl LocationAsks<'_> {
    /// Records a request.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Meetup does not exist or the id is already stored.
    pub fn insert(&self, request: &LocationRequest) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                &format!(
                    "INSERT INTO location_ask ({ASK_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                ),
                params![
                    request.group.as_bytes(),
                    request.id.as_bytes(),
                    match request.direction {
                        RequestDirection::Incoming => 0,
                        RequestDirection::Outgoing => 1,
                    },
                    request.peer.get(),
                    time(request.created_at, "location_ask.created_at")?,
                    request.state.code(),
                ],
            )?;
            Ok(())
        })
    }

    /// Moves a request to `state`.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such request.
    pub fn set_state(
        &self,
        group: &GroupId,
        id: &LocationRequestId,
        state: LocationRequestState,
    ) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE location_ask SET state = ?3 WHERE group_id = ?1 AND req_id = ?2",
                    params![group.as_bytes(), id.as_bytes(), state.code()],
                )?,
                "location_ask",
            )
        })
    }

    /// The Meetup's requests, oldest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self, group: &GroupId) -> Result<Vec<LocationRequest>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {ASK_COLUMNS} FROM location_ask WHERE group_id = ?1 \
                 ORDER BY created_at, req_id"
            ))?;
            let rows = statement.query_map([group.as_bytes()], |row| Ok(ask_from_row(row)))?;
            rows.map(|row| row?).collect()
        })
    }

    /// Deletes requests created before `cutoff` (asks expire after 10 minutes); returns how
    /// many.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete_created_before(&self, cutoff: UnixSeconds) -> Result<usize, StoreError> {
        self.session.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM location_ask WHERE created_at < ?1",
                [time(cutoff, "cutoff")?],
            )?)
        })
    }
}

#[cfg(test)]
#[path = "meetup_tests.rs"]
mod tests;
