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
