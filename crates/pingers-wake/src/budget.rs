// SPDX-License-Identifier: AGPL-3.0-only
//! The wake budget: the seconds a background wake allows.

use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use core::time::Duration;

use pingers_time::{MonotonicTime, Timer};

/// The time a wake may take, measured on the injected [`Timer`].
pub(crate) struct Budget<'timer, T: Timer> {
    timer: &'timer T,
    deadline: MonotonicTime,
}

impl<'timer, T: Timer> Budget<'timer, T> {
    /// A budget of `allowance` from now.
    pub(crate) fn start(timer: &'timer T, allowance: Duration) -> Self {
        Self {
            timer,
            deadline: timer.now().saturating_add(allowance),
        }
    }

    /// The same budget, for a step that shares the whole of it.
    pub(crate) const fn whole(&self) -> Self {
        Self {
            timer: self.timer,
            deadline: self.deadline,
        }
    }

    /// An equal share of what is left, for one of `shares` steps that run one after another:
    /// time an earlier share does not use flows to the later ones. Never ends after `self`.
    pub(crate) fn share(&self, shares: usize) -> Self {
        let now = self.timer.now();
        let left = self.deadline.saturating_duration_since(now);
        let share = u32::try_from(shares)
            .ok()
            .and_then(|shares| left.checked_div(shares))
            .unwrap_or(left);
        Self {
            timer: self.timer,
            deadline: now.saturating_add(share).min(self.deadline),
        }
    }

    /// Whether the budget is spent.
    pub(crate) fn exhausted(&self) -> bool {
        self.timer.now() >= self.deadline
    }

    /// Runs `future` until it completes or the budget runs out, whichever is first. The
    /// future is dropped (cancelled) when the budget runs out. A spent budget runs nothing.
    pub(crate) async fn within<F: Future>(&self, future: F) -> Option<F::Output> {
        if self.exhausted() {
            return None;
        }
        let mut future = pin!(future);
        let mut deadline = self.timer.sleep_until(self.deadline);
        poll_fn(|context| {
            if let Poll::Ready(output) = future.as_mut().poll(context) {
                return Poll::Ready(Some(output));
            }
            if deadline.as_mut().poll(context).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
    }
}
