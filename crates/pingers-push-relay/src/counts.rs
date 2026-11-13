// SPDX-License-Identifier: AGPL-3.0-only
//! Outcome counters: the only thing the relay ever reports, and only when the operator turns
//! counts logging on (design A.2, C.1a). No tokens, servers, times of single requests or
//! addresses: an operator who logged those would become the timing adversary of A.2.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pingers_time::Timer;

use crate::service::PushOutcome;

/// Running totals of push outcomes since the last report.
#[derive(Debug, Default)]
pub struct Counts {
    accepted: AtomicU64,
    token_gone: AtomicU64,
    does_not_open: AtomicU64,
    rate_limited: AtomicU64,
    unauthorized: AtomicU64,
    bad_request: AtomicU64,
    platform_unsupported: AtomicU64,
    upstream_failed: AtomicU64,
    upstream_timed_out: AtomicU64,
    internal: AtomicU64,
}

/// Totals taken by [`Counts::take`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CountsSnapshot {
    /// Pushes handed to a push service.
    pub accepted: u64,
    /// Answered 410: token invalid, expired or unusable.
    pub token_gone: u64,
    /// Answered 422: blob does not open under the bind.
    pub does_not_open: u64,
    /// Answered 429.
    pub rate_limited: u64,
    /// Answered 401.
    pub unauthorized: u64,
    /// Answered 400.
    pub bad_request: u64,
    /// Answered 501: platform not enabled here.
    pub platform_unsupported: u64,
    /// Answered 502.
    pub upstream_failed: u64,
    /// Answered 504.
    pub upstream_timed_out: u64,
    /// Answered 500.
    pub internal: u64,
}

impl Counts {
    /// Counts one outcome.
    pub fn record(&self, outcome: PushOutcome) {
        let counter = match outcome {
            PushOutcome::Accepted => &self.accepted,
            PushOutcome::TokenGone => &self.token_gone,
            PushOutcome::DoesNotOpen => &self.does_not_open,
            PushOutcome::RateLimited => &self.rate_limited,
            PushOutcome::Unauthorized => &self.unauthorized,
            PushOutcome::BadRequest => &self.bad_request,
            PushOutcome::PlatformUnsupported => &self.platform_unsupported,
            PushOutcome::UpstreamFailed => &self.upstream_failed,
            PushOutcome::UpstreamTimedOut => &self.upstream_timed_out,
            PushOutcome::Internal => &self.internal,
        };
        // A counter wrapping after 2^64 pushes is harmless; fetch_add wraps by definition.
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns the totals and resets them to zero.
    pub fn take(&self) -> CountsSnapshot {
        let take = |counter: &AtomicU64| counter.swap(0, Ordering::Relaxed);
        CountsSnapshot {
            accepted: take(&self.accepted),
            token_gone: take(&self.token_gone),
            does_not_open: take(&self.does_not_open),
            rate_limited: take(&self.rate_limited),
            unauthorized: take(&self.unauthorized),
            bad_request: take(&self.bad_request),
            platform_unsupported: take(&self.platform_unsupported),
            upstream_failed: take(&self.upstream_failed),
            upstream_timed_out: take(&self.upstream_timed_out),
            internal: take(&self.internal),
        }
    }
}

impl fmt::Display for CountsSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "accepted={} gone={} does_not_open={} rate_limited={} unauthorized={} \
             bad_request={} unsupported={} upstream_failed={} upstream_timeout={} internal={}",
            self.accepted,
            self.token_gone,
            self.does_not_open,
            self.rate_limited,
            self.unauthorized,
            self.bad_request,
            self.platform_unsupported,
            self.upstream_failed,
            self.upstream_timed_out,
            self.internal,
        )
    }
}

/// Every `interval`, hands the totals since the last report to `sink`. Runs until dropped;
/// cancellation-safe (dropping it between reports loses nothing but the pending totals).
pub async fn report_counts<T: Timer>(
    counts: &Counts,
    timer: &T,
    interval: Duration,
    mut sink: impl FnMut(CountsSnapshot),
) {
    loop {
        timer.sleep(interval).await;
        sink(counts.take());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use pingers_testkit::ManualClock;
    use pingers_time::UnixSeconds;

    use super::*;

    const ALL: [PushOutcome; 10] = [
        PushOutcome::Accepted,
        PushOutcome::TokenGone,
        PushOutcome::DoesNotOpen,
        PushOutcome::RateLimited,
        PushOutcome::Unauthorized,
        PushOutcome::BadRequest,
        PushOutcome::PlatformUnsupported,
        PushOutcome::UpstreamFailed,
        PushOutcome::UpstreamTimedOut,
        PushOutcome::Internal,
    ];

    #[test]
    fn each_outcome_has_its_own_counter() {
        let counts = Counts::default();
        for (times, outcome) in ALL.iter().enumerate() {
            for _ in 0..=times {
                counts.record(*outcome);
            }
        }
        assert_eq!(
            counts.take().to_string(),
            "accepted=1 gone=2 does_not_open=3 rate_limited=4 unauthorized=5 bad_request=6 \
             unsupported=7 upstream_failed=8 upstream_timeout=9 internal=10"
        );
    }

    #[test]
    fn take_resets_the_totals() {
        let counts = Counts::default();
        counts.record(PushOutcome::Accepted);
        let _first = counts.take();
        assert_eq!(counts.take(), CountsSnapshot::default());
    }

    #[tokio::test]
    async fn reports_arrive_once_per_interval() {
        let clock = ManualClock::new(UnixSeconds::new(0));
        let counts = Counts::default();
        let reports = Arc::new(Mutex::new(Vec::new()));
        let sink_reports = Arc::clone(&reports);
        let reporter = report_counts(&counts, &clock, Duration::from_secs(60), move |snapshot| {
            sink_reports
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(snapshot.accepted);
        });
        let driver = async {
            let report_count = || reports.lock().unwrap_or_else(PoisonError::into_inner).len();
            tokio::task::yield_now().await;
            counts.record(PushOutcome::Accepted);
            clock.advance(Duration::from_secs(59));
            tokio::task::yield_now().await;
            assert_eq!(report_count(), 0);
            clock.advance(Duration::from_secs(1));
            tokio::task::yield_now().await;
            assert_eq!(report_count(), 1);
            clock.advance(Duration::from_secs(60));
            tokio::task::yield_now().await;
        };
        // Biased: the reporter is polled before the driver on every wake-up.
        tokio::select! {
            biased;
            () = reporter => {}
            () = driver => {}
        }
        assert_eq!(
            *reports.lock().unwrap_or_else(PoisonError::into_inner),
            vec![1, 0]
        );
    }
}
