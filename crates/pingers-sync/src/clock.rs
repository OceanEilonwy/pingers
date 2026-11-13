// SPDX-License-Identifier: AGPL-3.0-only
//! Clock skew against the server (design F.1): the device clock is corrected by the server's
//! `server_time` only within ±120 s; beyond half a bucket (450 s) the device posts nothing and
//! the owner raises `Notice::ClockSkew`. Between the two the device clock is used unchanged.

use std::time::Duration;

use pingers_time::{Bucket, UnixSeconds};

/// The largest correction applied to the device clock.
pub const MAX_CORRECTION: Duration = Duration::from_secs(120);

/// Skew beyond this stops posting: half a bucket.
pub const MAX_SKEW: Duration = Duration::from_secs(Bucket::LENGTH.as_secs() / 2);

/// How far the server's clock is from the device's: server minus device, in whole seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ClockOffset(i64);

impl ClockOffset {
    /// The offset `server` minus `device`, saturating at the `i64` range.
    #[must_use]
    pub fn between(server: UnixSeconds, device: UnixSeconds) -> Self {
        let ahead = server.saturating_duration_since(device).as_secs();
        let behind = device.saturating_duration_since(server).as_secs();
        let ahead = i64::try_from(ahead).unwrap_or(i64::MAX);
        let behind = i64::try_from(behind).unwrap_or(i64::MAX);
        Self(ahead.saturating_sub(behind))
    }

    /// Server minus device, in seconds (negative: the server is behind).
    #[must_use]
    pub const fn seconds(self) -> i64 {
        self.0
    }

    /// The size of the offset.
    #[must_use]
    pub const fn magnitude(self) -> Duration {
        Duration::from_secs(self.0.unsigned_abs())
    }

    /// `instant` moved by the offset, saturating at the epoch and at `u64::MAX`.
    #[must_use]
    pub const fn apply(self, instant: UnixSeconds) -> UnixSeconds {
        let moved = if self.0 >= 0 {
            instant.get().saturating_add(self.0.unsigned_abs())
        } else {
            instant.get().saturating_sub(self.0.unsigned_abs())
        };
        UnixSeconds::new(moved)
    }
}

/// What the last server time said about the device clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ClockStatus {
    /// No server time seen yet: the device clock is used.
    Unchecked,
    /// Within ±120 s: the device clock is corrected by the offset.
    Corrected(ClockOffset),
    /// Beyond 120 s but within half a bucket: the device clock is used unchanged.
    Uncorrected(ClockOffset),
    /// Beyond half a bucket: nothing is posted until a later server time is within range.
    Skewed(ClockOffset),
}

impl ClockStatus {
    /// Classifies an observed offset.
    #[must_use]
    pub fn from_offset(offset: ClockOffset) -> Self {
        let magnitude = offset.magnitude();
        if magnitude <= MAX_CORRECTION {
            Self::Corrected(offset)
        } else if magnitude <= MAX_SKEW {
            Self::Uncorrected(offset)
        } else {
            Self::Skewed(offset)
        }
    }

    /// The device time `device`, corrected if the status allows it.
    #[must_use]
    pub const fn correct(self, device: UnixSeconds) -> UnixSeconds {
        match self {
            Self::Corrected(offset) => offset.apply(device),
            Self::Unchecked | Self::Uncorrected(_) | Self::Skewed(_) => device,
        }
    }

    /// Whether posting is allowed.
    #[must_use]
    pub const fn allows_posting(self) -> bool {
        !matches!(self, Self::Skewed(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: UnixSeconds = UnixSeconds::new(1_800_000_000);

    fn server(delta: i64) -> UnixSeconds {
        ClockOffset(delta).apply(DEVICE)
    }

    #[test]
    fn offsets_are_signed_server_minus_device() {
        assert_eq!(ClockOffset::between(server(30), DEVICE).seconds(), 30);
        assert_eq!(ClockOffset::between(server(-30), DEVICE).seconds(), -30);
        assert_eq!(ClockOffset::between(DEVICE, DEVICE).seconds(), 0);
        assert_eq!(ClockOffset(-30).magnitude(), Duration::from_secs(30));
    }

    #[test]
    fn up_to_120_seconds_is_corrected_either_way() {
        for delta in [-120, -1, 0, 1, 120] {
            let status = ClockStatus::from_offset(ClockOffset(delta));
            assert_eq!(status, ClockStatus::Corrected(ClockOffset(delta)));
            assert_eq!(status.correct(DEVICE), server(delta));
            assert!(status.allows_posting());
        }
    }

    #[test]
    fn up_to_half_a_bucket_is_used_uncorrected() {
        for delta in [-450, -121, 121, 450] {
            let status = ClockStatus::from_offset(ClockOffset(delta));
            assert_eq!(status, ClockStatus::Uncorrected(ClockOffset(delta)));
            assert_eq!(status.correct(DEVICE), DEVICE);
            assert!(status.allows_posting());
        }
    }

    #[test]
    fn beyond_half_a_bucket_stops_posting() {
        for delta in [-451, 451, 86_400] {
            let status = ClockStatus::from_offset(ClockOffset(delta));
            assert_eq!(status, ClockStatus::Skewed(ClockOffset(delta)));
            assert_eq!(status.correct(DEVICE), DEVICE);
            assert!(!status.allows_posting());
        }
        assert!(ClockStatus::Unchecked.allows_posting());
        assert_eq!(ClockStatus::Unchecked.correct(DEVICE), DEVICE);
    }

    #[test]
    fn applying_saturates_at_the_epoch() {
        assert_eq!(
            ClockOffset(-10).apply(UnixSeconds::new(5)),
            UnixSeconds::new(0)
        );
        assert_eq!(MAX_SKEW, Duration::from_secs(450));
    }
}
