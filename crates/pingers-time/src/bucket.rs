// SPDX-License-Identifier: AGPL-3.0-only
//! The 15-minute bucket.

use core::time::Duration;

use crate::{TimeError, UnixSeconds};

/// Seconds in one bucket (design F.1: `T = 900 s`).
const BUCKET_SECONDS: u64 = 900;

/// The largest index whose start still fits in a `u64` of seconds.
const MAX_INDEX: u64 = u64::MAX / BUCKET_SECONDS;

/// A 15-minute window of wall time: `b = floor(unix_seconds / 900)` (design §0).
///
/// Leases start at a bucket and live for four; tags, seal keys and the meetup PSK are derived
/// from one; rate-limit nullifiers are counted per bucket. The bucket bounds replay and sets
/// lifetimes; it does not provide unlinkability (design B.4.1).
///
/// Invariant: the bucket's start is representable as [`UnixSeconds`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bucket(u64);

impl Bucket {
    /// The length of one bucket.
    pub const LENGTH: Duration = Duration::from_secs(BUCKET_SECONDS);

    /// The bucket that `instant` falls in.
    #[must_use]
    pub const fn containing(instant: UnixSeconds) -> Self {
        Self(instant.get() / BUCKET_SECONDS)
    }

    /// The bucket with this index, for example `b0` read from a lease on the wire.
    ///
    /// # Errors
    ///
    /// [`TimeError::BucketOutOfRange`] if the bucket's start would not fit in [`UnixSeconds`].
    pub const fn from_index(index: u64) -> Result<Self, TimeError> {
        if index > MAX_INDEX {
            Err(TimeError::BucketOutOfRange)
        } else {
            Ok(Self(index))
        }
    }

    /// The bucket index `b`, for encoding.
    #[must_use]
    pub const fn index(self) -> u64 {
        self.0
    }

    /// The index as eight big-endian bytes: the `be64(b)` of the derivations in design B.4.1.
    #[must_use]
    pub const fn to_be_bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    /// The first second of the bucket.
    #[must_use]
    pub const fn start(self) -> UnixSeconds {
        // Cannot saturate: the invariant keeps `index * 900` within `u64`.
        UnixSeconds::new(self.0.saturating_mul(BUCKET_SECONDS))
    }

    /// The first second after the bucket, or `None` for the last representable bucket.
    #[must_use]
    pub const fn end(self) -> Option<UnixSeconds> {
        match self.next() {
            Some(next) => Some(next.start()),
            None => None,
        }
    }

    /// Whether `instant` falls in this bucket.
    #[must_use]
    pub const fn contains(self, instant: UnixSeconds) -> bool {
        Self::containing(instant).0 == self.0
    }

    /// The following bucket, or `None` past the representable range.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        self.checked_add(BucketCount::ONE)
    }

    /// The preceding bucket, or `None` before the epoch.
    #[must_use]
    pub const fn previous(self) -> Option<Self> {
        self.checked_sub(BucketCount::ONE)
    }

    /// The bucket `count` buckets later, or `None` past the representable range.
    #[must_use]
    pub const fn checked_add(self, count: BucketCount) -> Option<Self> {
        match self.0.checked_add(count.get()) {
            Some(index) if index <= MAX_INDEX => Some(Self(index)),
            _ => None,
        }
    }

    /// The bucket `count` buckets earlier, or `None` before the epoch.
    #[must_use]
    pub const fn checked_sub(self, count: BucketCount) -> Option<Self> {
        match self.0.checked_sub(count.get()) {
            Some(index) => Some(Self(index)),
            None => None,
        }
    }

    /// How many buckets after `earlier` this one is, or zero if it is not after it.
    ///
    /// A lease starting at `b0` is live at `now` while
    /// `now.saturating_buckets_since(b0) < 4` (design B.4.2).
    #[must_use]
    pub const fn saturating_buckets_since(self, earlier: Self) -> BucketCount {
        BucketCount::new(self.0.saturating_sub(earlier.0))
    }
}

/// A number of buckets: a span, not a position (see [`Bucket`] for positions).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BucketCount(u64);

impl BucketCount {
    /// No buckets.
    pub const ZERO: Self = Self(0);

    /// One bucket.
    pub const ONE: Self = Self(1);

    /// Wraps a number of buckets.
    #[must_use]
    pub const fn new(count: u64) -> Self {
        Self(count)
    }

    /// The number of buckets.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use proptest::prelude::*;

    use super::*;

    #[test]
    fn bucket_boundaries_fall_on_multiples_of_900() {
        assert_eq!(Bucket::containing(UnixSeconds::new(899)).index(), 0);
        assert_eq!(Bucket::containing(UnixSeconds::new(900)).index(), 1);
        assert_eq!(Bucket::containing(UnixSeconds::new(1_799)).index(), 1);
    }

    #[test]
    fn index_beyond_range_is_rejected() {
        assert_eq!(
            Bucket::from_index(MAX_INDEX + 1),
            Err(TimeError::BucketOutOfRange)
        );
    }

    #[test]
    fn last_bucket_has_no_next_or_end() -> Result<(), TimeError> {
        let last = Bucket::from_index(MAX_INDEX)?;
        assert_eq!(last.next(), None);
        assert_eq!(last.end(), None);
        Ok(())
    }

    #[test]
    fn first_bucket_has_no_previous() {
        assert_eq!(Bucket::containing(UnixSeconds::EPOCH).previous(), None);
    }

    #[test]
    fn index_encodes_big_endian() -> Result<(), TimeError> {
        assert_eq!(
            Bucket::from_index(0x0102)?.to_be_bytes(),
            [0, 0, 0, 0, 0, 0, 1, 2]
        );
        Ok(())
    }

    #[test]
    fn buckets_since_a_later_bucket_is_zero() -> Result<(), TimeError> {
        let early = Bucket::from_index(10)?;
        let late = Bucket::from_index(13)?;
        assert_eq!(early.saturating_buckets_since(late), BucketCount::ZERO);
        assert_eq!(late.saturating_buckets_since(early), BucketCount::new(3));
        Ok(())
    }

    proptest! {
        #[test]
        fn every_instant_lies_in_its_bucket(seconds in any::<u64>()) {
            let instant = UnixSeconds::new(seconds);
            let bucket = instant.bucket();
            prop_assert!(bucket.start() <= instant);
            prop_assert!(bucket.contains(instant));
            if let Some(end) = bucket.end() {
                prop_assert!(instant < end);
                prop_assert_eq!(end.saturating_duration_since(bucket.start()), Bucket::LENGTH);
            }
        }

        #[test]
        fn index_round_trips(index in 0..=MAX_INDEX) {
            prop_assert_eq!(Bucket::from_index(index).map(Bucket::index), Ok(index));
        }
    }
}
