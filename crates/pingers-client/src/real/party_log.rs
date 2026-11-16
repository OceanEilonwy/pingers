// SPDX-License-Identifier: AGPL-3.0-only
//! A Party group's log, as pingers-sync's `GroupLogOwner` and `CommitOwner` over
//! pingers-parties.
//!
//! The engine calls these synchronously between requests, so each step opens a store session
//! inline (about 2.5 ms, debug build). How far the log was applied is a client record:
//! pingers-store has no column for it yet.

use std::sync::Arc;

use pingers_crypto::LogAuthKeyPair;
use pingers_parties::{Incoming, Outbound, OutgoingCommit, PartiesError, PartyLifecycle};
use pingers_proto::{CommitRequest, Epoch, GroupId, GroupLogEntry, PartyId, SequenceNumber};
use pingers_store::{Database, Session};
use pingers_sync::{CommitOwner, GroupLogOwner};
use pingers_time::Clock;

use super::Outside;
use super::records::{self, Key};
use crate::ClientError;

/// One Party's log while pingers-sync pulls it or commits to it.
pub(crate) struct PartyLog {
    database: Arc<Database>,
    outside: Outside,
    party: PartyId,
    group: GroupId,
    applied: Option<SequenceNumber>,
    key: LogAuthKeyPair,
    /// This device's commit, handed out once by `prepare_commit`.
    pub(crate) pending: Option<OutgoingCommit>,
    /// What else the log asked for (a leaver's proposal to commit, invite mailboxes to delete
    /// after a removal): sent by the outbox after the pull.
    pub(crate) outbound: Vec<Outbound>,
    /// The commit this device made was merged; its Welcome (if any) is still to be posted.
    pub(crate) accepted: Option<OutgoingCommit>,
    /// Whether anything the snapshot shows changed.
    pub(crate) changed: bool,
}

impl PartyLog {
    /// The Party's log position and current key, with `pending` to commit.
    pub(crate) fn load(
        database: &Arc<Database>,
        outside: &Outside,
        party: PartyId,
        pending: Option<OutgoingCommit>,
    ) -> Result<Self, ClientError> {
        let session = database.session()?;
        let row = session
            .parties()
            .get(&party)?
            .ok_or(PartiesError::UnknownParty)?;
        let clock = Arc::clone(&outside.clock);
        let lifecycle = PartyLifecycle::new(&session, clock.as_ref());
        Ok(Self {
            database: Arc::clone(database),
            outside: outside.clone(),
            party,
            group: row.group,
            applied: records::read(&session, &Key::AppliedThrough(&row.group), |reader| {
                Ok(SequenceNumber::new(reader.u64()?))
            })?,
            key: lifecycle.log_auth_key(&party)?,
            pending,
            outbound: Vec::new(),
            accepted: None,
            changed: false,
        })
    }

    /// The Party's group.
    pub(crate) const fn group(&self) -> GroupId {
        self.group
    }

