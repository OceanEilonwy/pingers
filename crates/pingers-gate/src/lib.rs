// SPDX-License-Identifier: AGPL-3.0-only
//! "May this device share right now?" (Focus, pause, zones).
//!
//! The single answer to "may this device share its presence right now?" (architecture §3).
//! Inputs: Focus/DND state, manual pause, quiet hours, exclusion zones with the 5-minute trail
//! buffer, and whether location sharing is on. Output: [`GateDecision::Share`] or
//! [`GateDecision::Suppress`] with the reason.
//!
//! Suppress means: only dummy uploads, no trial matching, decline every knock, leave any
//! Meetup (design B.10). No other crate may grow its own pause logic; they all ask [`decide`].
#![forbid(unsafe_code)]

use pingers_geo::ZoneStatus;

/// Why a value could not be constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GateError {
    /// A minute of the day must be below 1,440.
    #[error("minute of day out of range")]
    MinuteOutOfRange,
    /// Quiet hours that start and end at the same minute cover nothing (or everything);
    /// use [`QuietHoursSetting::Off`] or the manual pause instead.
    #[error("quiet hours start and end at the same minute")]
    EmptyQuietHours,
}

/// A minute of the local day, 0 (00:00) to 1,439 (23:59).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MinuteOfDay(u16);

impl MinuteOfDay {
    /// Checks the range.
    ///
    /// # Errors
    ///
    /// [`GateError::MinuteOutOfRange`] if `minute` is 1,440 or more.
    pub const fn new(minute: u16) -> Result<Self, GateError> {
        if minute < 1_440 {
            Ok(Self(minute))
        } else {
            Err(GateError::MinuteOutOfRange)
        }
    }

    /// The minute.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A daily window during which sharing is suppressed. May wrap past midnight
/// (22:00 to 07:00). The start minute is inside the window; the end minute is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuietHours {
    start: MinuteOfDay,
    end: MinuteOfDay,
}

impl QuietHours {
    /// A window from `start` (inclusive) to `end` (exclusive).
    ///
    /// # Errors
    ///
    /// [`GateError::EmptyQuietHours`] if `start == end`.
    pub fn new(start: MinuteOfDay, end: MinuteOfDay) -> Result<Self, GateError> {
        if start == end {
            Err(GateError::EmptyQuietHours)
        } else {
            Ok(Self { start, end })
        }
    }

    /// Whether `now` falls in the window.
    #[must_use]
    pub fn contains(&self, now: MinuteOfDay) -> bool {
        // `start == end` cannot be constructed, so `<` and `<=` here behave the same.
        if self.start < self.end {
            self.start <= now && now < self.end
        } else {
            now >= self.start || now < self.end
        }
    }

    /// The first minute inside the window, for showing the window ("23:00–07:00").
    #[must_use]
    pub const fn start(&self) -> MinuteOfDay {
        self.start
    }

    /// The first minute after the window.
    #[must_use]
    pub const fn end(&self) -> MinuteOfDay {
        self.end
    }
}

/// Whether quiet hours are configured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuietHoursSetting {
    /// No quiet hours.
    Off,
    /// Suppress sharing during this daily window.
    Scheduled(QuietHours),
}

/// The platform's Do Not Disturb / Focus state (design E.1, `DndState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FocusState {
    /// A Focus or DND mode that does not allow Pingers is on.
    On,
    /// No such mode is on.
    Off,
    /// The platform would not say. Does not suppress (see `CONTRACT.md`); the core raises a
    /// `DndUnavailable` notice instead.
    Unknown,
}

/// The user's manual "Pause sharing" toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PauseState {
    /// Paused by the user.
    Paused,
    /// Not paused.
    NotPaused,
}

/// The app-wide location-sharing switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationSharing {
    /// Sharing presence is on.
    On,
    /// Sharing presence is off.
    Off,
}

/// Everything the gate looks at, read fresh at every wake and every knock (design B.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateInputs {
    /// The app-wide switch.
    pub location_sharing: LocationSharing,
    /// The manual pause.
    pub pause: PauseState,
    /// Focus / DND.
    pub focus: FocusState,
    /// The quiet-hours setting.
    pub quiet_hours: QuietHoursSetting,
    /// The local time of day, as the shell reports it.
    pub local_time: MinuteOfDay,
    /// Exclusion zones, from pingers-geo's `ExclusionTrail`.
    pub zones: ZoneStatus,
}

