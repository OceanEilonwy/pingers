// SPDX-License-Identifier: AGPL-3.0-only
//! The phone's clock as screens need it: the time now, and the local time of day for the times
//! a screen prints ("18:40"). pingers-words writes the times; this module only computes them.

use core::time::Duration;

use pingers_gate::MinuteOfDay;
use pingers_geo::Meters;
use pingers_time::UnixSeconds;

use crate::ScreenError;

const SECONDS_PER_DAY: i64 = 86_400;

/// A relaxed walking pace.
const WALKING_METERS_PER_MINUTE: f64 = 80.0;

/// The phone's offset from UTC now ("+01:00" is 3600 seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UtcOffset(i32);

impl UtcOffset {
    /// UTC itself.
    pub const UTC: Self = Self(0);

    /// Checks an offset in seconds: within ±18 hours, as ISO 8601 allows.
    ///
    /// # Errors
    ///
    /// [`ScreenError::InvalidUtcOffset`].
    pub const fn from_seconds(seconds: i32) -> Result<Self, ScreenError> {
        if seconds < -64_800 || seconds > 64_800 {
            return Err(ScreenError::InvalidUtcOffset);
        }
        Ok(Self(seconds))
    }
}

/// The phone's clock, which only the shell knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalClock {
    /// Now.
    pub now: UnixSeconds,
    /// The phone's offset from UTC now.
    pub offset: UtcOffset,
}

impl LocalClock {
    /// The local time of day at `at`.
    #[must_use]
    pub fn time_of(&self, at: UnixSeconds) -> LocalTime {
        let seconds = i64::try_from(at.get()).unwrap_or(i64::MAX);
        let local = seconds.saturating_add(i64::from(self.offset.0));
        let into_day = local.rem_euclid(SECONDS_PER_DAY);
        let minute = u16::try_from(into_day / 60).unwrap_or(0);
        LocalTime { minute }
    }

    /// Which local day `at` falls on, counted back from today: "today", "yesterday", or a
    /// number of days ago. A time after now counts as today.
    #[must_use]
    pub fn day_of(&self, at: UnixSeconds) -> RelativeDay {
        let day = |time: UnixSeconds| {
            i64::try_from(time.get())
                .unwrap_or(i64::MAX)
                .saturating_add(i64::from(self.offset.0))
                .div_euclid(SECONDS_PER_DAY)
        };
        match day(self.now).saturating_sub(day(at)) {
            ..=0 => RelativeDay::Today,
            1 => RelativeDay::Yesterday,
            days => RelativeDay::DaysAgo(u32::try_from(days).unwrap_or(u32::MAX)),
        }
    }
}

/// A local day, counted back from today.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RelativeDay {
    /// Today.
    Today,
    /// Yesterday.
    Yesterday,
    /// Two or more days ago.
    DaysAgo(u32),
}

/// A local time of day, to the minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalTime {
    /// Minutes since midnight, below 1440.
    minute: u16,
}

impl LocalTime {
    /// The time a gate setting names.
    #[must_use]
    pub const fn from_minute_of_day(minute: MinuteOfDay) -> Self {
        Self {
            minute: minute.get(),
        }
    }

    /// A time picked on a 24-hour clock.
    ///
    /// # Errors
    ///
    /// [`ScreenError::InvalidTime`] unless `hour` is below 24 and `minute` below 60.
    pub fn new(hour: u8, minute: u8) -> Result<Self, ScreenError> {
        if hour >= 24 || minute >= 60 {
            return Err(ScreenError::InvalidTime);
        }
        let minute = u16::from(hour)
            .checked_mul(60)
            .and_then(|start| start.checked_add(u16::from(minute)))
            .ok_or(ScreenError::InvalidTime)?;
        Ok(Self { minute })
    }

