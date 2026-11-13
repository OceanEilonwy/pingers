// SPDX-License-Identifier: AGPL-3.0-only
//! The production [`Timer`] and the deadline helper every network call goes through.

use std::future::Future;
use std::time::Duration;

use pingers_time::{MonotonicTime, Sleep, Timer};

use crate::TransportError;

/// The production [`Timer`], over tokio's clock. Must be created and used inside a tokio
/// runtime. Tests use `pingers_time::ManualClock` instead.
#[derive(Clone, Copy, Debug)]
pub struct TokioTimer {
    origin: tokio::time::Instant,
}

impl TokioTimer {
    /// A timer whose origin is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: tokio_now(),
        }
    }
}

impl Default for TokioTimer {
    fn default() -> Self {
        Self::new()
    }
}

impl Timer for TokioTimer {
    fn now(&self) -> MonotonicTime {
        MonotonicTime::from_elapsed(tokio_now().saturating_duration_since(self.origin))
    }

    fn sleep_until(&self, deadline: MonotonicTime) -> Sleep<'_> {
        let instant = self
            .origin
            .checked_add(deadline.elapsed())
            .unwrap_or_else(|| far_future(self.origin));
        Box::pin(tokio::time::sleep_until(instant))
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "TokioTimer is the one production reader of tokio's monotonic clock"
)]
fn tokio_now() -> tokio::time::Instant {
    tokio::time::Instant::now()
}

/// About thirty years (262 800 hours) after `origin`: a deadline that will not arrive, for overflowing inputs.
fn far_future(origin: tokio::time::Instant) -> tokio::time::Instant {
    origin
        .checked_add(Duration::from_hours(262_800))
        .unwrap_or(origin)
}

/// Runs `future` until `deadline` on `timer`: its output, or [`TransportError::Timeout`].
/// The future is polled first, so a result that is ready at the deadline still counts.
pub(crate) async fn within<F: Future>(
    timer: &dyn Timer,
    deadline: MonotonicTime,
    future: F,
) -> Result<F::Output, TransportError> {
    tokio::select! {
        biased;
        output = future => Ok(output),
        () = timer.sleep_until(deadline) => Err(TransportError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_time::{ManualClock, UnixSeconds};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn sleep_completes_when_tokio_time_advances() {
        let timer = TokioTimer::new();
        let deadline = timer.now().saturating_add(Duration::from_secs(5));
        timer.sleep_until(deadline).await;
        assert!(timer.now() >= deadline);
    }

    #[tokio::test(start_paused = true)]
    async fn an_overflowing_deadline_never_arrives() {
        let timer = TokioTimer::new();
        let never = MonotonicTime::from_elapsed(Duration::MAX);
        let result = within(
            &timer,
            timer.now().saturating_add(Duration::from_secs(1)),
            async {
                timer.sleep_until(never).await;
            },
        )
        .await;
        assert!(matches!(result, Err(TransportError::Timeout)));
    }

    #[tokio::test]
    async fn a_ready_future_wins_even_past_its_deadline() -> Result<(), TransportError> {
        let clock = ManualClock::new(UnixSeconds::EPOCH);
        let value = within(&clock, MonotonicTime::ORIGIN, async { 7 }).await?;
        assert_eq!(value, 7);
        Ok(())
    }

    #[tokio::test]
    async fn a_pending_future_times_out_when_the_timer_reaches_the_deadline() {
        let clock = ManualClock::new(UnixSeconds::EPOCH);
        clock.advance(Duration::from_secs(10));
        let deadline = MonotonicTime::from_elapsed(Duration::from_secs(10));
        let result = within(&clock, deadline, std::future::pending::<()>()).await;
        assert!(matches!(result, Err(TransportError::Timeout)));
    }
}
