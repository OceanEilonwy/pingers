// SPDX-License-Identifier: AGPL-3.0-only
//! Request totals by channel and outcome: no paths, identifiers, addresses or timings of single
//! requests (style guide §9).

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::http::StatusCode;

use crate::route::Channel;

/// What became of a request, coarsely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// 2xx.
    Ok,
    /// 4xx: refused, malformed, or on the wrong channel.
    ClientError,
    /// 501: the route's crate is not wired yet.
    NotImplemented,
    /// Any other status.
    ServerError,
}

impl Outcome {
    fn of(status: StatusCode) -> Self {
        if status.is_success() {
            Self::Ok
        } else if status.is_client_error() {
            Self::ClientError
        } else if status == StatusCode::NOT_IMPLEMENTED {
            Self::NotImplemented
        } else {
            Self::ServerError
        }
    }
}

#[derive(Debug, Default)]
struct ChannelCounts {
    ok: AtomicU64,
    client_error: AtomicU64,
    not_implemented: AtomicU64,
    server_error: AtomicU64,
}

impl ChannelCounts {
    fn counter(&self, outcome: Outcome) -> &AtomicU64 {
        match outcome {
            Outcome::Ok => &self.ok,
            Outcome::ClientError => &self.client_error,
            Outcome::NotImplemented => &self.not_implemented,
            Outcome::ServerError => &self.server_error,
        }
    }
}

/// The server's counters.
#[derive(Debug, Default)]
pub(crate) struct Metrics {
    direct: ChannelCounts,
    oblivious: ChannelCounts,
    stream: ChannelCounts,
}

const CHANNELS: [(Channel, &str); 3] = [
    (Channel::Direct, "direct"),
    (Channel::Oblivious, "ohttp"),
    (Channel::ObliviousStream, "ohttp_chunked"),
];

const OUTCOMES: [(Outcome, &str); 4] = [
    (Outcome::Ok, "ok"),
    (Outcome::ClientError, "client_error"),
    (Outcome::NotImplemented, "not_implemented"),
    (Outcome::ServerError, "server_error"),
];

impl Metrics {
    fn channel(&self, channel: Channel) -> &ChannelCounts {
        match channel {
            Channel::Direct => &self.direct,
            Channel::Oblivious => &self.oblivious,
            Channel::ObliviousStream => &self.stream,
        }
    }

    /// Counts one answered request.
    pub(crate) fn record(&self, channel: Channel, status: StatusCode) {
        // A counter that wraps after 2^64 requests is still a counter.
        self.channel(channel)
            .counter(Outcome::of(status))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// The counters in the Prometheus text format.
    pub(crate) fn render(&self) -> String {
        let mut text = String::from("# TYPE pingers_requests_total counter\n");
        for (channel, channel_name) in CHANNELS {
            for (outcome, outcome_name) in OUTCOMES {
                let count = self
                    .channel(channel)
                    .counter(outcome)
                    .load(Ordering::Relaxed);
                // Writing to a String cannot fail.
                let _ = writeln!(
                    text,
                    "pingers_requests_total{{channel=\"{channel_name}\",outcome=\"{outcome_name}\"}} {count}"
                );
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::Metrics;
    use crate::route::Channel;

    fn line(metrics: &Metrics, channel: &str, outcome: &str) -> Option<String> {
        let prefix =
            format!("pingers_requests_total{{channel=\"{channel}\",outcome=\"{outcome}\"}} ");
        metrics
            .render()
            .lines()
            .find_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
    }

    #[test]
    fn statuses_land_in_their_outcome_on_their_channel() {
        let metrics = Metrics::default();
        metrics.record(Channel::Direct, StatusCode::OK);
        metrics.record(Channel::Direct, StatusCode::NO_CONTENT);
        metrics.record(Channel::Oblivious, StatusCode::FORBIDDEN);
        metrics.record(Channel::Oblivious, StatusCode::NOT_IMPLEMENTED);
        metrics.record(Channel::ObliviousStream, StatusCode::INTERNAL_SERVER_ERROR);
        let expected = [
            ("direct", "ok", "2"),
            ("direct", "client_error", "0"),
            ("ohttp", "client_error", "1"),
            ("ohttp", "not_implemented", "1"),
            ("ohttp", "ok", "0"),
            ("ohttp_chunked", "server_error", "1"),
            ("ohttp_chunked", "not_implemented", "0"),
        ];
        for (channel, outcome, count) in expected {
            assert_eq!(
                line(&metrics, channel, outcome).as_deref(),
                Some(count),
                "{channel} {outcome}"
            );
        }
    }

    #[test]
    fn rendering_lists_every_counter_once() {
        let rendered = Metrics::default().render();
        assert!(rendered.starts_with("# TYPE pingers_requests_total counter\n"));
        assert_eq!(rendered.lines().count(), 13);
    }
}