    /// The minute of the day, as pingers-gate takes it.
    ///
    /// # Errors
    ///
    /// None in practice: a `LocalTime` is always within the day. Reported as
    /// [`ScreenError::InvalidTime`] rather than assumed.
    pub fn minute_of_day(self) -> Result<MinuteOfDay, ScreenError> {
        MinuteOfDay::new(self.minute).map_err(|_out_of_range| ScreenError::InvalidTime)
    }

    /// Midnight, 00:00.
    pub const MIDNIGHT: Self = Self { minute: 0 };

    /// Minutes since midnight, 0 to 1439.
    #[must_use]
    pub fn minutes_since_midnight(self) -> u32 {
        u32::from(self.minute)
    }

    /// The hour, 0 to 23.
    #[must_use]
    pub const fn hour(self) -> u16 {
        self.minute / 60
    }

    /// The minute past the hour, 0 to 59.
    #[must_use]
    pub const fn minute(self) -> u16 {
        self.minute % 60
    }
}

/// A length of time in whole minutes, as screens show it ("1 h 25", "2 min walk").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Minutes(u32);

impl Minutes {
    /// Wraps a count of minutes.
    #[must_use]
    pub const fn new(minutes: u32) -> Self {
        Self(minutes)
    }

    /// A duration, rounded down to the minute.
    #[must_use]
    pub fn from_duration(duration: Duration) -> Self {
        Self(u32::try_from(duration.as_secs() / 60).unwrap_or(u32::MAX))
    }

    /// About how long it takes to walk a distance at 80 m a minute (4.8 km/h), to the nearest
    /// minute and never less than one.
    #[must_use]
    pub fn walking(distance: Meters) -> Self {
        let minutes = (distance.get() / WALKING_METERS_PER_MINUTE).round();
        // `Meters` is finite and not negative; clamp anyway so the cast below is exact.
        let clamped = minutes.clamp(1.0, f64::from(u32::MAX));
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a whole number clamped into u32's range converts exactly; Rust has no \
                      TryFrom from floats"
        )]
        let whole = clamped as u32;
        Self(whole)
    }

    /// The count of minutes.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Whole hours.
    #[must_use]
    pub const fn hours(self) -> u32 {
        self.0 / 60
    }

    /// Minutes past the whole hours.
    #[must_use]
    pub const fn minutes_past_hour(self) -> u32 {
        self.0 % 60
    }
}

/// A length of time in whole hours, rounded up, for invite ages and expiries ("expires in 23 h").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hours(u32);

impl Hours {
    /// A duration in hours, rounded up so an invite with 10 minutes left still says "1 h".
    #[must_use]
    pub fn rounding_up(duration: Duration) -> Self {
        Self(u32::try_from(duration.as_secs().div_ceil(3600)).unwrap_or(u32::MAX))
    }

    /// A duration in hours, rounded down, for "shared 2 h ago".
    #[must_use]
    pub fn rounding_down(duration: Duration) -> Self {
        Self(u32::try_from(duration.as_secs() / 3600).unwrap_or(u32::MAX))
    }

