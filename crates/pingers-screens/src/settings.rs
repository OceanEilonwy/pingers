// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy settings ("Sharing and Focus"): when to share, who sees the exact spot, delivery.

use pingers_gate::{GateDecision, LocationSharing, SuppressReason};
use pingers_meetup::LocationVisibility;

use crate::ScreenError;
use crate::clock::{LocalTime, Minutes};
use crate::common::{Initials, Name, PartyColour};
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

/// A Party the name changes in ("Changes in every Party you're in").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyChip {
    /// Its name.
    pub name: Name,
    /// Its initials.
    pub initials: Initials,
    /// Its colour.
    pub colour: PartyColour,
}

/// The name this device's user goes by, the same in every Party (canvas `YourName`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YourName {
    /// The name, once set.
    pub name: Option<Name>,
    /// Its initials, once set.
    pub initials: Option<Initials>,
    /// The Parties it changes in.
    pub parties: Vec<PartyChip>,
}

/// The name this device's user goes by.
#[must_use]
pub fn your_name(snapshot: &ClientSnapshot) -> YourName {
    YourName {
        name: snapshot.display_name.clone(),
        initials: snapshot.display_name.as_ref().map(Initials::of),
        parties: snapshot
            .parties
            .iter()
            .map(|party| PartyChip {
                name: party.name.clone(),
                initials: Initials::of(&party.name),
                colour: party.colour,
            })
            .collect(),
    }
}

/// Where quiet hours end, relative to where they start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndDay {
    /// Later the same day.
    SameDay,
    /// After midnight ("next day").
    NextDay,
}

/// A stretch of the 24-hour bar, in minutes since midnight: `start` up to `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarSpan {
    /// Where it starts.
    pub start: LocalTime,
    /// Minutes from `start` to its end.
    pub length: Minutes,
}

/// The quiet window the editor shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuietWindow {
    /// A window: how long, where it ends, and the bar's stretches (two when it crosses
    /// midnight).
    Quiet {
        /// How long.
        length: Minutes,
        /// Same day or next.
        ends: EndDay,
        /// The bar.
        bar: Vec<BarSpan>,
    },
    /// The start and end are equal: the core refuses it (`ScreenError::EmptyWindow`).
    Empty,
}

/// The Quiet hours editor (canvas `QuietHours`, `QuietHoursError`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuietHoursEditor {
    /// The switch.
    pub state: Toggle,
    /// "Starts".
    pub from: LocalTime,
    /// "Ends".
    pub to: LocalTime,
    /// The window.
    pub window: QuietWindow,
    /// Whether "Save" is enabled: only once the times differ.
    pub can_save: bool,
}

const MINUTES_PER_DAY: u32 = 24 * 60;

/// The Quiet hours editor for the times picked so far.
#[must_use]
pub fn quiet_hours_editor(state: Toggle, from: LocalTime, to: LocalTime) -> QuietHoursEditor {
    let start = from.minutes_since_midnight();
    let end = to.minutes_since_midnight();
    let window = match start.cmp(&end) {
        core::cmp::Ordering::Equal => QuietWindow::Empty,
        core::cmp::Ordering::Less => QuietWindow::Quiet {
            length: Minutes::new(end.saturating_sub(start)),
            ends: EndDay::SameDay,
            bar: vec![BarSpan {
                start: from,
                length: Minutes::new(end.saturating_sub(start)),
            }],
        },
        core::cmp::Ordering::Greater => {
            let mut bar = vec![BarSpan {
                start: from,
                length: Minutes::new(MINUTES_PER_DAY.saturating_sub(start)),
            }];
            if end > 0 {
                bar.insert(
                    0,
                    BarSpan {
                        start: LocalTime::MIDNIGHT,
                        length: Minutes::new(end),
                    },
                );
            }
            QuietWindow::Quiet {
                length: Minutes::new(MINUTES_PER_DAY.saturating_sub(start).saturating_add(end)),
                ends: EndDay::NextDay,
                bar,
            }
        }
    };
    QuietHoursEditor {
        state,
        from,
        to,
        can_save: window != QuietWindow::Empty,
        window,
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
        let parties: Vec<_> = your_name(&snapshot)
            .parties
            .iter()
            .map(|chip| {
                (
                    chip.name.as_str().to_owned(),
                    chip.initials.as_str().to_owned(),
                    chip.colour,
                )
            })
            .collect();
        assert_eq!(
            parties.first(),
            Some(&(
                "Climbing crew".to_owned(),
                "CC".to_owned(),
                PartyColour::Blue
            ))
        );
        assert_eq!(parties.len(), snapshot.parties.len());
        snapshot.display_name = None;
        let unnamed = your_name(&snapshot);
        assert_eq!((unnamed.name, unnamed.initials), (None, None));
        Ok(())
    }

    #[test]
    fn quiet_hours_past_midnight_end_the_next_day() -> Result<(), ScreenError> {
        let editor = quiet_hours_editor(Toggle::On, LocalTime::new(23, 0)?, LocalTime::new(7, 0)?);
        assert!(editor.can_save);
        let QuietWindow::Quiet { length, ends, bar } = editor.window else {
            return Err(ScreenError::EmptyWindow);
        };
        assert_eq!((length.get(), ends), (480, EndDay::NextDay));
        let spans: Vec<_> = bar
            .iter()
            .map(|span| (span.start.minutes_since_midnight(), span.length.get()))
            .collect();
        assert_eq!(spans, [(0, 420), (1_380, 60)]);
        Ok(())
    }

    #[test]
    fn quiet_hours_within_a_day_and_up_to_midnight_are_one_span() -> Result<(), ScreenError> {
        let day = quiet_hours_editor(Toggle::Off, LocalTime::new(13, 0)?, LocalTime::new(14, 30)?);
        assert_eq!(
            day.window,
            QuietWindow::Quiet {
                length: Minutes::new(90),
                ends: EndDay::SameDay,
                bar: vec![BarSpan {
                    start: LocalTime::new(13, 0)?,
                    length: Minutes::new(90)
                }],
            }
        );
        let to_midnight =
            quiet_hours_editor(Toggle::On, LocalTime::new(22, 0)?, LocalTime::MIDNIGHT);
        let QuietWindow::Quiet { bar, ends, .. } = to_midnight.window else {
            return Err(ScreenError::EmptyWindow);
        };
        assert_eq!((bar.len(), ends), (1, EndDay::NextDay));
        Ok(())
    }

    #[test]
    fn equal_quiet_hours_cannot_be_saved() -> Result<(), ScreenError> {
        let ten = LocalTime::new(22, 0)?;
        let editor = quiet_hours_editor(Toggle::On, ten, ten);
        assert_eq!(editor.window, QuietWindow::Empty);
        assert!(!editor.can_save);
        Ok(())
    }
}
