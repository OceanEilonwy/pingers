// SPDX-License-Identifier: AGPL-3.0-only
//! `request_queue`, `board_upload` and `own_group_entry`: what pingers-sync keeps across an
//! app restart (schema version 3).

use std::fmt;
use std::num::NonZeroUsize;

use pingers_crypto::RevokeKey;
use pingers_proto::{GroupId, LeaseId, SequenceNumber};
use pingers_time::UnixSeconds;
use rusqlite::{OptionalExtension as _, Row, params};
use zeroize::Zeroizing;

use crate::codec::{
    from_int, narrow, read_array, read_optional_array, read_secret, read_secret_blob, read_time,
    time, to_int,
};
use crate::server::Scope;
use crate::{ServerSession, Session, StoreError};

impl ServerSession<'_> {
    /// Anonymous requests to this server waiting for a later wake (`request_queue` table).
    #[must_use]
    pub fn request_queue(&self) -> RequestQueue<'_> {
        RequestQueue {
            session: self.scope(),
        }
    }

    /// The last upload to this server's board: its lease id and revoke key (`board_upload`).
    #[must_use]
    pub fn board_upload(&self) -> BoardUploads<'_> {
        BoardUploads {
            session: self.scope(),
        }
    }
}

impl Session {
    /// Log positions of group entries this device stored (`own_group_entry` table).
    #[must_use]
    pub fn own_group_entries(&self) -> OwnGroupEntries<'_> {
        OwnGroupEntries { session: self }
    }
}

/// How many requests the queue holds; when full the oldest is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QueueCapacity(NonZeroUsize);

impl QueueCapacity {
    /// Wraps a capacity.
    #[must_use]
    pub const fn new(capacity: NonZeroUsize) -> Self {
        Self(capacity)
    }

    /// The capacity.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for QueueCapacity {
    /// 32: a few knocks and Welcomes per wake, over a handful of wakes.
    fn default() -> Self {
        Self(NonZeroUsize::MIN.saturating_add(31))
    }
}

/// A queued request as pingers-sync encodes it, opaque to the store. It can hold
/// capabilities (a revoke key, a group request signature), so it is zeroed on drop and its
/// `Debug` prints nothing of it.
pub struct RequestBytes(Zeroizing<Vec<u8>>);

impl RequestBytes {
    /// Wraps the encoded request, taking ownership so it is zeroed when dropped.
    #[must_use]
    pub const fn from_bytes(bytes: Zeroizing<Vec<u8>>) -> Self {
        Self(bytes)
    }

    /// The encoded request, for pingers-sync to decode.
    #[must_use]
    pub fn expose_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for RequestBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RequestBytes(redacted)")
    }
}

/// A request to queue.
pub struct NewQueuedRequest<'request> {
    /// The group it belongs to, if it is a group request: ending that Meetup or deleting
    /// that Party deletes it.
    pub group: Option<GroupId>,
    /// The last second it may still be sent in.
    pub keep_until: UnixSeconds,
    /// The encoded request.
    pub request: &'request RequestBytes,
}

/// A request taken off the queue. Only [`RequestQueue::take_oldest`] makes one, so
/// [`RequestQueue::put_back`] always restores a real entry at its old place.
#[derive(Debug)]
pub struct QueuedRequest {
    position: i64,
    /// The group it belongs to, if any.
    pub group: Option<GroupId>,
    /// The last second it may still be sent in.
    pub keep_until: UnixSeconds,
    /// The encoded request.
    pub request: RequestBytes,
}

/// What a push onto the queue did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[must_use]
pub enum Pushed {
    /// Queued with room to spare.
    Queued,
    /// Queued, and the oldest entries were dropped to keep within the capacity.
    QueuedDroppingOldest,
}

/// Repository for the `request_queue` table: oldest first, bounded by a [`QueueCapacity`].
pub struct RequestQueue<'session> {
    session: Scope<'session>,
}

fn queued_from_row(row: &Row<'_>) -> Result<QueuedRequest, StoreError> {
    Ok(QueuedRequest {
        position: row.get(0)?,
        group: read_optional_array(row, 1, "request_queue.group_id")?.map(GroupId::from_bytes),
        keep_until: read_time(row, 2, "request_queue.keep_until")?,
        request: RequestBytes(read_secret_blob(row, 3, "request_queue.request")?),
    })
}

