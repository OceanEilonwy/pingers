// SPDX-License-Identifier: AGPL-3.0-only
//! Exclusion zones, in words.

use pingers_screens::clock::LocalTime;
use pingers_screens::snapshot::{Days, HideTrail, ZoneTimes};
use pingers_screens::zones::{ExclusionZones, ZoneAppliesTo, ZoneShape};

use crate::Language;
use crate::common::{distance, list, time_range};

/// "Exclusion zones": the screen's title and the settings row.
pub const TITLE: &str = "Exclusion zones";

/// A zone row, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneRowText {
    /// "Home".
    pub name: String,
    /// "150 m circle · all parties", "Drawn area · weekdays 08:00–18:00 · all parties".
    pub detail: String,
}

/// The Exclusion zones screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExclusionZonesText {
    /// "Exclusion zones".
    pub title: String,
    /// What zones do.
    pub intro: String,
    /// The zones.
    pub zones: Vec<ZoneRowText>,
    /// "Add exclusion zone".
    pub add: String,
    /// "Hide trail near zones".
    pub hide_trail: String,
    /// Its help line.
    pub hide_trail_help: String,
    /// Whether the switch is on.
    pub hide_trail_on: bool,
}

pub(crate) fn times(language: Language, times: ZoneTimes) -> Option<String> {
    let Language::English = language;
    match times {
        ZoneTimes::Always => None,
        ZoneTimes::Window { days, window } => {
            let range = time_range(
                language,
                LocalTime::from_minute_of_day(window.start()),
                LocalTime::from_minute_of_day(window.end()),
            );
            Some(match days {
                Days::EveryDay => format!("daily {range}"),
                Days::Weekdays => format!("weekdays {range}"),
                Days::Weekends => format!("weekends {range}"),
            })
        }
    }
}

pub(crate) fn applies_to(language: Language, applies: &ZoneAppliesTo) -> String {
    let Language::English = language;
    match applies {
        ZoneAppliesTo::AllParties => "all parties".into(),
        ZoneAppliesTo::Parties(names) => {
            if names.is_empty() {
                "no parties".into()
            } else {
                let names: Vec<String> =
                    names.iter().map(|name| name.as_str().to_owned()).collect();
                list(language, &names)
            }
        }
    }
}

/// A zone's shape inside a sentence: "150 m circle", "drawn area".
pub(crate) fn shape(language: Language, shape: ZoneShape) -> String {
    let Language::English = language;
    match shape {
        ZoneShape::Circle(radius) => format!("{} circle", distance(language, radius)),
        ZoneShape::Area => "drawn area".into(),
    }
}

/// The Exclusion zones screen, in words.
#[must_use]
pub fn exclusion_zones(language: Language, view: &ExclusionZones) -> ExclusionZonesText {
    let Language::English = language;
    ExclusionZonesText {
        title: TITLE.into(),
        intro: "Inside these areas Pingers never sends your location, to any party. Zones are \
                stored only on this phone."
            .into(),
        zones: view
            .zones
            .iter()
            .map(|zone| {
                let shape = match zone.shape {
                    ZoneShape::Circle(radius) => format!("{} circle", distance(language, radius)),
                    ZoneShape::Area => "Drawn area".into(),
                };
                let parts: Vec<String> = [
                    Some(shape),
                    times(language, zone.times),
                    Some(applies_to(language, &zone.applies_to)),
                ]
                .into_iter()
                .flatten()
                .collect();
                ZoneRowText {
                    name: zone.name.as_str().to_owned(),
                    detail: parts.join(" · "),
                }
            })
            .collect(),
        add: "Add exclusion zone".into(),
        hide_trail: "Hide trail near zones".into(),
        hide_trail_help: "Stay hidden for 5 min after leaving, so nobody can trace you back".into(),
        hide_trail_on: matches!(view.hide_trail, HideTrail::On),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_gate::MinuteOfDay;
    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::snapshot::{DailyWindow, ZoneParties, ZoneSchedule};
    use pingers_screens::zones::exclusion_zones as screen;

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn zones_read_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = exclusion_zones(EN, &screen(&snapshot));
        assert_eq!(text.title, "Exclusion zones");
        let rows: Vec<_> = text
            .zones
            .iter()
            .map(|row| (row.name.as_str(), row.detail.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Home", "150 m circle · all parties"),
                ("Work", "Drawn area · weekdays 08:00–18:00 · all parties"),
            ]
        );
        assert!(text.hide_trail_on);
        assert_eq!(text.add, "Add exclusion zone");
        assert_eq!(text.hide_trail, "Hide trail near zones");
        assert!(text.hide_trail_help.starts_with("Stay hidden for 5 min"));
        assert!(text.intro.starts_with("Inside these areas"));
        Ok(())
    }

    #[test]
    fn schedules_name_days_and_parties() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let window = DailyWindow::new(MinuteOfDay::new(22 * 60)?, MinuteOfDay::new(6 * 60)?)?;
        let schedules = [
            ZoneSchedule {
                times: ZoneTimes::Window {
                    days: Days::EveryDay,
                    window,
                },
                parties: ZoneParties::Only(vec![fixtures::uni_flat(), fixtures::five_a_side()]),
            },
            ZoneSchedule {
                times: ZoneTimes::Window {
                    days: Days::Weekends,
                    window,
                },
                parties: ZoneParties::Only(Vec::new()),
            },
        ];
        for (zone, schedule) in snapshot.zones.iter_mut().zip(schedules) {
            zone.schedule = schedule;
        }
        snapshot.settings.hide_trail = HideTrail::Off;
        let text = exclusion_zones(EN, &screen(&snapshot));
        let details: Vec<_> = text.zones.iter().map(|row| row.detail.as_str()).collect();
        assert_eq!(
            details,
            [
                "150 m circle · daily 22:00–06:00 · Uni flat and Saturday 5-a-side",
                "Drawn area · weekends 22:00–06:00 · no parties"
            ]
        );
        assert!(!text.hide_trail_on);
        Ok(())
    }
}
