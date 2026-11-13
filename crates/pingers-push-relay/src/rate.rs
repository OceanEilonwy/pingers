// SPDX-License-Identifier: AGPL-3.0-only
//! In-memory token-bucket rate limits, per Pingers server and per push token (design C.1a).
//!
//! Integer arithmetic only: a bucket holds `per_minute * 60` units, refills `per_minute`
//! units each second and a push costs 60 units. A full bucket allows a burst of `per_minute`
//! pushes. Buckets are forgotten once they have refilled, so memory follows recent traffic.

use std::collections::HashMap;
use std::hash::Hash;
use std::num::NonZeroU32;

use pingers_time::UnixSeconds;

/// Units one push costs; also the number of seconds in the refill period.
const UNITS_PER_PUSH: u64 = 60;

/// A limit of so many pushes per minute, with bursts up to the same number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimit {
    per_minute: NonZeroU32,
}

impl RateLimit {
    /// A limit of `per_minute` pushes per minute.
    #[must_use]
    pub const fn per_minute(per_minute: NonZeroU32) -> Self {
        Self { per_minute }
    }

    fn capacity(self) -> u64 {
        u64::from(self.per_minute.get()).saturating_mul(UNITS_PER_PUSH)
    }

    fn refill_per_second(self) -> u64 {
        u64::from(self.per_minute.get())
    }
}

/// Whether a push may go ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Within the limit; one push has been counted.
    Allowed,
    /// Over the limit; nothing was counted.
    Limited,
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    units: u64,
    updated: UnixSeconds,
}

impl Bucket {
    fn refilled(self, limit: RateLimit, now: UnixSeconds) -> u64 {
        let seconds = now.saturating_duration_since(self.updated).as_secs();
        self.units
            .saturating_add(seconds.saturating_mul(limit.refill_per_second()))
            .min(limit.capacity())
    }
}

/// Buckets keyed by `K`, holding at most `max_entries` partly drained buckets.
#[derive(Debug)]
pub(crate) struct Limiter<K> {
    buckets: HashMap<K, Bucket>,
    max_entries: usize,
}

impl<K: Hash + Eq> Limiter<K> {
    /// A limiter that tracks at most `max_entries` keys. When that many buckets are partly
    /// drained at once, new keys are refused until some refill: failing closed keeps memory
    /// bounded under a flood of distinct tokens.
    pub(crate) fn new(max_entries: usize) -> Self {
        Self {
            buckets: HashMap::new(),
            max_entries,
        }
    }

    /// Counts one push for `key` if `limit` allows it at `now`.
    pub(crate) fn try_acquire(&mut self, key: K, limit: RateLimit, now: UnixSeconds) -> Decision {
        if !self.buckets.contains_key(&key) && self.buckets.len() >= self.max_entries {
            self.forget_full(limit, now);
            if self.buckets.len() >= self.max_entries {
                return Decision::Limited;
            }
        }
        let bucket = self.buckets.entry(key).or_insert(Bucket {
            units: limit.capacity(),
            updated: now,
        });
        let units = bucket.refilled(limit, now);
        match units.checked_sub(UNITS_PER_PUSH) {
            Some(remaining) => {
                *bucket = Bucket {
                    units: remaining,
                    updated: now,
                };
                Decision::Allowed
            }
            None => Decision::Limited,
        }
    }

    /// Drops buckets that have refilled completely: a missing bucket means the same.
    fn forget_full(&mut self, limit: RateLimit, now: UnixSeconds) {
        self.buckets
            .retain(|_, bucket| bucket.refilled(limit, now) < limit.capacity());
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn limit(per_minute: u32) -> RateLimit {
        RateLimit::per_minute(NonZeroU32::MIN.saturating_add(per_minute.saturating_sub(1)))
    }

    fn at(seconds: u64) -> UnixSeconds {
        UnixSeconds::new(seconds)
    }

    fn drain(limiter: &mut Limiter<u8>, key: u8, limit: RateLimit, now: UnixSeconds) -> usize {
        std::iter::repeat_with(|| limiter.try_acquire(key, limit, now))
            .take_while(|decision| *decision == Decision::Allowed)
            .count()
    }

    #[test]
    fn a_fresh_key_may_burst_up_to_the_per_minute_limit() {
        let mut limiter = Limiter::new(10);
        assert_eq!(drain(&mut limiter, 1, limit(5), at(1000)), 5);
    }

    #[test]
    fn refill_restores_one_push_per_sixtieth_of_a_minute_per_unit_of_limit() {
        let mut limiter = Limiter::new(10);
        let limit = limit(6);
        drain(&mut limiter, 1, limit, at(1000));
        // 6 per minute: one push every 10 seconds.
        assert_eq!(limiter.try_acquire(1, limit, at(1009)), Decision::Limited);
        assert_eq!(limiter.try_acquire(1, limit, at(1010)), Decision::Allowed);
        assert_eq!(limiter.try_acquire(1, limit, at(1010)), Decision::Limited);
    }

    #[test]
    fn a_refused_push_does_not_drain_the_bucket() {
        let mut limiter = Limiter::new(10);
        let limit = limit(6);
        drain(&mut limiter, 1, limit, at(1000));
        for second in 1000..1010 {
            assert_eq!(limiter.try_acquire(1, limit, at(second)), Decision::Limited);
        }
        assert_eq!(limiter.try_acquire(1, limit, at(1010)), Decision::Allowed);
    }

    #[test]
    fn refill_stops_at_the_burst_size() {
        let mut limiter = Limiter::new(10);
        let limit = limit(3);
        drain(&mut limiter, 1, limit, at(1000));
        assert_eq!(drain(&mut limiter, 1, limit, at(1_000_000)), 3);
    }

    #[test]
    fn keys_are_limited_independently() {
        let mut limiter = Limiter::new(10);
        let limit = limit(2);
        drain(&mut limiter, 1, limit, at(1000));
        assert_eq!(limiter.try_acquire(2, limit, at(1000)), Decision::Allowed);
    }

    #[test]
    fn a_clock_that_steps_back_refills_nothing() {
        let mut limiter = Limiter::new(10);
        let limit = limit(2);
        drain(&mut limiter, 1, limit, at(1000));
        assert_eq!(limiter.try_acquire(1, limit, at(10)), Decision::Limited);
    }

    #[test]
    fn when_full_refilled_buckets_make_room_for_new_keys() {
        let mut limiter = Limiter::new(2);
        let limit = limit(60);
        limiter.try_acquire(1, limit, at(1000));
        limiter.try_acquire(2, limit, at(1000));
        // One second later both buckets are full again and are forgotten.
        assert_eq!(limiter.try_acquire(3, limit, at(1001)), Decision::Allowed);
        assert_eq!(limiter.tracked(), 1);
    }

    #[test]
    fn when_full_of_drained_buckets_new_keys_are_refused() {
        let mut limiter = Limiter::new(2);
        let limit = limit(60);
        limiter.try_acquire(1, limit, at(1000));
        limiter.try_acquire(2, limit, at(1000));
        assert_eq!(limiter.try_acquire(3, limit, at(1000)), Decision::Limited);
        // Known keys still get their own budget.
        assert_eq!(limiter.try_acquire(1, limit, at(1000)), Decision::Allowed);
    }
}
