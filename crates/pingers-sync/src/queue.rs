// SPDX-License-Identifier: AGPL-3.0-only
//! Anonymous requests that could not be sent in this wake (design B.12): no relay was
//! reachable, or the wake budget ran out before they started.
//!
//! The caller chooses per request whether it is worth keeping. A lease upload is not: a
//! missed lease looks like a dummy to the server, and the next wake posts a fresh one. A
//! knock or a Welcome is, until the lease it belongs to ends.

use std::collections::VecDeque;
use std::num::NonZeroUsize;

use pingers_time::UnixSeconds;

use crate::{ApiRequest, Route, WakeBudget};

/// What to do with an anonymous request that could not be sent now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum QueuePolicy {
    /// Drop it: a later wake makes a fresh one (lease uploads, reads, board pages).
    Drop,
    /// Keep it and send it at a later flush, until this time; then drop it.
    KeepUntil(UnixSeconds),
}

/// How a request is sent: what to do if it cannot be, and the wake budget it must fit in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sending {
    /// Queue or drop when not sent.
    pub policy: QueuePolicy,
    /// The time left in this wake.
    pub budget: WakeBudget,
}

impl Sending {
    /// Dropped if it cannot be sent now.
    #[must_use]
    pub const fn or_drop(budget: WakeBudget) -> Self {
        Self {
            policy: QueuePolicy::Drop,
            budget,
        }
    }

    /// Queued until `until` if it cannot be sent now.
    #[must_use]
    pub const fn or_keep_until(until: UnixSeconds, budget: WakeBudget) -> Self {
        Self {
            policy: QueuePolicy::KeepUntil(until),
            budget,
        }
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

/// A request waiting for a later wake.
pub(crate) struct Queued {
    pub(crate) request: ApiRequest,
    pub(crate) keep_until: UnixSeconds,
}

/// The queue itself, oldest first.
pub(crate) struct RequestQueue {
    entries: VecDeque<Queued>,
    capacity: QueueCapacity,
}

/// What a push onto the queue did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pushed {
    /// Queued with room to spare.
    Queued,
    /// Queued, and the oldest entry was dropped to make room.
    QueuedDroppingOldest(Route),
}

impl RequestQueue {
    pub(crate) const fn new(capacity: QueueCapacity) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Appends `request`, dropping the oldest entry if the queue is full.
    pub(crate) fn push(&mut self, request: ApiRequest, keep_until: UnixSeconds) -> Pushed {
        let dropped = if self.entries.len() >= self.capacity.get() {
            self.entries
                .pop_front()
                .map(|oldest| oldest.request.route())
        } else {
            None
        };
        self.entries.push_back(Queued {
            request,
            keep_until,
        });
        dropped.map_or(Pushed::Queued, Pushed::QueuedDroppingOldest)
    }

    /// Puts a request taken for sending back at the front, unchanged in order.
    pub(crate) fn push_front(&mut self, queued: Queued) {
        self.entries.push_front(queued);
    }

    /// Removes and counts entries whose time has passed (`keep_until` before `now`).
    pub(crate) fn drop_expired(&mut self, now: UnixSeconds) -> usize {
        let before = self.entries.len();
        self.entries.retain(|queued| queued.keep_until >= now);
        before.saturating_sub(self.entries.len())
    }

    /// Takes the oldest entry.
    pub(crate) fn pop(&mut self) -> Option<Queued> {
        self.entries.pop_front()
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn capacity(count: usize) -> QueueCapacity {
        QueueCapacity::new(NonZeroUsize::new(count).unwrap_or(NonZeroUsize::MIN))
    }

    #[test]
    fn a_full_queue_drops_its_oldest_entry() {
        let mut queue = RequestQueue::new(capacity(2));
        let until = UnixSeconds::new(100);
        assert_eq!(queue.push(ApiRequest::params(), until), Pushed::Queued);
        assert_eq!(
            queue.push(ApiRequest::privacy_pass_keys(), until),
            Pushed::Queued
        );
        assert_eq!(
            queue.push(ApiRequest::params(), until),
            Pushed::QueuedDroppingOldest(Route::Params)
        );
        assert_eq!(queue.len(), 2);
        assert_eq!(
            queue.pop().map(|queued| queued.request.route()),
            Some(Route::PrivacyPassKeys)
        );
    }

    #[test]
    fn entries_are_kept_through_their_last_second_and_dropped_after() {
        let mut queue = RequestQueue::new(capacity(4));
        queue.push(ApiRequest::params(), UnixSeconds::new(100));
        queue.push(ApiRequest::privacy_pass_keys(), UnixSeconds::new(200));
        assert_eq!(queue.drop_expired(UnixSeconds::new(100)), 0);
        assert_eq!(queue.drop_expired(UnixSeconds::new(101)), 1);
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue.pop().map(|queued| queued.request.route()),
            Some(Route::PrivacyPassKeys)
        );
    }

    #[test]
    fn the_default_capacity_is_thirty_two() {
        assert_eq!(QueueCapacity::default().get(), 32);
    }
}
