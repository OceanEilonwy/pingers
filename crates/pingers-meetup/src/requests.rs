// SPDX-License-Identifier: AGPL-3.0-only
//! Location requests in both directions, each expiring after [`ASK_EXPIRY`] (design B.6.1).
//!
//! An unanswered or declined request simply lapses: "Not now" and an expired request leave
//! the same (empty) state, and nothing is sent either way.

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

/// Requests waiting on an answer, in both directions.
#[derive(Clone, Debug, Default)]
pub(crate) struct Requests {
    incoming: BTreeMap<LocationRequestId, Incoming>,
    /// When this device's request to each member lapses.
    outgoing: BTreeMap<LeafIndex, UnixSeconds>,
}

/// Whether a received request is new.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Received {
    /// Not seen before; it is now pending.
    New,
    /// The same request id was already pending.
    Duplicate,
}

pub(crate) fn expiry_from(now: UnixSeconds) -> Result<UnixSeconds, MeetupError> {
    now.checked_add(ASK_EXPIRY).ok_or(MeetupError::Overflow)
}

impl Requests {
    /// Records a received request, pending until `ASK_EXPIRY` after `now`.
    pub(crate) fn receive(
        &mut self,
        request: LocationRequestId,
        from: LeafIndex,
        now: UnixSeconds,
    ) -> Result<Received, MeetupError> {
        if self.incoming.contains_key(&request) {
            return Ok(Received::Duplicate);
        }
        let expires = expiry_from(now)?;
        self.incoming.insert(request, Incoming { from, expires });
        Ok(Received::New)
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
    pub(crate) fn sent(&mut self, target: LeafIndex, expires: UnixSeconds) {
        self.outgoing.insert(target, expires);
    }

    /// When this device's request to `target` lapses, if one is pending.
    pub(crate) fn outgoing(&self, target: LeafIndex, now: UnixSeconds) -> Option<UnixSeconds> {
        self.outgoing
            .get(&target)
            .copied()
            .filter(|expires| *expires > now)
    }

    /// The request to `target` was answered with a grant.
    pub(crate) fn answered(&mut self, target: LeafIndex) {
        self.outgoing.remove(&target);
    }

    /// Drops everything that has lapsed by `now`.
    pub(crate) fn expire(&mut self, now: UnixSeconds) {
        self.incoming
            .retain(|_request, pending| pending.expires > now);
        self.outgoing.retain(|_target, expires| *expires > now);
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
        self.incoming.is_empty() && self.outgoing.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn request(byte: u8) -> LocationRequestId {
        LocationRequestId::from_bytes([byte; 16])
    }

    #[test]
    fn a_request_can_be_answered_until_just_before_it_expires() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let now = UnixSeconds::new(1_000);
        assert_eq!(
            requests.receive(request(1), LeafIndex::new(2), now)?,
            Received::New
        );
        let pending = requests.take(request(1), UnixSeconds::new(1_599))?;
        assert_eq!(pending.from, LeafIndex::new(2));
        assert_eq!(pending.expires, UnixSeconds::new(1_600));
        Ok(())
    }

    #[test]
    fn a_request_cannot_be_answered_once_expired() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let _new = requests.receive(request(1), LeafIndex::new(2), UnixSeconds::new(1_000))?;
        assert!(matches!(
            requests.take(request(1), UnixSeconds::new(1_600)),
            Err(MeetupError::UnknownRequest)
        ));
        Ok(())
    }

    #[test]
    fn a_repeated_request_id_is_a_duplicate() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let now = UnixSeconds::new(1_000);
        let _new = requests.receive(request(1), LeafIndex::new(2), now)?;
        assert_eq!(
            requests.receive(request(1), LeafIndex::new(2), now)?,
            Received::Duplicate
        );
        Ok(())
    }

    #[test]
    fn expiry_drops_lapsed_requests_in_both_directions() -> Result<(), MeetupError> {
        let mut requests = Requests::default();
        let _new = requests.receive(request(1), LeafIndex::new(2), UnixSeconds::new(1_000))?;
        requests.sent(LeafIndex::new(3), UnixSeconds::new(1_700));
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
    fn an_outgoing_request_is_pending_until_it_expires() {
        let mut requests = Requests::default();
        requests.sent(LeafIndex::new(3), UnixSeconds::new(1_600));
        assert_eq!(
            requests.outgoing(LeafIndex::new(3), UnixSeconds::new(1_599)),
            Some(UnixSeconds::new(1_600))
        );
        assert_eq!(
            requests.outgoing(LeafIndex::new(3), UnixSeconds::new(1_600)),
            None
        );
    }
}
