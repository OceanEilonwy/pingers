// SPDX-License-Identifier: AGPL-3.0-only
//! Location requests in both directions, each expiring after [`ASK_EXPIRY`] (design B.6.1).
//!
//! An unanswered or declined request simply lapses: "Not now" and an expired request leave
//! the same state, and nothing is sent either way.
//!
//! Asks are stored in the group log, so one can arrive twice (live, then again when the log
//! is read after a reconnect) or late. Each request id is therefore remembered until its ask
//! has expired by its own timestamp, whatever the answer, and expiry counts from that
//! timestamp rather than from receipt.

use std::collections::BTreeMap;

use pingers_proto::{LeafIndex, LocationRequestId};
use pingers_time::UnixSeconds;

use crate::{ASK_EXPIRY, MeetupError};

/// A request this device received and has not answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Incoming {
    pub(crate) from: LeafIndex,
    pub(crate) expires: UnixSeconds,
}

/// A request this device sent that is still pending ("Request sent").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingAsk {
    /// The request.
    pub id: LocationRequestId,
    /// When it lapses.
    pub expires: UnixSeconds,
}

/// A request this device received that waits on an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    /// The request, for the answer.
    pub id: LocationRequestId,
    /// Who asks.
    pub from: LeafIndex,
    /// When it lapses unanswered.
    pub expires: UnixSeconds,
}

/// Requests waiting on an answer, in both directions.
#[derive(Clone, Debug, Default)]
pub(crate) struct Requests {
    incoming: BTreeMap<LocationRequestId, Incoming>,
    /// Every request id received, until its ask has expired by its own timestamp; a copy
    /// arriving later is then refused as expired.
    seen: BTreeMap<LocationRequestId, UnixSeconds>,
    /// This device's pending request to each member.
    outgoing: BTreeMap<LeafIndex, PendingAsk>,
}

/// Whether a received ask should be acted on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sighting {
    /// Live and not seen before; if it waits on an answer, it lapses at `expires`.
    New {
        /// When the request lapses.
        expires: UnixSeconds,
    },
    /// The same request id arrived before.
    Duplicate,
    /// Older than [`ASK_EXPIRY`] by its own timestamp.
    Expired,
}

pub(crate) fn expiry_from(now: UnixSeconds) -> Result<UnixSeconds, MeetupError> {
    now.checked_add(ASK_EXPIRY).ok_or(MeetupError::Overflow)
}

impl Requests {
    /// Checks a received ask written at `asked_at` and remembers its id. A live new ask lapses
    /// `ASK_EXPIRY` after `asked_at`, or after `now` if the sender's clock is ahead (an ask
    /// never waits longer than that from receipt).
    pub(crate) fn sight(
        &mut self,
        request: LocationRequestId,
        asked_at: UnixSeconds,
        now: UnixSeconds,
    ) -> Result<Sighting, MeetupError> {
        let by_timestamp = expiry_from(asked_at)?;
        if by_timestamp <= now {
            return Ok(Sighting::Expired);
        }
        if self.seen.contains_key(&request) {
            return Ok(Sighting::Duplicate);
        }
        self.seen.insert(request, by_timestamp);
        Ok(Sighting::New {
            expires: expiry_from(asked_at.min(now))?,
        })
    }

    /// Keeps a new request from `from` pending until `expires`.
    pub(crate) fn hold(
        &mut self,
        request: LocationRequestId,
        from: LeafIndex,
        expires: UnixSeconds,
    ) {
        self.incoming.insert(request, Incoming { from, expires });
    }

    /// Takes a pending request to answer it. An expired one is gone.
    pub(crate) fn take(
        &mut self,
        request: LocationRequestId,
        now: UnixSeconds,
    ) -> Result<Incoming, MeetupError> {
        match self.incoming.remove(&request) {
            Some(pending) if pending.expires > now => Ok(pending),
            Some(_) | None => Err(MeetupError::UnknownRequest),
        }
    }

    /// Records this device's request to `target`.
    pub(crate) fn sent(&mut self, target: LeafIndex, ask: PendingAsk) {
        self.outgoing.insert(target, ask);
    }

    /// This device's request to `target`, if one is pending.
    pub(crate) fn outgoing(&self, target: LeafIndex, now: UnixSeconds) -> Option<PendingAsk> {
        self.outgoing
            .get(&target)
            .copied()
            .filter(|ask| ask.expires > now)
    }

    /// The received requests still waiting on an answer at `now`.
    pub(crate) fn incoming(&self, now: UnixSeconds) -> Vec<PendingRequest> {
        self.incoming
            .iter()
            .filter(|(_id, pending)| pending.expires > now)
            .map(|(id, pending)| PendingRequest {
                id: *id,
                from: pending.from,
                expires: pending.expires,
            })
            .collect()
    }

    /// The request to `target` was answered with a grant.
    pub(crate) fn answered(&mut self, target: LeafIndex) {
        self.outgoing.remove(&target);
    }

    /// Drops everything that has lapsed by `now`.
    pub(crate) fn expire(&mut self, now: UnixSeconds) {
        self.incoming
            .retain(|_request, pending| pending.expires > now);
        self.outgoing.retain(|_target, ask| ask.expires > now);
        self.seen.retain(|_request, expires| *expires > now);
    }

    /// Forgets requests from and to a member who left.
    pub(crate) fn forget(&mut self, leaf: LeafIndex) {
        self.incoming
            .retain(|_request, pending| pending.from != leaf);
        self.outgoing.remove(&leaf);
    }

