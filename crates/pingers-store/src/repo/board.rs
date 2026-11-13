// SPDX-License-Identifier: AGPL-3.0-only
//! `lease` (with `lease_slot`), `knock_out` and `board_seen`.

use pingers_crypto::RevokeKey;
use pingers_proto::{
    Lease, LeaseId, MailboxId, RecordRandomness, SeenSegment, SegmentHash, SpotId,
};
use pingers_time::{Bucket, UnixSeconds};
use rusqlite::{OptionalExtension as _, Row, params};

use crate::codec::{
    bucket, read_array, read_bucket, read_optional_array, read_secret, read_time, time,
};
use crate::repo::expect_one;
use crate::{Session, StoreError};

/// How many segment hashes `board_seen` keeps (design C.2: "4 rows").
pub const SEEN_SEGMENTS_KEPT: usize = 4;

impl Session {
    /// The current board lease (`lease` and `lease_slot` tables).
    #[must_use]
    pub fn leases(&self) -> Leases<'_> {
        Leases { session: self }
    }

    /// Knocks this device sent (`knock_out` table).
    #[must_use]
    pub fn knocks_out(&self) -> KnocksOut<'_> {
        KnocksOut { session: self }
    }

    /// Board segment hashes seen, for the equivocation check (`board_seen` table).
    #[must_use]
    pub fn board_seen(&self) -> BoardSeen<'_> {
        BoardSeen { session: self }
    }
}

/// What this device put in one slot of its lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaseSlot {
    /// The record's fresh randomness `r`.
    pub randomness: RecordRandomness,
    /// The Spot the record announces, or `None` for a dummy.
    pub spot: Option<SpotId>,
    /// The mailbox knocks on this record arrive in.
    pub knock_mailbox: MailboxId,
}

/// This device's lease on the board, with what it needs to revoke or replace it.
pub struct StoredLease {
    /// The id the server assigned.
    pub id: LeaseId,
    /// The bucket it starts in (`b0`).
    pub start: Bucket,
    /// The key whose hash is the lease's revoke commitment.
    pub revoke_key: RevokeKey,
    /// The four slots, in lease order.
    pub slots: [LeaseSlot; Lease::SLOTS],
}

/// Repository for the current lease. Holds at most one: replacing the lease deletes the old
/// one in the same transaction, so the store never accumulates which Spots this device
/// announced in past buckets.
pub struct Leases<'session> {
    session: &'session Session,
}

impl Leases<'_> {
    /// Makes `lease` the current lease, deleting any previous one.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if the bucket does not fit; [`StoreError::Database`] if the
    /// write fails (nothing is changed then).
    pub fn replace(&self, lease: &StoredLease) -> Result<(), StoreError> {
        self.session.transaction(|session| {
            session.with(|connection| {
                connection.execute("DELETE FROM lease", [])?;
                connection.execute(
                    "INSERT INTO lease (lease_id, b0, revoke_key) VALUES (?1, ?2, ?3)",
                    params![
                        lease.id.as_bytes(),
                        bucket(lease.start, "lease.b0")?,
                        lease.revoke_key.expose_bytes(),
                    ],
                )?;
                let mut statement = connection.prepare(
                    "INSERT INTO lease_slot (lease_id, slot, r, spot_id, knock_mailbox) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;
                for (index, slot) in (0_u8..).zip(&lease.slots) {
                    statement.execute(params![
                        lease.id.as_bytes(),
                        index,
                        slot.randomness.as_bytes(),
                        slot.spot.as_ref().map(SpotId::as_bytes),
                        slot.knock_mailbox.as_bytes(),
                    ])?;
                }
                Ok(())
            })
        })
    }

    /// The current lease, if any.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode or the lease does not have
    /// exactly four slots.
    pub fn current(&self) -> Result<Option<StoredLease>, StoreError> {
        self.session.with(|connection| {
            let header = connection
                .query_row("SELECT lease_id, b0, revoke_key FROM lease", [], |row| {
                    Ok((|| {
                        Ok::<_, StoreError>((
                            LeaseId::from_bytes(read_array(row, 0, "lease.lease_id")?),
                            read_bucket(row, 1, "lease.b0")?,
                            RevokeKey::from_bytes(read_secret(row, 2, "lease.revoke_key")?),
                        ))
                    })())
                })
                .optional()?
                .transpose()?;
            let Some((id, start, revoke_key)) = header else {
                return Ok(None);
            };
            let mut statement = connection.prepare(
                "SELECT r, spot_id, knock_mailbox FROM lease_slot WHERE lease_id = ?1 \
                 ORDER BY slot",
            )?;
            let slots = statement
                .query_map([id.as_bytes()], |row| Ok(slot_from_row(row)))?
                .map(|row| row?)
                .collect::<Result<Vec<_>, _>>()?;
            let slots =
                <[LeaseSlot; Lease::SLOTS]>::try_from(slots).map_err(|_| StoreError::Corrupt {
                    field: "lease_slot",
                })?;
            Ok(Some(StoredLease {
                id,
                start,
                revoke_key,
                slots,
            }))
        })
    }

    /// Deletes the current lease (after revoking it); clearing when there is none is not an
    /// error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn clear(&self) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute("DELETE FROM lease", [])?;
            Ok(())
        })
    }
}

