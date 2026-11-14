// SPDX-License-Identifier: AGPL-3.0-only
//! Device settings, in words: the Settings hub (canvas `Settings`), the Quiet hours editor
//! (`QuietHours`, `QuietHoursError`) and the Zone times editor (`ZoneTimes`).

use pingers_screens::ScreenError;
use pingers_screens::clock::{LocalTime, Minutes};
use pingers_screens::device::{Bytes, DeviceSettings, MapsOnPhone};
use pingers_screens::settings::{EndDay, QuietHoursEditor, QuietWindow};
use pingers_screens::snapshot::{Days, Toggle, ZoneTimes};
use pingers_screens::zones::{ZoneTimesDraft, ZoneTimesEditor};

use crate::Language;
use crate::common::{plural, time, time_range};
use crate::errors::screen_error;

/// The Settings hub, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceSettingsText {
    /// "Settings".
    pub title: String,
    /// "This phone, for every Party".
    pub subtitle: String,
    /// "BE", once a name is set.
    pub you_avatar: Option<String>,
    /// "Ben", or "Your name" before one is set.
    pub you_title: String,
    /// "Your name in every Party".
    pub you_subtitle: String,
    /// "When not to share".
    pub when_heading: String,
    /// "Quiet hours".
    pub quiet_hours: String,
    /// "23:00–07:00 every day", or "Off".
    pub quiet_hours_summary: String,
    /// "Exclusion zones".
    pub zones: String,
    /// "Home · Work, weekdays 08:00–18:00", or "None".
    pub zones_summary: String,
    /// "This phone".
    pub phone_heading: String,
    /// "Wake-ups".
    pub wakeups: String,
    /// "On · through ntfy".
    pub wakeups_summary: String,
    /// "Downloaded maps".
    pub maps: String,
    /// "2 areas · 246 MB", or "None".
    pub maps_summary: String,
    /// "How Pingers keeps you private".
    pub security_link: String,
    /// Where server settings are.
    pub footnote: String,
}

/// A size on disk: "246 MB", "1.2 GB", "640 KB".
#[must_use]
pub fn size(language: Language, bytes: Bytes) -> String {
    let Language::English = language;
    let bytes = bytes.get();
    let rounded = |unit: u64| {
        bytes
            .saturating_add(unit / 2)
            .checked_div(unit)
            .unwrap_or(0)
    };
    match bytes {
        0..1_000_000 => format!("{} KB", rounded(1_000).max(1)),
        1_000_000..1_000_000_000 => format!("{} MB", rounded(1_000_000)),
        _ => {
            let tenths = rounded(100_000_000);
            format!("{}.{} GB", tenths / 10, tenths % 10)
        }
    }
}

/// "2 areas · 246 MB", or "None".
#[must_use]
pub fn maps_summary(language: Language, maps: MapsOnPhone) -> String {
    let Language::English = language;
    if maps.areas == 0 {
        return "None".into();
    }
    format!(
        "{} · {}",
        plural(language, maps.areas, "area", "areas"),
        size(language, maps.size)
    )
}

/// "23:00–07:00 every day", or "Off".
fn quiet_hours_summary(language: Language, view: &DeviceSettings) -> String {
    let Language::English = language;
    match view.quiet_hours.state {
        Toggle::On => format!(
            "{} every day",
            time_range(language, view.quiet_hours.from, view.quiet_hours.to)
        ),
        Toggle::Off => "Off".into(),
    }
}

