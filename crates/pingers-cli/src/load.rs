// SPDX-License-Identifier: AGPL-3.0-only
//! Load generation plans for the board-size and server-cost experiments (design H.1 #6, H.3
//! #11): how many devices keep how many records on the board, and how often they knock.

use core::fmt;
use core::num::NonZeroU32;
use core::time::Duration;

/// Records one lease holds: one real or dummy record per slot (design B.4.2).
const RECORDS_PER_LEASE: u32 = 4;

/// Seconds in a bucket (design F.1).
const BUCKET_SECONDS: u64 = 900;

/// A validated load plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LoadPlan {
    /// Live records to keep on the board (design blocker 6 measures 4,000 and 20,000).
    pub(crate) board_records: NonZeroU32,
    /// Knocks per minute across all devices.
    pub(crate) knocks_per_minute: u32,
    /// How long to run.
    pub(crate) duration: Duration,
    /// Seed for every device's randomness.
    pub(crate) seed: u64,
}

impl LoadPlan {
    /// Simulated devices: one lease each, enough leases to hold the board.
    pub(crate) fn devices(&self) -> u32 {
        self.board_records.get().div_ceil(RECORDS_PER_LEASE)
    }

    /// Buckets the run touches, counting a partial bucket.
    pub(crate) fn buckets(&self) -> u64 {
        self.duration.as_secs().div_ceil(BUCKET_SECONDS)
    }

    /// Knocks sent over the run, counting a partial minute.
    pub(crate) fn knocks(&self) -> u64 {
        let minutes = self.duration.as_secs().div_ceil(60);
        u64::from(self.knocks_per_minute).saturating_mul(minutes)
    }
}

impl fmt::Display for LoadPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "load plan: {} board records from {} devices over {} s ({} buckets), {} knocks ({} per minute), seed {}",
            self.board_records,
            self.devices(),
            self.duration.as_secs(),
            self.buckets(),
            self.knocks(),
            self.knocks_per_minute,
            self.seed
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(records: u32, knocks_per_minute: u32, seconds: u64) -> Option<LoadPlan> {
        Some(LoadPlan {
            board_records: NonZeroU32::new(records)?,
            knocks_per_minute,
            duration: Duration::from_secs(seconds),
            seed: 1,
        })
    }

    #[test]
    fn devices_round_up_to_whole_leases() {
        assert_eq!(plan(4_000, 0, 60).map(|plan| plan.devices()), Some(1_000));
        assert_eq!(plan(4_001, 0, 60).map(|plan| plan.devices()), Some(1_001));
        assert_eq!(plan(1, 0, 60).map(|plan| plan.devices()), Some(1));
    }

    #[test]
    fn partial_buckets_and_minutes_count() {
        let plan = plan(4, 10, 901);
        assert_eq!(plan.map(|plan| plan.buckets()), Some(2));
        assert_eq!(plan.map(|plan| plan.knocks()), Some(160));
        let exact = self::plan(4, 10, 900);
        assert_eq!(exact.map(|plan| plan.buckets()), Some(1));
        assert_eq!(exact.map(|plan| plan.knocks()), Some(150));
    }

    #[test]
    fn the_summary_states_the_derived_numbers() {
        assert_eq!(
            plan(20_000, 30, 1_800).map(|plan| plan.to_string()),
            Some(String::from(
                "load plan: 20000 board records from 5000 devices over 1800 s (2 buckets), 900 knocks (30 per minute), seed 1"
            ))
        );
    }
}