fn slot_from_row(row: &Row<'_>) -> Result<LeaseSlot, StoreError> {
    Ok(LeaseSlot {
        randomness: RecordRandomness::from_bytes(read_array(row, 0, "lease_slot.r")?),
        spot: read_optional_array(row, 1, "lease_slot.spot_id")?.map(SpotId::from_bytes),
        knock_mailbox: MailboxId::from_bytes(read_array(row, 2, "lease_slot.knock_mailbox")?),
    })
}

/// Where a knock this device sent stands; drives the `UnansweredKnock` notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnockState {
    /// Sent, no answer yet.
    Sent,
    /// Answered (a Welcome or a reply arrived).
    Answered,
    /// No answer before it expired.
    Expired,
}

impl KnockState {
    const fn code(self) -> i64 {
        match self {
            Self::Sent => 0,
            Self::Answered => 1,
            Self::Expired => 2,
        }
    }
}

/// A knock this device sent (design C.2 `knock_out`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutgoingKnock {
    /// The Spot of the matched record.
    pub spot: SpotId,
    /// The matched record's randomness.
    pub matched: RecordRandomness,
    /// When the knock was sent.
    pub sent_at: UnixSeconds,
    /// Where it stands.
    pub state: KnockState,
    /// The mailbox the answer arrives in.
    pub reply_mailbox: MailboxId,
}

/// Repository for the `knock_out` table.
pub struct KnocksOut<'session> {
    session: &'session Session,
}

fn knock_from_row(row: &Row<'_>) -> Result<OutgoingKnock, StoreError> {
    Ok(OutgoingKnock {
        spot: SpotId::from_bytes(read_array(row, 0, "knock_out.spot_id")?),
        matched: RecordRandomness::from_bytes(read_array(row, 1, "knock_out.matched_r")?),
        sent_at: read_time(row, 2, "knock_out.sent_at")?,
        state: match row.get::<_, i64>(3)? {
            0 => KnockState::Sent,
            1 => KnockState::Answered,
            2 => KnockState::Expired,
            _ => {
                return Err(StoreError::Corrupt {
                    field: "knock_out.state",
                });
            }
        },
        reply_mailbox: MailboxId::from_bytes(read_array(row, 4, "knock_out.reply_mailbox")?),
    })
}