/// "Home · Work, weekdays 08:00–18:00", or "None".
fn zones_summary(language: Language, view: &DeviceSettings) -> String {
    let Language::English = language;
    if view.zones.is_empty() {
        return "None".into();
    }
    view.zones
        .iter()
        .map(|zone| match crate::zones::times(language, zone.times) {
            Some(times) => format!("{}, {times}", zone.name.as_str()),
            None => zone.name.as_str().to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The Settings hub, in words.
#[must_use]
pub fn device_settings(language: Language, view: &DeviceSettings) -> DeviceSettingsText {
    let Language::English = language;
    DeviceSettingsText {
        title: "Settings".into(),
        subtitle: "This phone, for every Party".into(),
        you_avatar: view
            .you
            .initials
            .as_ref()
            .map(|initials| initials.as_str().to_owned()),
        you_title: view
            .you
            .name
            .as_ref()
            .map_or_else(|| "Your name".into(), |name| name.as_str().to_owned()),
        you_subtitle: match view.you.name {
            Some(_) => "Your name in every Party".into(),
            None => "Not set yet · the same in every Party".into(),
        },
        when_heading: "When not to share".into(),
        quiet_hours: "Quiet hours".into(),
        quiet_hours_summary: quiet_hours_summary(language, view),
        zones: crate::zones::TITLE.into(),
        zones_summary: zones_summary(language, view),
        phone_heading: "This phone".into(),
        wakeups: crate::wakeups::TITLE.into(),
        wakeups_summary: crate::wakeups::summary(language, &view.wakeups),
        maps: "Downloaded maps".into(),
        maps_summary: maps_summary(language, view.maps),
        security_link: crate::security::TITLE.into(),
        footnote: "Each Party's server, relays, invites and safety codes are in that Party's \
                   settings."
            .into(),
    }
}

/// A length of time as a sentence says it: "8 hours", "1 hour 30 minutes", "45 minutes".
#[must_use]
pub fn length(language: Language, length: Minutes) -> String {
    let Language::English = language;
    let hours = usize::try_from(length.hours()).unwrap_or(usize::MAX);
    let minutes = usize::try_from(length.minutes_past_hour()).unwrap_or(0);
    match (hours, minutes) {
        (0, minutes) => plural(language, minutes, "minute", "minutes"),
        (hours, 0) => plural(language, hours, "hour", "hours"),
        (hours, minutes) => format!(
            "{} {}",
            plural(language, hours, "hour", "hours"),
            plural(language, minutes, "minute", "minutes")
        ),
    }
}

/// The Quiet hours editor, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuietHoursText {
    /// "Quiet hours".
    pub title: String,
    /// What quiet hours do, and that they look like being elsewhere.
    pub intro: String,
    /// "Quiet hours", the switch.
    pub switch_label: String,
    /// "Starts".
    pub starts: String,
    /// "23:00".
    pub from: String,
    /// "Starts at 23:00. Change", for screen readers.
    pub starts_action: String,
    /// "Ends".
    pub ends: String,
    /// "07:00".
    pub to: String,
    /// "next day" when the window crosses midnight.
    pub to_note: Option<String>,
    /// "Ends at 07:00 the next day. Change", for screen readers.
    pub ends_action: String,
    /// "8 hours, every day", or "No quiet time set".
    pub length: String,
    /// "Quiet from 23:00 to 07:00", or "No quiet time", for screen readers.
    pub bar_label: String,
    /// The core's refusal, when start and end are equal.
    pub error: Option<String>,
    /// The time zone and Meetups.
    pub footnote: String,
    /// "Save".
    pub save: String,
}

/// The Quiet hours editor, in words.
#[must_use]
pub fn quiet_hours(language: Language, view: &QuietHoursEditor) -> QuietHoursText {
    let Language::English = language;
    let from = time(language, view.from);
    let to = time(language, view.to);
    let next_day = matches!(
        view.window,
        QuietWindow::Quiet {
            ends: EndDay::NextDay,
            ..
        }
    );
    QuietHoursText {
        title: "Quiet hours".into(),
        intro: "Every day between these times Pingers shares nothing. Your phone keeps sending \
                blank check-ins, so nobody can tell quiet hours from being somewhere else."
            .into(),
        switch_label: "Quiet hours".into(),
        starts: "Starts".into(),
        starts_action: format!("Starts at {from}. Change"),
        ends: "Ends".into(),
        ends_action: if next_day {
            format!("Ends at {to} the next day. Change")
        } else {
            format!("Ends at {to}. Change")
        },
        to_note: next_day.then(|| "next day".into()),
        length: match &view.window {
            QuietWindow::Quiet { length: quiet, .. } => {
                format!("{}, every day", length(language, *quiet))
            }
            QuietWindow::Empty => "No quiet time set".into(),
        },
        bar_label: match view.window {
            QuietWindow::Quiet { .. } => format!("Quiet from {from} to {to}"),
            QuietWindow::Empty => "No quiet time".into(),
        },
        error: (view.window == QuietWindow::Empty)
            .then(|| screen_error(language, &ScreenError::EmptyWindow)),
        footnote: "Uses this phone's time zone. If you're in a Meetup when quiet hours start, \
                   you leave it."
            .into(),
        save: "Save".into(),
        from,
        to,
    }
}

/// The Zone times editor, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneTimesText {
    /// "Work".
    pub title: String,
    /// "Exclusion zone · drawn area · all parties".
    pub subtitle: String,
    /// "When it applies".
    pub when_heading: String,
    /// "When this zone applies", for screen readers.
    pub when_label: String,
    /// "All the time".
    pub always: String,
    /// "Only at certain times".
    pub certain_times: String,
    /// "Days", for screen readers.
    pub days_label: String,
    /// "Every day", "Weekdays", "Weekends".
    pub days: [String; 3],
    /// "From".
    pub from: String,
    /// "08:00", while a window is picked.
    pub from_time: Option<String>,
    /// "From 08:00. Change".
    pub from_action: Option<String>,
    /// "Until".
    pub until: String,
    /// "18:00".
    pub until_time: Option<String>,
    /// "Until 18:00. Change".
    pub until_action: Option<String>,
    /// "Monday to Friday, 08:00–18:00", the bold start of the summary.
    pub summary_lead: String,
    /// The rest of the summary.
    pub summary_rest: String,
    /// The core's refusal, when start and end are equal.
    pub error: Option<String>,
    /// Where the zone is kept.
    pub footnote: String,
    /// "Save".
    pub save: String,
    /// "Delete zone".
    pub delete: String,
}

fn days_sentence(language: Language, days: Days) -> &'static str {
    let Language::English = language;
    match days {
        Days::EveryDay => "Every day",
        Days::Weekdays => "Monday to Friday",
        Days::Weekends => "Saturday and Sunday",
    }
}

