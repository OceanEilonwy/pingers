// SPDX-License-Identifier: AGPL-3.0-only
//! A fixed start time and helpers for moving a [`ManualClock`].

use pingers_time::{Clock as _, ManualClock, UnixSeconds};

/// The wall time tests start at unless they need another: 2025-11-13T02:15:00Z, the first
/// second of bucket 1,958,889. Starting on a bucket boundary keeps "15 minutes later" in the
/// next bucket, which most lease and tag tests want.
pub const TEST_START: UnixSeconds = UnixSeconds::new(1_763_000_100);

/// Why the clock could not be moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClockError {
    /// The target is earlier than the clock's wall time. Use [`ManualClock::set_wall`] to model
    /// a wall-clock correction; monotonic time never goes back.
    #[error("cannot advance from {now:?} back to {target:?}")]
    WouldGoBack {
        /// The clock's wall time.
        now: UnixSeconds,
        /// The requested time.
        target: UnixSeconds,
    },
}

/// A [`ManualClock`] at [`TEST_START`].
#[must_use]
pub fn test_clock() -> ManualClock {
    ManualClock::new(TEST_START)
}

/// Advances wall and monotonic time together until the wall clock reads `target`, waking any
/// sleeper whose deadline passes. Reaching the current time is a no-op.
///
/// # Errors
///
/// [`ClockError::WouldGoBack`] if `target` is earlier than the clock's wall time.
pub fn advance_to(clock: &ManualClock, target: UnixSeconds) -> Result<(), ClockError> {
    let now = clock.now();
    if target < now {
        return Err(ClockError::WouldGoBack { now, target });
    }
    clock.advance(target.saturating_duration_since(now));
    Ok(())
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use core::time::Duration;

    use pingers_time::MonotonicTime;

    use super::*;

    #[test]
    fn test_start_is_a_bucket_boundary() {
        assert_eq!(TEST_START.bucket().start(), TEST_START);
        assert_eq!(test_clock().now(), TEST_START);
    }

    #[test]
    fn advance_to_moves_wall_and_monotonic_time() -> Result<(), ClockError> {
        let clock = test_clock();
        let target = UnixSeconds::new(1_763_000_190);
        advance_to(&clock, target)?;
        assert_eq!(clock.now(), target);
        assert_eq!(
            pingers_time::Timer::now(&clock),
            MonotonicTime::from_elapsed(Duration::from_secs(90))
        );
        Ok(())
    }

    #[test]
    fn advance_to_the_present_is_allowed_and_changes_nothing() -> Result<(), ClockError> {
        let clock = test_clock();
        advance_to(&clock, TEST_START)?;
        assert_eq!(clock.now(), TEST_START);
        assert_eq!(pingers_time::Timer::now(&clock), MonotonicTime::ORIGIN);
        Ok(())
    }

    #[test]
    fn advance_to_the_past_is_refused() {
        let clock = test_clock();
        let earlier = UnixSeconds::new(1_763_000_099);
        assert_eq!(
            advance_to(&clock, earlier),
            Err(ClockError::WouldGoBack {
                now: TEST_START,
                target: earlier
            })
        );
        assert_eq!(clock.now(), TEST_START);
    }
}
