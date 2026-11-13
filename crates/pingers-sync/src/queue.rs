// SPDX-License-Identifier: AGPL-3.0-only
//! Anonymous requests that could not be sent in this wake (design B.12): no relay was
//! reachable, or the wake budget ran out before they started.
//!
//! The caller chooses per request whether it is worth keeping. A lease upload is not: a
//! missed lease looks like a dummy to the server, and the next wake posts a fresh one. A
//! knock or a Welcome is, until the lease it belongs to ends.
//!
//! The queue itself is pingers-store's `request_queue` table, so it survives a restart and
//! is shared with the app's other processes; the engine reaches it through one session per
//! step.

use pingers_time::UnixSeconds;

/// How many requests the queue holds; when full the oldest is dropped. pingers-store owns
/// it now that the queue is a table; re-exported so callers keep this path.
pub use pingers_store::QueueCapacity;

use crate::WakeBudget;

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