/// The Zone times editor, in words.
#[must_use]
pub fn zone_times(language: Language, view: &ZoneTimesEditor) -> ZoneTimesText {
    let Language::English = language;
    let zone = &view.zone;
    let (lead, rest, from, to) = match view.draft {
        ZoneTimesDraft::Always => (
            "All the time".to_owned(),
            ", Pingers sends nothing from here. Nobody in your Party can notice you while you're \
             in this place."
                .to_owned(),
            None,
            None,
        ),
        ZoneTimesDraft::Window { days, from, to } => (
            format!(
                "{}, {}",
                days_sentence(language, days),
                time_range(language, from, to)
            ),
            ", Pingers sends nothing from here. At other times this place is like anywhere else: \
             if it's inside a Spot, your Party can notice you."
                .to_owned(),
            Some(from),
            Some(to),
        ),
    };
    let text = |value: Option<LocalTime>| value.map(|value| time(language, value));
    ZoneTimesText {
        title: zone.name.as_str().to_owned(),
        subtitle: format!(
            "Exclusion zone · {} · {}",
            crate::zones::shape(language, zone.shape),
            crate::zones::applies_to(language, &zone.applies_to)
        ),
        when_heading: "When it applies".into(),
        when_label: "When this zone applies".into(),
        always: "All the time".into(),
        certain_times: "Only at certain times".into(),
        days_label: "Days".into(),
        days: ["Every day".into(), "Weekdays".into(), "Weekends".into()],
        from: "From".into(),
        from_action: text(from).map(|time| format!("From {time}. Change")),
        from_time: text(from),
        until: "Until".into(),
        until_action: text(to).map(|time| format!("Until {time}. Change")),
        until_time: text(to),
        summary_lead: lead,
        summary_rest: rest,
        error: view
            .problem
            .as_ref()
            .map(|problem| screen_error(language, problem)),
        footnote: "The zone and its times stay on this phone. Uses this phone's time zone.".into(),
        save: "Save".into(),
        delete: "Delete zone".into(),
    }
}