    /// Queues this device's changed profile if one waited for a commit and the log has
    /// settled (call after a pull or a commit, not between log entries: the profile must be
    /// sealed for the epoch the log ends at). A Party this device was removed from has none.
    pub(crate) fn publish_waiting_profile(&mut self) -> Result<(), ClientError> {
        let session = self.session()?;
        let clock = Arc::clone(&self.outside.clock);
        let mut rng = (self.outside.rng)();
        match PartyLifecycle::new(&session, clock.as_ref())
            .publish_waiting_profile(&self.party, &mut rng)
        {
            Ok(outbound) => {
                self.outbound.extend(outbound);
                Ok(())
            }
            Err(PartiesError::UnknownParty) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn session(&self) -> Result<Session, ClientError> {
        Ok(self.database.session()?)
    }

    /// Moves the log position past `sequence` and refreshes the key (the epoch may have moved).
    fn advance(&mut self, session: &Session, sequence: SequenceNumber) -> Result<(), ClientError> {
        records::write(session, &Key::AppliedThrough(&self.group), |writer| {
            writer.u64(sequence.get());
        })?;
        self.applied = Some(sequence);
        let clock = Arc::clone(&self.outside.clock);
        match PartyLifecycle::new(session, clock.as_ref()).log_auth_key(&self.party) {
            Ok(key) => self.key = key,
            // This device was removed: the Party is gone and the pull ends with the log.
            Err(PartiesError::UnknownParty) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

impl GroupLogOwner for PartyLog {
    type Error = ClientError;

    fn applied_through(&self) -> Option<SequenceNumber> {
        self.applied
    }

    fn log_auth_key(&self) -> &LogAuthKeyPair {
        &self.key
    }

    /// An entry pingers-parties cannot use (an old epoch, or one of this device's own after a
    /// restart forgot it) is consumed, as pingers-sync asks; only a storage failure stops.
    fn apply(&mut self, entry: GroupLogEntry) -> Result<(), ClientError> {
        let session = self.session()?;
        let clock = Arc::clone(&self.outside.clock);
        let lifecycle = PartyLifecycle::new(&session, clock.as_ref());
        match lifecycle.process_group_message(&self.group, &entry.body) {
            Ok(Incoming::Removed { outbound, .. }) => {
                self.outbound.extend(outbound);
                self.changed = true;
            }
            Ok(Incoming::LeaveProposed { party, .. }) => {
                let mut rng = (self.outside.rng)();
                match lifecycle.commit_pending_proposals(&party, &mut rng) {
                    Ok(commit) => self.outbound.push(Outbound::Commit(commit)),
                    // Another commit of this device waits; the proposal is committed with it.
                    Err(PartiesError::CommitInFlight) => {}
                    Err(error) => return Err(error.into()),
                }
                self.changed = true;
            }
            Ok(_) => self.changed = true,
            Err(PartiesError::Store(error)) => return Err(error.into()),
            Err(PartiesError::Records(error)) => return Err(error.into()),
            Err(_unusable) => {}
        }
        self.advance(&session, entry.sequence)
    }

    fn skip_own(&mut self, sequence: SequenceNumber) -> Result<(), ClientError> {
        let session = self.session()?;
        self.advance(&session, sequence)
    }
}

impl CommitOwner for PartyLog {
    fn prepare_commit(&mut self) -> Result<Option<CommitRequest>, ClientError> {
        Ok(self.pending.as_ref().map(|commit| commit.request.clone()))
    }

    fn commit_accepted(&mut self, sequence: SequenceNumber) -> Result<(), ClientError> {
        let session = self.session()?;
        let clock = Arc::clone(&self.outside.clock);
        PartyLifecycle::new(&session, clock.as_ref()).commit_accepted(&self.party)?;
        self.accepted = self.pending.take();
        self.changed = true;
        self.advance(&session, sequence)
    }

    /// The commit is dropped; a rejected Add goes back to the waiting joins (pingers-parties),
    /// so nothing is left to commit on the next round.
    fn commit_rejected(&mut self, _head_epoch: Epoch) -> Result<(), ClientError> {
        let session = self.session()?;
        let clock = Arc::clone(&self.outside.clock);
        PartyLifecycle::new(&session, clock.as_ref()).commit_rejected(&self.party)?;
        self.pending = None;
        Ok(())
    }
}

/// The Parties that live on `server`.
pub(crate) fn parties_on(
    session: &Session,
    server: &pingers_proto::ServerId,
) -> Result<Vec<PartyId>, ClientError> {
    Ok(session
        .parties()
        .list()?
        .into_iter()
        .filter(|row| row.server == *server)
        .map(|row| row.id)
        .collect())
}

/// The clock, for callers that only hold the outside world.
pub(crate) fn now(outside: &Outside) -> pingers_time::UnixSeconds {
    outside.clock.now()
}