impl KnocksOut<'_> {
    /// Records a sent knock.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if this record was already knocked on.
    pub fn insert(&self, knock: &OutgoingKnock) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT INTO knock_out (spot_id, matched_r, sent_at, state, reply_mailbox) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    knock.spot.as_bytes(),
                    knock.matched.as_bytes(),
                    time(knock.sent_at, "knock_out.sent_at")?,
                    knock.state.code(),
                    knock.reply_mailbox.as_bytes(),
                ],
            )?;
            Ok(())
        })
    }

    /// Moves a knock to `state`.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such knock.
    pub fn set_state(
        &self,
        spot: &SpotId,
        matched: &RecordRandomness,
        state: KnockState,
    ) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE knock_out SET state = ?3 WHERE spot_id = ?1 AND matched_r = ?2",
                    params![spot.as_bytes(), matched.as_bytes(), state.code()],
                )?,
                "knock_out",
            )
        })
    }

    /// The knocks in `state`, oldest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn in_state(&self, state: KnockState) -> Result<Vec<OutgoingKnock>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT spot_id, matched_r, sent_at, state, reply_mailbox FROM knock_out \
                 WHERE state = ?1 ORDER BY sent_at, spot_id, matched_r",
            )?;
            let rows = statement.query_map([state.code()], |row| Ok(knock_from_row(row)))?;
            rows.map(|row| row?).collect()
        })
    }

    /// Deletes knocks sent before `cutoff`, whatever their state; returns how many.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if `cutoff` does not fit; [`StoreError::Database`] if the
    /// delete fails.
    pub fn delete_sent_before(&self, cutoff: UnixSeconds) -> Result<usize, StoreError> {
        self.session.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM knock_out WHERE sent_at < ?1",
                [time(cutoff, "cutoff")?],
            )?)
        })
    }
}

/// Repository for the `board_seen` table: the newest [`SEEN_SEGMENTS_KEPT`] segment hashes.
pub struct BoardSeen<'session> {
    session: &'session Session,
}