    /// The count of hours.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn days_are_counted_back_in_local_time() -> Result<(), ScreenError> {
        // 2026-11-14 00:30 in UTC+1 is still 2026-11-13 23:30 UTC.
        let now = UnixSeconds::new(1_763_163_000);
        let clock = LocalClock {
            now,
            offset: UtcOffset::from_seconds(3_600)?,
        };
        assert_eq!(clock.day_of(now), RelativeDay::Today);
        assert_eq!(clock.day_of(UnixSeconds::new(now.get() - 1_800)), RelativeDay::Today);
        assert_eq!(clock.day_of(UnixSeconds::new(now.get() - 3_600)), RelativeDay::Yesterday);
        assert_eq!(
            clock.day_of(UnixSeconds::new(now.get() - 3 * 86_400)),
            RelativeDay::DaysAgo(3)
        );
        assert_eq!(clock.day_of(UnixSeconds::new(now.get() + 86_400)), RelativeDay::Today);
        Ok(())
    }

    #[test]
    fn a_picked_time_is_a_minute_of_the_day() -> Result<(), ScreenError> {
        let late = LocalTime::new(23, 59)?;
        assert_eq!((late.hour(), late.minute()), (23, 59));
        assert_eq!(late.minute_of_day()?.get(), 1_439);
        assert_eq!(LocalTime::new(0, 0)?.minute_of_day()?.get(), 0);
        assert_eq!(LocalTime::new(7, 30)?.minute_of_day()?.get(), 450);
        for (hour, minute) in [(24, 0), (0, 60), (255, 255)] {
            assert_eq!(
                LocalTime::new(hour, minute),
                Err(ScreenError::InvalidTime),
                "{hour}:{minute}"
            );
        }
        Ok(())
    }

    #[test]
    fn local_time_follows_the_offset_and_wraps_at_midnight() -> Result<(), ScreenError> {
        // 2026-11-13 18:40 UTC.
        let at = UnixSeconds::new(1_794_528_000 + 18 * 3600 + 40 * 60);
        let utc = LocalClock {
            now: at,
            offset: UtcOffset::UTC,
        };
        let time = utc.time_of(at);
        assert_eq!((time.hour(), time.minute()), (18, 40));
        let ahead = LocalClock {
            now: at,
            offset: UtcOffset::from_seconds(6 * 3600)?,
        };
        let time = ahead.time_of(at);
        assert_eq!((time.hour(), time.minute()), (0, 40));
        let early = UnixSeconds::new(1_794_528_000 + 2 * 3600 + 40 * 60);
        let behind = LocalClock {
            now: early,
            offset: UtcOffset::from_seconds(-5 * 3600)?,
        };
        let time = behind.time_of(early);
        assert_eq!((time.hour(), time.minute()), (21, 40));
        Ok(())
    }

    #[test]
    fn offsets_stay_within_18_hours() {
        assert!(UtcOffset::from_seconds(64_800).is_ok());
        assert!(UtcOffset::from_seconds(-64_800).is_ok());
        assert_eq!(
            UtcOffset::from_seconds(64_801),
            Err(ScreenError::InvalidUtcOffset)
        );
        assert_eq!(
            UtcOffset::from_seconds(-64_801),
            Err(ScreenError::InvalidUtcOffset)
        );
    }

    #[test]
    fn minutes_and_hours_round_as_screens_need() {
        let minutes = Minutes::from_duration(Duration::from_secs(85 * 60 + 59));
        assert_eq!(
            (minutes.get(), minutes.hours(), minutes.minutes_past_hour()),
            (85, 1, 25)
        );
        assert_eq!(Hours::rounding_up(Duration::from_secs(600)).get(), 1);
        assert_eq!(Hours::rounding_up(Duration::from_secs(7200)).get(), 2);
        assert_eq!(Hours::rounding_up(Duration::from_secs(7201)).get(), 3);
        assert_eq!(Hours::rounding_down(Duration::from_secs(7199)).get(), 1);
        assert_eq!(Minutes::new(7).get(), 7);
        let long = Minutes::new(125);
        assert_eq!((long.hours(), long.minutes_past_hour()), (2, 5));
    }

    #[test]
    fn walks_round_to_the_nearest_minute_and_take_at_least_one() {
        let walk = |metres| Minutes::walking(Meters::from_whole(metres)).get();
        assert_eq!(walk(0), 1);
        assert_eq!(walk(85), 1);
        assert_eq!(walk(119), 1);
        assert_eq!(walk(120), 2);
        assert_eq!(walk(800), 10);
    }

    #[test]
    fn gate_times_become_local_times() -> Result<(), pingers_gate::GateError> {
        let time = LocalTime::from_minute_of_day(MinuteOfDay::new(23 * 60)?);
        assert_eq!((time.hour(), time.minute()), (23, 0));
        Ok(())
    }
}