    /// Forgets every received request (the setting changed to `Nobody`).
    pub(crate) fn drop_incoming(&mut self) {
        self.incoming.clear();
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.incoming.is_empty() && self.outgoing.is_empty() && self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn request(byte: u8) -> LocationRequestId {
        LocationRequestId::from_bytes([byte; 16])
    }

    /// Sights and holds a new ask, as the session does for one that waits on an answer.
    fn receive(
        requests: &mut Requests,
        id: LocationRequestId,
        from: LeafIndex,
        now: UnixSeconds,
    ) -> Result<Sighting, MeetupError> {
        let sighting = requests.sight(id, now, now)?;
        if let Sighting::New { expires } = sighting {
            requests.hold(id, from, expires);
        }
        Ok(sighting)
    }

    #[test]
    fn a_request_can_be_answered_until_just_before_it_expires() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let now = UnixSeconds::new(1_000);
        assert_eq!(
            receive(&mut requests, request(1), LeafIndex::new(2), now)?,
            Sighting::New {
                expires: UnixSeconds::new(1_600)
            }
        );
        let pending = requests.take(request(1), UnixSeconds::new(1_599))?;
        assert_eq!(pending.from, LeafIndex::new(2));
        assert_eq!(pending.expires, UnixSeconds::new(1_600));
        Ok(())
    }

    #[test]
    fn a_request_cannot_be_answered_once_expired() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let _new = receive(
            &mut requests,
            request(1),
            LeafIndex::new(2),
            UnixSeconds::new(1_000),
        )?;
        assert!(matches!(
            requests.take(request(1), UnixSeconds::new(1_600)),
            Err(MeetupError::UnknownRequest)
        ));
        Ok(())
    }

    #[test]
    fn a_repeated_request_id_is_a_duplicate_even_after_an_answer() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let now = UnixSeconds::new(1_000);
        let _new = receive(&mut requests, request(1), LeafIndex::new(2), now)?;
        let _answered = requests.take(request(1), now)?;
        assert_eq!(requests.sight(request(1), now, now)?, Sighting::Duplicate);
        Ok(())
    }

    #[test]
    fn a_late_ask_expires_by_its_timestamp_not_its_receipt() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let asked_at = UnixSeconds::new(1_000);
        assert_eq!(
            requests.sight(request(1), asked_at, UnixSeconds::new(1_599))?,
            Sighting::New {
                expires: UnixSeconds::new(1_600)
            }
        );
        assert_eq!(
            requests.sight(request(2), asked_at, UnixSeconds::new(1_600))?,
            Sighting::Expired
        );
        Ok(())
    }

    #[test]
    fn an_ask_dated_ahead_waits_no_longer_than_from_receipt() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        assert_eq!(
            requests.sight(request(1), UnixSeconds::new(5_000), UnixSeconds::new(1_000))?,
            Sighting::New {
                expires: UnixSeconds::new(1_600)
            }
        );
        // Its id is kept until the ask is expired by its own timestamp, so a copy read
        // after the pending request lapsed does not prompt again.
        requests.expire(UnixSeconds::new(1_600));
        assert_eq!(
            requests.sight(request(1), UnixSeconds::new(5_000), UnixSeconds::new(1_600))?,
            Sighting::Duplicate
        );
        requests.expire(UnixSeconds::new(5_600));
        assert!(requests.is_empty());
        Ok(())
    }

    #[test]
    fn expiry_drops_lapsed_requests_in_both_directions() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let _new = receive(
            &mut requests,
            request(1),
            LeafIndex::new(2),
            UnixSeconds::new(1_000),
        )?;
        requests.sent(
            LeafIndex::new(3),
            PendingAsk {
                id: request(9),
                expires: UnixSeconds::new(1_700),
            },
        );
        requests.expire(UnixSeconds::new(1_600));
        assert!(matches!(
            requests.take(request(1), UnixSeconds::new(0)),
            Err(MeetupError::UnknownRequest)
        ));
        assert!(!requests.is_empty());
        requests.expire(UnixSeconds::new(1_700));
        assert!(requests.is_empty());
        Ok(())
    }

    #[test]
    fn forgetting_a_member_keeps_everyone_elses_requests() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let now = UnixSeconds::new(1_000);
        let _left = receive(&mut requests, request(1), LeafIndex::new(2), now)?;
        let _stays = receive(&mut requests, request(2), LeafIndex::new(3), now)?;
        requests.forget(LeafIndex::new(2));
        assert!(requests.take(request(1), now).is_err());
        assert_eq!(requests.take(request(2), now)?.from, LeafIndex::new(3));
        Ok(())
    }

    #[test]
    fn an_outgoing_request_is_pending_until_it_expires() {
        let mut requests = Requests::default();
        let ask = PendingAsk {
            id: request(4),
            expires: UnixSeconds::new(1_600),
        };
        requests.sent(LeafIndex::new(3), ask);
        assert_eq!(
            requests.outgoing(LeafIndex::new(3), UnixSeconds::new(1_599)),
            Some(ask)
        );
        assert_eq!(
            requests.outgoing(LeafIndex::new(3), UnixSeconds::new(1_600)),
            None
        );
    }

    #[test]
    fn received_requests_are_listed_until_they_lapse() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let _new = receive(
            &mut requests,
            request(1),
            LeafIndex::new(2),
            UnixSeconds::new(1_000),
        )?;
        let listed = requests.incoming(UnixSeconds::new(1_000));
        assert_eq!(
            listed,
            vec![PendingRequest {
                id: request(1),
                from: LeafIndex::new(2),
                expires: UnixSeconds::new(1_600),
            }]
        );
        assert!(requests.incoming(UnixSeconds::new(1_600)).is_empty());
        Ok(())
    }
}