impl BoardSeen<'_> {
    /// Records the hash the server reported for a segment, replacing any earlier hash for the
    /// same start bucket, and keeps only the newest [`SEEN_SEGMENTS_KEPT`].
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if the bucket does not fit; [`StoreError::Database`] if the
    /// write fails.
    pub fn record(&self, segment: &SeenSegment) -> Result<(), StoreError> {
        self.session.transaction(|session| {
            session.with(|connection| {
                connection.execute(
                    "INSERT OR REPLACE INTO board_seen (b0, segment_sha256) VALUES (?1, ?2)",
                    params![
                        bucket(segment.start, "board_seen.b0")?,
                        segment.hash.as_bytes()
                    ],
                )?;
                connection.execute(
                    "DELETE FROM board_seen WHERE b0 NOT IN \
                     (SELECT b0 FROM board_seen ORDER BY b0 DESC LIMIT ?1)",
                    [i64::try_from(SEEN_SEGMENTS_KEPT).unwrap_or(i64::MAX)],
                )?;
                Ok(())
            })
        })
    }

    /// The kept segments, newest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self) -> Result<Vec<SeenSegment>, StoreError> {
        self.session.with(|connection| {
            let mut statement =
                connection.prepare("SELECT b0, segment_sha256 FROM board_seen ORDER BY b0 DESC")?;
            let rows = statement.query_map([], |row| {
                Ok((|| {
                    Ok(SeenSegment {
                        start: read_bucket(row, 0, "board_seen.b0")?,
                        hash: SegmentHash::from_bytes(read_array(
                            row,
                            1,
                            "board_seen.segment_sha256",
                        )?),
                    })
                })())
            })?;
            rows.map(|row| row?).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use zeroize::Zeroizing;

    use super::*;
    use crate::test_support::{Fixture, TestError};

    fn lease(fill: u8, start: u64) -> Result<StoredLease, TestError> {
        let slot = |index: u8| LeaseSlot {
            randomness: RecordRandomness::from_bytes([fill.wrapping_add(index); 16]),
            spot: (index == 1).then(|| SpotId::from_bytes([index; 16])),
            knock_mailbox: MailboxId::from_bytes([index; 16]),
        };
        Ok(StoredLease {
            id: LeaseId::from_bytes([fill; 16]),
            start: Bucket::from_index(start).map_err(|_| StoreError::Busy)?,
            revoke_key: RevokeKey::from_bytes(Zeroizing::new([fill; 32])),
            slots: [slot(0), slot(1), slot(2), slot(3)],
        })
    }

    #[test]
    fn lease_round_trips_with_slots_in_order() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let leases = fixture.session.leases();
        assert!(leases.current()?.is_none());
        let stored = lease(1, 2_000_000)?;
        leases.replace(&stored)?;
        let loaded = leases
            .current()?
            .ok_or(StoreError::NotFound { table: "lease" })?;
        assert_eq!(loaded.id, stored.id);
        assert_eq!(loaded.start, stored.start);
        assert_eq!(
            loaded.revoke_key.expose_bytes(),
            stored.revoke_key.expose_bytes()
        );
        assert_eq!(loaded.slots, stored.slots);
        Ok(())
    }

    #[test]
    fn replacing_a_lease_keeps_no_trace_of_the_old_one() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let leases = fixture.session.leases();
        leases.replace(&lease(1, 10)?)?;
        leases.replace(&lease(2, 11)?)?;
        assert_eq!(fixture.rows("lease")?, 1);
        assert_eq!(fixture.rows("lease_slot")?, 4);
        assert_eq!(
            leases.current()?.map(|current| current.id),
            Some(LeaseId::from_bytes([2; 16]))
        );
        leases.clear()?;
        assert!(leases.current()?.is_none());
        assert_eq!(fixture.rows("lease_slot")?, 0);
        Ok(())
    }

    fn knock(fill: u8, sent_at: u64) -> OutgoingKnock {
        OutgoingKnock {
            spot: SpotId::from_bytes([fill; 16]),
            matched: RecordRandomness::from_bytes([fill; 16]),
            sent_at: UnixSeconds::new(sent_at),
            state: KnockState::Sent,
            reply_mailbox: MailboxId::from_bytes([fill; 16]),
        }
    }

    #[test]
    fn knocks_move_between_states() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let knocks = fixture.session.knocks_out();
        let first = knock(1, 100);
        let second = knock(2, 50);
        knocks.insert(&first)?;
        knocks.insert(&second)?;
        assert!(knocks.insert(&first).is_err());
        assert_eq!(knocks.in_state(KnockState::Sent)?, vec![second, first]);
        knocks.set_state(&first.spot, &first.matched, KnockState::Answered)?;
        knocks.set_state(&second.spot, &second.matched, KnockState::Expired)?;
        assert_eq!(
            knocks.in_state(KnockState::Answered)?,
            vec![OutgoingKnock {
                state: KnockState::Answered,
                ..first
            }]
        );
        assert_eq!(
            knocks.in_state(KnockState::Expired)?,
            vec![OutgoingKnock {
                state: KnockState::Expired,
                ..second
            }]
        );
        assert!(knocks.in_state(KnockState::Sent)?.is_empty());
        assert!(matches!(
            knocks.set_state(&second.spot, &first.matched, KnockState::Sent),
            Err(StoreError::NotFound { table: "knock_out" })
        ));
        Ok(())
    }

    #[test]
    fn old_knocks_are_pruned_by_send_time() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let knocks = fixture.session.knocks_out();
        knocks.insert(&knock(1, 100))?;
        knocks.insert(&knock(2, 200))?;
        assert_eq!(knocks.delete_sent_before(UnixSeconds::new(100))?, 0);
        assert_eq!(knocks.delete_sent_before(UnixSeconds::new(150))?, 1);
        assert_eq!(knocks.in_state(KnockState::Sent)?, vec![knock(2, 200)]);
        Ok(())
    }

    fn segment(start: u64, fill: u8) -> Result<SeenSegment, TestError> {
        Ok(SeenSegment {
            start: Bucket::from_index(start).map_err(|_| StoreError::Busy)?,
            hash: SegmentHash::from_bytes([fill; 32]),
        })
    }

    #[test]
    fn board_seen_keeps_the_newest_four() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let seen = fixture.session.board_seen();
        for start in [3, 1, 5, 2, 4] {
            seen.record(&segment(start, 0)?)?;
        }
        // Replacing a kept bucket's hash does not evict anything.
        seen.record(&segment(5, 9)?)?;
        assert_eq!(
            seen.list()?,
            vec![
                segment(5, 9)?,
                segment(4, 0)?,
                segment(3, 0)?,
                segment(2, 0)?
            ]
        );
        Ok(())
    }
}
