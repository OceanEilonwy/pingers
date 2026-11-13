// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy settings ("Sharing and Focus"): when to share, who sees the exact spot, delivery.

use pingers_gate::{GateDecision, LocationSharing, SuppressReason};
use pingers_meetup::LocationVisibility;

use crate::clock::LocalTime;
use crate::snapshot::{ClientSnapshot, PauseInFocus, Toggle, WakePushes};

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
}
