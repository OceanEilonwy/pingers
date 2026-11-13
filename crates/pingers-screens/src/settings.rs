// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy settings ("Sharing and Focus"): when to share, who sees the exact spot, delivery.

use pingers_gate::{GateDecision, LocationSharing, SuppressReason};
use pingers_meetup::LocationVisibility;

use crate::ScreenError;
use crate::clock::LocalTime;
use crate::common::Name;
use crate::snapshot::{
    ClientSnapshot, PauseInFocus, QuietHoursPreference, Toggle, WakePushes, daily_window,
};

/// Quiet hours as the row shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuietHoursRow {
    /// Whether they apply.
    pub state: Toggle,
    /// Where the window starts.
    pub from: LocalTime,
    /// Where it ends.
    pub to: LocalTime,
}

/// The Privacy settings screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Privacy {
    /// Why sharing is paused right now, for the banner; `None` when sharing, or when the switch
    /// is simply off (the switch shows that).
    pub paused_because: Option<SuppressReason>,
    /// The sharing switch.
    pub sharing: LocationSharing,
    /// "Pause during Focus".
    pub pause_in_focus: PauseInFocus,
    /// Quiet hours.
    pub quiet_hours: QuietHoursRow,
    /// How many exclusion zones.
    pub exclusion_zones: usize,
    /// Who sees the exact spot in Meetups.
    pub visibility: LocationVisibility,
    /// "No wake-up pushes".
    pub wake_pushes: WakePushes,
}

/// The Privacy settings screen.
#[must_use]
pub fn privacy(snapshot: &ClientSnapshot) -> Privacy {
    let settings = &snapshot.settings;
    let paused_because = match snapshot.gate {
        GateDecision::Suppress(SuppressReason::LocationSharingOff) | GateDecision::Share => None,
        GateDecision::Suppress(reason) => Some(reason),
    };
    Privacy {
        paused_because,
        sharing: settings.location_sharing,
        pause_in_focus: settings.pause_in_focus,
        quiet_hours: QuietHoursRow {
            state: settings.quiet_hours.state,
            from: LocalTime::from_minute_of_day(settings.quiet_hours.window.start()),
            to: LocalTime::from_minute_of_day(settings.quiet_hours.window.end()),
        },
        exclusion_zones: snapshot.zones.len(),
        visibility: settings.visibility,
        wake_pushes: settings.wake_pushes,
    }
}

/// Quiet hours as the user set them: the switch and a window picked on the clock.
///
/// # Errors
///
/// [`ScreenError::EmptyWindow`] if the window starts where it ends; [`ScreenError::InvalidTime`]
/// for a time outside the day.
pub fn quiet_hours(
    state: Toggle,
    from: LocalTime,
    to: LocalTime,
) -> Result<QuietHoursPreference, ScreenError> {
    let window = daily_window(from.minute_of_day()?, to.minute_of_day()?)?;
    Ok(QuietHoursPreference { state, window })
}

/// The name this device's user goes by in Parties they create or join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YourName {
    /// The name, once set.
    pub name: Option<Name>,
}

/// The name this device's user goes by.
#[must_use]
pub fn your_name(snapshot: &ClientSnapshot) -> YourName {
    YourName {
        name: snapshot.display_name.clone(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn settings_read_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = privacy(&snapshot);
        assert_eq!(view.paused_because, None);
        assert_eq!(view.sharing, LocationSharing::On);
        assert_eq!(view.pause_in_focus, PauseInFocus::On);
        assert_eq!(view.quiet_hours.state, Toggle::Off);
        assert_eq!(
            (view.quiet_hours.from.hour(), view.quiet_hours.to.hour()),
            (23, 7)
        );
        assert_eq!(view.exclusion_zones, 2);
        assert_eq!(view.visibility, LocationVisibility::AskEachTime);
        assert_eq!(view.wake_pushes, WakePushes::On);
        Ok(())
    }

    #[test]
    fn the_banner_names_why_sharing_is_paused() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.gate = GateDecision::Suppress(SuppressReason::Focus);
        assert_eq!(
            privacy(&snapshot).paused_because,
            Some(SuppressReason::Focus)
        );
        snapshot.gate = GateDecision::Suppress(SuppressReason::LocationSharingOff);
        assert_eq!(privacy(&snapshot).paused_because, None);
        Ok(())
    }

    #[test]
    fn quiet_hours_take_the_picked_window_and_switch() -> Result<(), ScreenError> {
        let set = quiet_hours(Toggle::On, LocalTime::new(22, 30)?, LocalTime::new(6, 45)?)?;
        assert_eq!(set.state, Toggle::On);
        assert_eq!(
            (set.window.start().get(), set.window.end().get()),
            (1_350, 405)
        );
        let off = quiet_hours(Toggle::Off, LocalTime::new(22, 30)?, LocalTime::new(6, 45)?)?;
        assert_eq!(off.state, Toggle::Off);
        Ok(())
    }

    #[test]
    fn quiet_hours_that_start_when_they_end_are_refused() -> Result<(), ScreenError> {
        let seven = LocalTime::new(7, 0)?;
        assert_eq!(
            quiet_hours(Toggle::On, seven, seven),
            Err(ScreenError::EmptyWindow)
        );
        Ok(())
    }

    #[test]
    fn your_name_is_the_one_set_on_this_device() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        assert_eq!(
            your_name(&snapshot).name.as_ref().map(Name::as_str),
            Some("Sam")
        );
        snapshot.display_name = None;
        assert_eq!(your_name(&snapshot), YourName { name: None });
        Ok(())
    }
}