impl RequestQueue<'_> {
    /// Appends a request, first dropping the oldest entries so that at most `capacity`
    /// remain, all in one transaction.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if `keep_until` does not fit; [`StoreError::Database`] if
    /// the request is empty or the write fails (nothing is changed then).
    pub fn push(
        &self,
        entry: &NewQueuedRequest<'_>,
        capacity: QueueCapacity,
    ) -> Result<Pushed, StoreError> {
        let keep_until = time(entry.keep_until, "request_queue.keep_until")?;
        // Room for the new entry: keep at most capacity - 1 of the old ones.
        let kept = i64::try_from(capacity.get().saturating_sub(1)).unwrap_or(i64::MAX);
        self.session.transaction(|session| {
            session.with(|connection| {
                let dropped = connection.execute(
                    "DELETE FROM request_queue WHERE server_key = ?2 AND position NOT IN \
                     (SELECT position FROM request_queue WHERE server_key = ?2 \
                     ORDER BY position DESC LIMIT ?1)",
                    params![kept, session.key()],
                )?;
                connection.execute(
                    "INSERT INTO request_queue (group_id, keep_until, request, server_key) \
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        entry.group.as_ref().map(GroupId::as_bytes),
                        keep_until,
                        entry.request.expose_bytes(),
                        session.key(),
                    ],
                )?;
                Ok(if dropped == 0 {
                    Pushed::Queued
                } else {
                    Pushed::QueuedDroppingOldest
                })
            })
        })
    }

    /// Removes and returns the oldest entry. Removing it before it is sent means a crash
    /// mid-send loses it rather than sending it twice.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn take_oldest(&self) -> Result<Option<QueuedRequest>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "DELETE FROM request_queue WHERE position = \
                     (SELECT min(position) FROM request_queue WHERE server_key = ?1) \
                     RETURNING position, group_id, keep_until, request",
                    [self.session.key()],
                    |row| Ok(queued_from_row(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// Puts an entry taken by [`RequestQueue::take_oldest`] back at its old place, ahead of
    /// anything queued since.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if `keep_until` does not fit; [`StoreError::Database`] if
    /// the write fails.
    pub fn put_back(&self, entry: &QueuedRequest) -> Result<(), StoreError> {
        let keep_until = time(entry.keep_until, "request_queue.keep_until")?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT INTO request_queue (position, group_id, keep_until, request, \
                 server_key) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    entry.position,
                    entry.group.as_ref().map(GroupId::as_bytes),
                    keep_until,
                    entry.request.expose_bytes(),
                    self.session.key(),
                ],
            )?;
            Ok(())
        })
    }

    /// Deletes the entries whose last second is before `now`; returns how many.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if `now` does not fit; [`StoreError::Database`].
    pub fn drop_expired(&self, now: UnixSeconds) -> Result<usize, StoreError> {
        let now = time(now, "request_queue.keep_until")?;
        self.session.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM request_queue WHERE keep_until < ?1 AND server_key = ?2",
                params![now, self.session.key()],
            )?)
        })
    }

    /// How many requests are queued.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the query fails.
    pub fn count(&self) -> Result<usize, StoreError> {
        self.session.with(|connection| {
            let count: i64 = connection.query_row(
                "SELECT count(*) FROM request_queue WHERE server_key = ?1",
                [self.session.key()],
                |row| row.get(0),
            )?;
            narrow(count, "request_queue count")
        })
    }
}

/// The last lease this device uploaded: what the next upload names in `replaces`.
pub struct LastUpload {
    /// The id the server assigned.
    pub lease: LeaseId,
    /// The key whose hash is the lease's revoke commitment.
    pub revoke_key: RevokeKey,
}

/// Repository for the `board_upload` table: at most one row, replaced by every upload.
///
/// Separate from [`crate::Leases`], which keeps the whole lease with each slot's Spot and
/// knock mailbox: only pingers-wake knows those, while the upload is pingers-sync's.
pub struct BoardUploads<'session> {
    session: Scope<'session>,
}

impl BoardUploads<'_> {
    /// Records `upload` as the last one, replacing the one before.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the write fails.
    pub fn remember(&self, upload: &LastUpload) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO board_upload (server_key, lease_id, revoke_key) \
                 VALUES (?1, ?2, ?3)",
                params![
                    self.session.key(),
                    upload.lease.as_bytes(),
                    upload.revoke_key.expose_bytes()
                ],
            )?;
            Ok(())
        })
    }

    /// The last upload, if any.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn last(&self) -> Result<Option<LastUpload>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT lease_id, revoke_key FROM board_upload WHERE server_key = ?1",
                    [self.session.key()],
                    |row| {
                        Ok((|| {
                            Ok::<_, StoreError>(LastUpload {
                                lease: LeaseId::from_bytes(read_array(
                                    row,
                                    0,
                                    "board_upload.lease_id",
                                )?),
                                revoke_key: RevokeKey::from_bytes(read_secret(
                                    row,
                                    1,
                                    "board_upload.revoke_key",
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

/// Repository for the `own_group_entry` table: positions of entries this device stored in a
/// group's log, so a pull passes over them even after a restart.
pub struct OwnGroupEntries<'session> {
    session: &'session Session,
}

impl OwnGroupEntries<'_> {
    /// Records that the entry at `sequence` of `group` is this device's; recording it again
    /// is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if `sequence` does not fit; [`StoreError::Database`].
    pub fn remember(&self, group: &GroupId, sequence: SequenceNumber) -> Result<(), StoreError> {
        let sequence = to_int(sequence.get(), "own_group_entry.sequence")?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR IGNORE INTO own_group_entry (group_id, sequence) VALUES (?1, ?2)",
                params![group.as_bytes(), sequence],
            )?;
            Ok(())
        })
    }

    /// The positions recorded for `group`, in log order.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn positions(&self, group: &GroupId) -> Result<Vec<SequenceNumber>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT sequence FROM own_group_entry WHERE group_id = ?1 ORDER BY sequence",
            )?;
            let rows = statement.query_map([group.as_bytes()], |row| row.get::<_, i64>(0))?;
            rows.map(|row| {
                Ok(SequenceNumber::new(from_int(
                    row?,
                    "own_group_entry.sequence",
                )?))
            })
            .collect()
        })
    }

    /// Forgets the positions of `group` up to and including `through`: the pull has moved
    /// past them.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if `through` does not fit; [`StoreError::Database`].
    pub fn forget_through(
        &self,
        group: &GroupId,
        through: SequenceNumber,
    ) -> Result<(), StoreError> {
        let through = to_int(through.get(), "own_group_entry.sequence")?;
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM own_group_entry WHERE group_id = ?1 AND sequence <= ?2",
                params![group.as_bytes(), through],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
