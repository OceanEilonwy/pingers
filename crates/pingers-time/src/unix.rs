// SPDX-License-Identifier: AGPL-3.0-only
//! Wall-clock instants.

use core::time::Duration;

use crate::Bucket;

/// A wall-clock instant: whole seconds since 1970-01-01T00:00:00Z.
///
/// This is the unit of every expiry, lease start and `server_time` in the protocol. It has no
/// sub-second precision because nothing in the design needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixSeconds(u64);

impl UnixSeconds {
    /// The Unix epoch itself.
    pub const EPOCH: Self = Self(0);

    /// Wraps a count of seconds since the Unix epoch, for example one read from the wire.
    #[must_use]
    pub const fn new(seconds: u64) -> Self {
        Self(seconds)
    }

    /// The count of seconds since the Unix epoch, for encoding.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The bucket this instant falls in.
    #[must_use]
    pub const fn bucket(self) -> Bucket {
        Bucket::containing(self)
    }

    /// This instant moved forward by `duration` (whole seconds; sub-second parts are dropped),
    /// or `None` if that is past the end of time.
    #[must_use]
    pub const fn checked_add(self, duration: Duration) -> Option<Self> {
        match self.0.checked_add(duration.as_secs()) {
            Some(seconds) => Some(Self(seconds)),
            None => None,
        }
    }

    /// This instant moved back by `duration` (whole seconds), or `None` if that is before the
    /// epoch.
    #[must_use]
    pub const fn checked_sub(self, duration: Duration) -> Option<Self> {
        match self.0.checked_sub(duration.as_secs()) {
            Some(seconds) => Some(Self(seconds)),
            None => None,
        }
    }

    /// How long after `earlier` this instant is, or zero if it is not after it.
    #[must_use]
    pub const fn saturating_duration_since(self, earlier: Self) -> Duration {
        Duration::from_secs(self.0.saturating_sub(earlier.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_past_the_end_of_time_is_none() {
        assert_eq!(
            UnixSeconds::new(u64::MAX).checked_add(Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn subtracting_before_the_epoch_is_none() {
        assert_eq!(UnixSeconds::EPOCH.checked_sub(Duration::from_secs(1)), None);
    }

    #[test]
    fn subtracting_within_range_moves_back() {
        let earlier = UnixSeconds::new(10).checked_sub(Duration::from_secs(3));
        assert_eq!(earlier, Some(UnixSeconds::new(7)));
        let epoch = UnixSeconds::new(10).checked_sub(Duration::from_secs(10));
        assert_eq!(epoch, Some(UnixSeconds::EPOCH));
    }

    #[test]
    fn adding_drops_sub_second_parts() {
        let later = UnixSeconds::new(10).checked_add(Duration::from_millis(1_999));
        assert_eq!(later, Some(UnixSeconds::new(11)));
    }

    #[test]
    fn duration_since_a_later_instant_is_zero() {
        let earlier = UnixSeconds::new(5);
        let later = UnixSeconds::new(8);
        assert_eq!(earlier.saturating_duration_since(later), Duration::ZERO);
        assert_eq!(
            later.saturating_duration_since(earlier),
            Duration::from_secs(3)
        );
    }
}
