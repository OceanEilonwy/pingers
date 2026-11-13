// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy settings, in words.

use pingers_gate::SuppressReason;
use pingers_meetup::LocationVisibility;
use pingers_screens::settings::Privacy;

use crate::Language;
use crate::common::time_range;

/// Why sharing is paused, for a banner: "Focus is on".
#[must_use]
pub fn paused_because(language: Language, reason: SuppressReason) -> String {
    let Language::English = language;
    match reason {
        SuppressReason::LocationSharingOff => "Location sharing is off",
        SuppressReason::Paused => "You paused sharing",
        SuppressReason::Focus => "Focus is on",
        SuppressReason::QuietHours => "Quiet hours",
        SuppressReason::ExclusionZone => "In an exclusion zone",
        // A reason pingers-gate adds later still reads as a pause.
        _ => "Sharing paused",
    }
    .into()
}

/// What happens next, under the banner.
#[must_use]
pub fn paused_detail(language: Language, reason: SuppressReason) -> String {
    let Language::English = language;
    let resumes = match reason {
        SuppressReason::LocationSharingOff | SuppressReason::Paused => {
            "Resumes when you turn sharing back on."
        }
        SuppressReason::Focus => "Resumes when Focus ends.",
        SuppressReason::QuietHours => "Resumes when quiet hours end.",
        SuppressReason::ExclusionZone => "Resumes after you leave the zone.",
        _ => "Resumes on its own.",
    };
    format!("Nobody can tell you've paused. {resumes}")
}

/// A visibility choice and its help line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibilityOption {
    /// What it is.
    pub value: LocationVisibility,
    /// "Ask me each time".
    pub label: String,
    /// "Others see you're here, not where", if it has one.
    pub help: Option<String>,
}

/// The Privacy settings screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyText {
    /// "Privacy".
    pub title: String,
    /// "Paused · Focus is on", while paused.
    pub paused: Option<String>,
    /// "Nobody can tell you've paused. Resumes when Focus ends.", while paused.
    pub paused_detail: Option<String>,
    /// "When to share".
    pub when_heading: String,
    /// "Location sharing".
    pub sharing: String,
    /// "Pause during Focus".
    pub pause_in_focus: String,
    /// "Do Not Disturb, Sleep, any Focus".
    pub pause_in_focus_help: String,
    /// "Quiet hours".
    pub quiet_hours: String,
    /// "23:00–07:00".
    pub quiet_hours_range: String,
    /// "Exclusion zones".
    pub exclusion_zones: String,
    /// "2".
    pub exclusion_zone_count: String,
    /// "Who sees my exact spot in meetups".
    pub who_heading: String,
    /// The three choices.
    pub visibility: Vec<VisibilityOption>,
    /// "Delivery".
    pub delivery_heading: String,
    /// "No wake-up pushes".
    pub no_wake_pushes: String,
    /// Its help line.
    pub no_wake_pushes_help: String,
    /// The link to Security details.
    pub security_link: String,
}

/// The Privacy settings screen, in words.
#[must_use]
pub fn privacy(language: Language, view: &Privacy) -> PrivacyText {
    let Language::English = language;
    PrivacyText {
        title: "Privacy".into(),
        paused: view
            .paused_because
            .map(|reason| format!("Paused · {}", paused_because(language, reason))),
        paused_detail: view
            .paused_because
            .map(|reason| paused_detail(language, reason)),
        when_heading: "When to share".into(),
        sharing: "Location sharing".into(),
        pause_in_focus: "Pause during Focus".into(),
        pause_in_focus_help: "Do Not Disturb, Sleep, any Focus".into(),
        quiet_hours: "Quiet hours".into(),
        quiet_hours_range: time_range(language, view.quiet_hours.from, view.quiet_hours.to),
        exclusion_zones: crate::zones::TITLE.into(),
        exclusion_zone_count: view.exclusion_zones.to_string(),
        who_heading: "Who sees my exact spot in meetups".into(),
        visibility: vec![
            VisibilityOption {
                value: LocationVisibility::AskEachTime,
                label: "Ask me each time".into(),
                help: Some("Others see you're here, not where".into()),
            },
            VisibilityOption {
                value: LocationVisibility::Everyone,
                label: "Everyone in the meetup".into(),
                help: None,
            },
            VisibilityOption {
                value: LocationVisibility::Nobody,
                label: "Nobody".into(),
                help: None,
            },
        ],
        delivery_heading: "Delivery".into(),
        no_wake_pushes: "No wake-up pushes".into(),
        no_wake_pushes_help: "Nobody, not even Apple or Google, sees when you're woken. Meetups \
                              can take up to an hour to start."
            .into(),
        security_link: crate::security::TITLE.into(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_gate::GateDecision;
    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::settings::privacy as screen;

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn settings_read_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = privacy(EN, &screen(&snapshot));
        assert_eq!(text.title, "Privacy");
        assert_eq!((text.paused, text.paused_detail), (None, None));
        assert_eq!(text.quiet_hours_range, "23:00–07:00");
        assert_eq!(text.exclusion_zone_count, "2");
        assert_eq!(text.exclusion_zones, "Exclusion zones");
        let labels: Vec<_> = text
            .visibility
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["Ask me each time", "Everyone in the meetup", "Nobody"]
        );
        assert_eq!(
            text.visibility
                .first()
                .and_then(|option| option.help.as_deref()),
            Some("Others see you're here, not where")
        );
        assert!(
            text.no_wake_pushes_help
                .starts_with("Nobody, not even Apple or Google")
        );
        assert_eq!(
            (
                text.when_heading.as_str(),
                text.who_heading.as_str(),
                text.delivery_heading.as_str()
            ),
            (
                "When to share",
                "Who sees my exact spot in meetups",
                "Delivery"
            )
        );
        Ok(())
    }

    #[test]
    fn the_banner_says_why_and_when_it_ends() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.gate = GateDecision::Suppress(SuppressReason::Focus);
        let text = privacy(EN, &screen(&snapshot));
        assert_eq!(text.paused.as_deref(), Some("Paused · Focus is on"));
        assert_eq!(
            text.paused_detail.as_deref(),
            Some("Nobody can tell you've paused. Resumes when Focus ends.")
        );
        Ok(())
    }

    #[test]
    fn every_reason_has_words() {
        let said = [
            SuppressReason::LocationSharingOff,
            SuppressReason::Paused,
            SuppressReason::Focus,
            SuppressReason::QuietHours,
            SuppressReason::ExclusionZone,
        ]
        .map(|reason| (paused_because(EN, reason), paused_detail(EN, reason)));
        assert_eq!(
            said.each_ref().map(|(reason, _)| reason.as_str()),
            [
                "Location sharing is off",
                "You paused sharing",
                "Focus is on",
                "Quiet hours",
                "In an exclusion zone"
            ]
        );
        assert_eq!(
            said.each_ref()
                .map(|(_, detail)| detail.trim_start_matches("Nobody can tell you've paused. ")),
            [
                "Resumes when you turn sharing back on.",
                "Resumes when you turn sharing back on.",
                "Resumes when Focus ends.",
                "Resumes when quiet hours end.",
                "Resumes after you leave the zone."
            ]
        );
    }
}