/// When a zone applies, for its stored times: the same words its row uses.
#[must_use]
pub fn zone_times_summary(language: Language, times: ZoneTimes) -> String {
    crate::zones::times(language, times).unwrap_or_else(|| "All the time".into())
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::device::{AppLabel, DevicePlatform, PushService, device_settings as hub};
    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::settings::quiet_hours_editor;
    use pingers_screens::snapshot::{WakePushes, ZoneId};
    use pingers_screens::zones::zone_times_editor;

    use super::*;

    const EN: Language = Language::English;

    fn platform() -> Result<DevicePlatform, FixtureError> {
        Ok(DevicePlatform {
            push_service: PushService::PushApp(AppLabel::new("ntfy").ok_or(FixtureError::Missing)?),
            maps: MapsOnPhone {
                areas: 2,
                size: Bytes::new(246_000_000),
            },
        })
    }

    #[test]
    fn the_hub_reads_like_the_canvas() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.settings.quiet_hours.state = Toggle::On;
        let text = device_settings(EN, &hub(&snapshot, &platform()?));
        assert_eq!(text.title, "Settings");
        assert_eq!(text.subtitle, "This phone, for every Party");
        assert_eq!(text.you_avatar.as_deref(), Some("SA"));
        assert_eq!(text.you_title, "Sam");
        assert_eq!(text.you_subtitle, "Your name in every Party");
        assert_eq!(text.when_heading, "When not to share");
        assert_eq!(text.quiet_hours_summary, "23:00–07:00 every day");
        assert_eq!(text.zones_summary, "Home · Work, weekdays 08:00–18:00");
        assert_eq!(text.wakeups_summary, "On · through ntfy");
        assert_eq!(text.maps_summary, "2 areas · 246 MB");
        assert_eq!(text.security_link, "How Pingers keeps you private");
        Ok(())
    }

    #[test]
    fn an_empty_hub_says_off_none_and_asks_for_a_name() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.display_name = None;
        snapshot.zones.clear();
        snapshot.settings.wake_pushes = WakePushes::NoWakePushes;
        let mut platform = platform()?;
        platform.maps = MapsOnPhone::default();
        let text = device_settings(EN, &hub(&snapshot, &platform));
        assert_eq!(text.you_avatar, None);
        assert_eq!(text.you_title, "Your name");
        assert_eq!(text.you_subtitle, "Not set yet · the same in every Party");
        assert_eq!(text.quiet_hours_summary, "Off");
        assert_eq!(text.zones_summary, "None");
        assert_eq!(text.wakeups_summary, "Off");
        assert_eq!(text.maps_summary, "None");
        Ok(())
    }

    #[test]
    fn sizes_round_to_the_unit_people_read() {
        assert_eq!(size(EN, Bytes::new(246_400_000)), "246 MB");
        assert_eq!(size(EN, Bytes::new(1_250_000_000)), "1.3 GB");
        assert_eq!(size(EN, Bytes::new(640_000)), "640 KB");
        assert_eq!(size(EN, Bytes::new(10)), "1 KB");
        assert_eq!(
            maps_summary(
                EN,
                MapsOnPhone {
                    areas: 1,
                    size: Bytes::new(62_000_000)
                }
            ),
            "1 area · 62 MB"
        );
    }

    #[test]
    fn quiet_hours_read_like_the_canvas() -> Result<(), ScreenError> {
        let text = quiet_hours(
            EN,
            &quiet_hours_editor(Toggle::On, LocalTime::new(23, 0)?, LocalTime::new(7, 0)?),
        );
        assert_eq!(text.title, "Quiet hours");
        assert_eq!((text.from.as_str(), text.to.as_str()), ("23:00", "07:00"));
        assert_eq!(text.to_note.as_deref(), Some("next day"));
        assert_eq!(text.length, "8 hours, every day");
        assert_eq!(text.starts_action, "Starts at 23:00. Change");
        assert_eq!(text.ends_action, "Ends at 07:00 the next day. Change");
        assert_eq!(text.bar_label, "Quiet from 23:00 to 07:00");
        assert_eq!(text.error, None);
        Ok(())
    }

    #[test]
    fn equal_quiet_hours_show_the_cores_refusal() -> Result<(), ScreenError> {
        let ten = LocalTime::new(22, 0)?;
        let text = quiet_hours(EN, &quiet_hours_editor(Toggle::On, ten, ten));
        assert_eq!(text.length, "No quiet time set");
        assert_eq!(text.bar_label, "No quiet time");
        assert_eq!(
            text.error.as_deref(),
            Some("The start and end times must differ.")
        );
        assert_eq!(text.to_note, None);
        assert_eq!(text.ends_action, "Ends at 22:00. Change");
        Ok(())
    }

    #[test]
    fn lengths_name_hours_and_minutes() {
        assert_eq!(length(EN, Minutes::new(90)), "1 hour 30 minutes");
        assert_eq!(length(EN, Minutes::new(45)), "45 minutes");
        assert_eq!(length(EN, Minutes::new(60)), "1 hour");
    }

    #[test]
    fn zone_times_read_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = zone_times(EN, &zone_times_editor(&snapshot, ZoneId::new(2), None)?);
        assert_eq!(text.title, "Work");
        assert_eq!(text.subtitle, "Exclusion zone · drawn area · all parties");
        assert_eq!(text.summary_lead, "Monday to Friday, 08:00–18:00");
        assert_eq!(
            text.summary_rest,
            ", Pingers sends nothing from here. At other times this place is like anywhere \
             else: if it's inside a Spot, your Party can notice you."
        );
        assert_eq!(text.from_time.as_deref(), Some("08:00"));
        assert_eq!(text.until_action.as_deref(), Some("Until 18:00. Change"));
        assert_eq!(text.delete, "Delete zone");
        let home = zone_times(EN, &zone_times_editor(&snapshot, ZoneId::new(1), None)?);
        assert_eq!(home.summary_lead, "All the time");
        assert_eq!(home.from_time, None);
        let eight = LocalTime::new(8, 0)?;
        let equal = ZoneTimesDraft::Window {
            days: Days::Weekends,
            from: eight,
            to: eight,
        };
        let refused = zone_times(
            EN,
            &zone_times_editor(&snapshot, ZoneId::new(2), Some(equal))?,
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("The start and end times must differ.")
        );
        assert_eq!(refused.summary_lead, "Saturday and Sunday, 08:00–08:00");
        assert_eq!(zone_times_summary(EN, ZoneTimes::Always), "All the time");
        Ok(())
    }
}
