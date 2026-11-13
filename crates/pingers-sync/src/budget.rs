// SPDX-License-Identifier: AGPL-3.0-only
//! The wake budget: how long the current wake may still run (design E.2; pingers-wake owns
//! the number, this crate honours it).

use std::time::Duration;

use pingers_time::{MonotonicTime, Timer};

/// The least time a request is started with. An OHTTP exchange through a relay takes a
/// round trip to the relay and one to the gateway; with less than this left it would most
/// likely be cut off after the server had acted on it, which is worse than not sending it.
pub const MIN_REQUEST_TIME: Duration = Duration::from_secs(2);

/// How long the current wake may still run, as a deadline on the injected [`Timer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WakeBudget {
    deadline: Option<MonotonicTime>,
}

impl WakeBudget {
    /// No limit: the app is in the foreground. Requests still time out in the transport.
    pub const UNLIMITED: Self = Self { deadline: None };

    /// `left` from now, as the OS reported it for this wake.
    #[must_use]
    pub fn from_now(timer: &dyn Timer, left: Duration) -> Self {
        Self {
            deadline: Some(timer.now().saturating_add(left)),
        }
    }

    /// The deadline, if there is one.
    #[must_use]
    pub const fn deadline(self) -> Option<MonotonicTime> {
        self.deadline
    }

    /// Time left, or `None` without a limit.
    #[must_use]
    pub fn remaining(self, timer: &dyn Timer) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(timer.now()))
    }

    /// Whether a new request may still be started: at least [`MIN_REQUEST_TIME`] is left.
    #[must_use]
    pub fn allows_request(self, timer: &dyn Timer) -> bool {
        self.remaining(timer)
            .is_none_or(|remaining| remaining >= MIN_REQUEST_TIME)
    }
}

#[cfg(test)]
mod tests {
    use pingers_time::{ManualClock, UnixSeconds};

    use super::*;

    #[test]
    fn a_request_starts_only_with_the_minimum_time_left() {
        let clock = ManualClock::new(UnixSeconds::new(1_000_000));
        let budget = WakeBudget::from_now(&clock, Duration::from_secs(5));
        assert!(budget.allows_request(&clock));
        clock.advance(Duration::from_secs(3));
        assert_eq!(budget.remaining(&clock), Some(MIN_REQUEST_TIME));
        assert!(
            budget.allows_request(&clock),
            "exactly the minimum is enough"
        );
        clock.advance(Duration::from_millis(1));
        assert!(!budget.allows_request(&clock));
        clock.advance(Duration::from_secs(10));
        assert_eq!(budget.remaining(&clock), Some(Duration::ZERO));
    }

    #[test]
    fn unlimited_never_runs_out() {
        let clock = ManualClock::new(UnixSeconds::new(1_000_000));
        clock.advance(Duration::from_hours(24));
        assert!(WakeBudget::UNLIMITED.allows_request(&clock));
        assert_eq!(WakeBudget::UNLIMITED.remaining(&clock), None);
        assert_eq!(WakeBudget::UNLIMITED.deadline(), None);
    }
}