/// Why sharing is suppressed. When several apply, [`decide`] reports the first in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SuppressReason {
    /// Location sharing is switched off.
    LocationSharingOff,
    /// The user paused sharing.
    Paused,
    /// Focus or DND is on.
    Focus,
    /// Inside quiet hours.
    QuietHours,
    /// In an exclusion zone, or within its trail buffer.
    ExclusionZone,
}

/// The gate's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[must_use]
pub enum GateDecision {
    /// Real records, trial matching and knocks are allowed.
    Share,
    /// Dummies only, no matching, decline every knock, leave any Meetup (design B.10).
    Suppress(SuppressReason),
}

/// Decides whether the device may share its presence now.
///
/// [`FocusState::Unknown`] does not suppress: where the platform will not report Focus, the
/// user is told and offered the manual pause instead (design B.10, `CONTRACT.md`). It never
/// hides another reason that applies.
pub fn decide(inputs: &GateInputs) -> GateDecision {
    let in_quiet_hours = match inputs.quiet_hours {
        QuietHoursSetting::Off => false,
        QuietHoursSetting::Scheduled(window) => window.contains(inputs.local_time),
    };
    if inputs.location_sharing == LocationSharing::Off {
        GateDecision::Suppress(SuppressReason::LocationSharingOff)
    } else if inputs.pause == PauseState::Paused {
        GateDecision::Suppress(SuppressReason::Paused)
    } else if inputs.focus == FocusState::On {
        GateDecision::Suppress(SuppressReason::Focus)
    } else if in_quiet_hours {
        GateDecision::Suppress(SuppressReason::QuietHours)
    } else if inputs.zones.suppresses() {
        GateDecision::Suppress(SuppressReason::ExclusionZone)
    } else {
        GateDecision::Share
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_time::UnixSeconds;

    use super::*;

    fn minute(value: u16) -> Result<MinuteOfDay, GateError> {
        MinuteOfDay::new(value)
    }

    fn sharing() -> Result<GateInputs, GateError> {
        Ok(GateInputs {
            location_sharing: LocationSharing::On,
            pause: PauseState::NotPaused,
            focus: FocusState::Off,
            quiet_hours: QuietHoursSetting::Off,
            local_time: minute(12 * 60)?,
            zones: ZoneStatus::Clear,
        })
    }

    #[test]
    fn nothing_set_means_share() -> Result<(), GateError> {
        assert_eq!(decide(&sharing()?), GateDecision::Share);
        Ok(())
    }

    #[test]
    fn unknown_focus_does_not_suppress() -> Result<(), GateError> {
        // Focus On is covered by reasons_are_reported_in_priority_order.
        let unknown = GateInputs {
            focus: FocusState::Unknown,
            ..sharing()?
        };
        assert_eq!(decide(&unknown), GateDecision::Share);
        Ok(())
    }

    #[test]
    fn quiet_hours_report_their_ends_as_given() -> Result<(), GateError> {
        let window = QuietHours::new(minute(22 * 60)?, minute(7 * 60)?)?;
        assert_eq!(window.start(), minute(22 * 60)?);
        assert_eq!(window.end(), minute(7 * 60)?);
        Ok(())
    }

    #[test]
    fn quiet_hours_wrapping_midnight_cover_early_morning() -> Result<(), GateError> {
        let window = QuietHours::new(minute(22 * 60)?, minute(7 * 60)?)?;
        let inputs = GateInputs {
            quiet_hours: QuietHoursSetting::Scheduled(window),
            local_time: minute(3 * 60)?,
            ..sharing()?
        };
        assert_eq!(
            decide(&inputs),
            GateDecision::Suppress(SuppressReason::QuietHours)
        );
        assert!(!window.contains(minute(7 * 60)?));
        assert!(window.contains(minute(22 * 60)?));
        Ok(())
    }

    #[test]
    fn daytime_quiet_hours_exclude_the_evening() -> Result<(), GateError> {
        let window = QuietHours::new(minute(9 * 60)?, minute(17 * 60)?)?;
        assert!(!window.contains(minute(20 * 60)?));
        assert!(window.contains(minute(9 * 60)?));
        Ok(())
    }

    #[test]
    fn trail_buffer_suppresses_like_a_zone() -> Result<(), GateError> {
        let inputs = GateInputs {
            zones: ZoneStatus::TrailBuffer {
                until: UnixSeconds::new(1_000),
            },
            ..sharing()?
        };
        assert_eq!(
            decide(&inputs),
            GateDecision::Suppress(SuppressReason::ExclusionZone)
        );
        Ok(())
    }

    #[test]
    fn invalid_values_are_rejected() -> Result<(), GateError> {
        assert_eq!(MinuteOfDay::new(1_440), Err(GateError::MinuteOutOfRange));
        assert_eq!(
            QuietHours::new(minute(60)?, minute(60)?),
            Err(GateError::EmptyQuietHours)
        );
        Ok(())
    }

    #[test]
    fn the_last_minute_of_the_day_is_valid_and_kept() -> Result<(), GateError> {
        assert_eq!(minute(1_439)?.get(), 1_439);
        assert_eq!(minute(0)?.get(), 0);
        Ok(())
    }

    #[test]
    fn daytime_quiet_hours_end_before_their_end_minute() -> Result<(), GateError> {
        let window = QuietHours::new(minute(9 * 60)?, minute(17 * 60)?)?;
        assert!(window.contains(minute(17 * 60 - 1)?));
        assert!(!window.contains(minute(17 * 60)?));
        assert!(!window.contains(minute(9 * 60 - 1)?));
        Ok(())
    }

    #[test]
    fn wrapping_quiet_hours_cover_both_sides_of_midnight() -> Result<(), GateError> {
        let window = QuietHours::new(minute(22 * 60)?, minute(7 * 60)?)?;
        assert!(window.contains(minute(1_439)?));
        assert!(window.contains(minute(0)?));
        assert!(window.contains(minute(7 * 60 - 1)?));
        assert!(!window.contains(minute(22 * 60 - 1)?));
        assert!(!window.contains(minute(12 * 60)?));
        Ok(())
    }

    #[test]
    fn outside_scheduled_quiet_hours_means_share() -> Result<(), GateError> {
        let inputs = GateInputs {
            quiet_hours: QuietHoursSetting::Scheduled(QuietHours::new(
                minute(22 * 60)?,
                minute(7 * 60)?,
            )?),
            local_time: minute(12 * 60)?,
            ..sharing()?
        };
        assert_eq!(decide(&inputs), GateDecision::Share);
        Ok(())
    }

    #[test]
    fn unknown_focus_does_not_hide_a_later_reason() -> Result<(), GateError> {
        let inputs = GateInputs {
            focus: FocusState::Unknown,
            zones: ZoneStatus::InsideZone,
            ..sharing()?
        };
        assert_eq!(
            decide(&inputs),
            GateDecision::Suppress(SuppressReason::ExclusionZone)
        );
        Ok(())
    }

    /// Every input set to suppress: each reason is reported only when every earlier one is
    /// cleared, which pins the whole priority order.
    #[test]
    fn reasons_are_reported_in_priority_order() -> Result<(), GateError> {
        let mut inputs = GateInputs {
            location_sharing: LocationSharing::Off,
            pause: PauseState::Paused,
            focus: FocusState::On,
            quiet_hours: QuietHoursSetting::Scheduled(QuietHours::new(minute(0)?, minute(1_439)?)?),
            local_time: minute(12 * 60)?,
            zones: ZoneStatus::InsideZone,
        };
        assert_eq!(
            decide(&inputs),
            GateDecision::Suppress(SuppressReason::LocationSharingOff)
        );
        inputs.location_sharing = LocationSharing::On;
        assert_eq!(
            decide(&inputs),
            GateDecision::Suppress(SuppressReason::Paused)
        );
        inputs.pause = PauseState::NotPaused;
        assert_eq!(
            decide(&inputs),
            GateDecision::Suppress(SuppressReason::Focus)
        );
        inputs.focus = FocusState::Off;
        assert_eq!(
            decide(&inputs),
            GateDecision::Suppress(SuppressReason::QuietHours)
        );
        inputs.quiet_hours = QuietHoursSetting::Off;
        assert_eq!(
            decide(&inputs),
            GateDecision::Suppress(SuppressReason::ExclusionZone)
        );
        inputs.zones = ZoneStatus::Clear;
        assert_eq!(decide(&inputs), GateDecision::Share);
        Ok(())
    }
}
