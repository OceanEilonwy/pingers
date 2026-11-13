// SPDX-License-Identifier: AGPL-3.0-only
//! Aggregate counters: the only thing the host ever reports, and only when the operator turns
//! counts logging on. No addresses, paths, regions or times of single requests: which region a
//! given client downloads is exactly what the host should not keep.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use pingers_time::Timer;

/// Running totals since the last report.
#[derive(Debug, Default)]
pub struct Counts {
    success: AtomicU64,
    redirection: AtomicU64,
    client_error: AtomicU64,
    server_error: AtomicU64,
    body_bytes: AtomicU64,
    cuts: AtomicU64,
    failed_cuts: AtomicU64,
    evictions: AtomicU64,
}

/// Totals taken by [`Counts::take`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CountsSnapshot {
    /// Answered 2xx (200 and 206).
    pub success: u64,
    /// Answered 3xx (304).
    pub redirection: u64,
    /// Answered 4xx (404, 405, 416), and the rare 1xx.
    pub client_error: u64,
    /// Answered 5xx.
    pub server_error: u64,
    /// Body bytes promised in `Content-Length` of GET answers (a client that disconnects early
    /// receives fewer).
    pub body_bytes: u64,
    /// Cache mode: regions cut.
    pub cuts: u64,
    /// Cache mode: cuts that failed.
    pub failed_cuts: u64,
    /// Cache mode: regions evicted.
    pub evictions: u64,
}

impl Counts {
    /// Counts one answer with `status` and a body of `body_bytes`.
    pub fn record(&self, status: StatusCode, body_bytes: u64) {
        let counter = if status.is_success() {
            &self.success
        } else if status.is_redirection() {
            &self.redirection
        } else if status.is_server_error() {
            &self.server_error
        } else {
            &self.client_error
        };
        // Wrapping after 2^64 is harmless; fetch_add wraps by definition.
        counter.fetch_add(1, Ordering::Relaxed);
        self.body_bytes.fetch_add(body_bytes, Ordering::Relaxed);
    }

    /// Counts one finished cut.
    pub fn record_cut(&self, succeeded: bool) {
        let counter = if succeeded {
            &self.cuts
        } else {
            &self.failed_cuts
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one evicted region.
    pub fn record_eviction(&self) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns the totals and resets them to zero.
    pub fn take(&self) -> CountsSnapshot {
        let take = |counter: &AtomicU64| counter.swap(0, Ordering::Relaxed);
        CountsSnapshot {
            success: take(&self.success),
            redirection: take(&self.redirection),
            client_error: take(&self.client_error),
            server_error: take(&self.server_error),
            body_bytes: take(&self.body_bytes),
            cuts: take(&self.cuts),
            failed_cuts: take(&self.failed_cuts),
            evictions: take(&self.evictions),
        }
    }
}

impl fmt::Display for CountsSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "2xx={} 3xx={} 4xx={} 5xx={} body_bytes={} cuts={} failed_cuts={} evictions={}",
            self.success,
            self.redirection,
            self.client_error,
            self.server_error,
            self.body_bytes,
            self.cuts,
            self.failed_cuts,
            self.evictions,
        )
    }
}

/// Every `interval`, hands the totals since the last report to `sink`. Runs until dropped.
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

    #[test]
    fn statuses_are_counted_by_class() {
        let counts = Counts::default();
        counts.record(StatusCode::OK, 10);
        counts.record(StatusCode::PARTIAL_CONTENT, 5);
        counts.record(StatusCode::NOT_MODIFIED, 0);
        counts.record(StatusCode::NOT_FOUND, 0);
        counts.record(StatusCode::RANGE_NOT_SATISFIABLE, 0);
        counts.record(StatusCode::INTERNAL_SERVER_ERROR, 0);
        counts.record_cut(true);
        counts.record_cut(false);
        counts.record_eviction();
        assert_eq!(
            counts.take().to_string(),
            "2xx=2 3xx=1 4xx=2 5xx=1 body_bytes=15 cuts=1 failed_cuts=1 evictions=1"
        );
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
                .push(snapshot.success);
        });
        let driver = async {
            let report_count = || reports.lock().unwrap_or_else(PoisonError::into_inner).len();
            tokio::task::yield_now().await;
            counts.record(StatusCode::OK, 1);
            clock.advance(Duration::from_secs(59));
            tokio::task::yield_now().await;
            assert_eq!(report_count(), 0);
            clock.advance(Duration::from_secs(1));
            tokio::task::yield_now().await;
            assert_eq!(report_count(), 1);
            clock.advance(Duration::from_secs(60));
            tokio::task::yield_now().await;
        };
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
