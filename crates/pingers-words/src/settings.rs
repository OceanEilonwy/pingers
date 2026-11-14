// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy settings, in words.

use pingers_gate::SuppressReason;
use pingers_meetup::LocationVisibility;
use pingers_screens::device::WakeupsSummary;
use pingers_screens::settings::{Privacy, YourName};
use pingers_screens::snapshot::{PushDelivery, PushDowngrade};

use crate::Language;

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
    /// "Who sees my exact spot in meetups".
    pub who_heading: String,
    /// The three choices.
    pub visibility: Vec<VisibilityOption>,
    /// "Delivery".
    pub delivery_heading: String,
    /// "Wake-ups", the link to the one home for delivery settings.
    pub wakeups: String,
    /// "On · through ntfy".
    pub wakeups_summary: String,
    /// Why wake-ups reach this phone late or not at all, under the delivery row; `None` when
    /// they arrive as they should, or when "No wake-up pushes" is on (its own row says so).
    pub push_delivery: Option<String>,
    /// The link to Security details.
    pub security_link: String,
}

/// The Privacy settings screen, in words.
#[must_use]
pub fn privacy(language: Language, view: &Privacy, wakeups: &WakeupsSummary) -> PrivacyText {
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
        wakeups: crate::wakeups::TITLE.into(),
        wakeups_summary: crate::wakeups::summary(language, wakeups),
        push_delivery: match view.push_delivery {
            PushDelivery::Full | PushDelivery::Downgraded(PushDowngrade::WakePushesOff) => None,
            PushDelivery::Downgraded(reason) => Some(push_downgrade(language, reason)),
        },
        security_link: crate::security::TITLE.into(),
    }
}

/// Why wake-up pushes are downgraded, and what the user can do, in one line.
#[must_use]
pub fn push_downgrade(language: Language, reason: PushDowngrade) -> String {
    let Language::English = language;
    match reason {
        PushDowngrade::WakePushesOff => {
            "Wake-up pushes are off. Meetups can take up to an hour to start."
        }
        PushDowngrade::NoPushToken => {
            "Nothing can wake Pingers: this phone has no push service. Install a UnifiedPush \
             distributor such as ntfy, or turn on Keep Pingers running."
        }
        PushDowngrade::NoPushRelay => {
            "This build has no push relay its server lists, so nothing can wake Pingers. \
             Meetups start when Pingers next wakes by itself."
        }
        PushDowngrade::RelayKeyMismatch => {
            "The push relay's key doesn't match the one this app trusts, so Pingers isn't \
             registering for wake-ups. Update the app or check the relay in Advanced."
        }
        PushDowngrade::PriorityLowered => {
            "Android has been holding back Pingers' wake-ups, so meetups may start late. Turn on \
             Keep Pingers running if it keeps happening."
        }
    }
    .into()
}

/// A Party the name changes in, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyChipText {
    /// "CC".
    pub initials: String,
    /// "Climbing crew".
    pub name: String,
}

/// Your name (canvas `YourName`), in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YourNameText {
    /// "Your name".
    pub title: String,
    /// "Back to Settings", for screen readers.
    pub back: String,
    /// "BE", once a name is set.
    pub avatar: Option<String>,
    /// "How you appear to members".
    pub appears_as: String,
    /// "Name", the field's label.
    pub label: String,
    /// The name set, if any.
    pub name: Option<String>,
    /// "First name or nickname", for an empty field.
    pub placeholder: String,
    /// How long it may be and who sees it.
    pub help: String,
    /// "Changes in every Party you're in", when there are Parties.
    pub parties_heading: Option<String>,
    /// The Parties.
    pub parties: Vec<PartyChipText>,
    /// When members see a new name.
    pub note: String,
    /// "Save".
    pub save: String,
}

/// Your name, in words.
#[must_use]
pub fn your_name(language: Language, view: &YourName) -> YourNameText {
    let Language::English = language;
    YourNameText {
        title: "Your name".into(),
        back: "Back to Settings".into(),
        avatar: view
            .initials
            .as_ref()
            .map(|initials| initials.as_str().to_owned()),
        appears_as: "How you appear to members".into(),
        label: "Name".into(),
        name: view.name.as_ref().map(|name| name.as_str().to_owned()),
        placeholder: "First name or nickname".into(),
        help: "Up to 80 characters. Members see it, encrypted end to end. Your server never \
               does."
            .into(),
        parties_heading: (!view.parties.is_empty())
            .then(|| "Changes in every Party you're in".into()),
        parties: view
            .parties
            .iter()
            .map(|chip| PartyChipText {
                initials: chip.initials.as_str().to_owned(),
                name: chip.name.as_str().to_owned(),
            })
            .collect(),
        note: "Members see the new name the next time their phone checks in. Until then they \
               see the name you had before."
            .into(),
        save: "Save".into(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_gate::GateDecision;
    use pingers_screens::device::{AppLabel, PushService};
    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::settings::privacy as screen;

    use super::*;

    const EN: Language = Language::English;

    fn wakeups() -> Result<WakeupsSummary, FixtureError> {
        Ok(WakeupsSummary {
            pushes: pingers_screens::snapshot::WakePushes::On,
            service: PushService::PushApp(AppLabel::new("ntfy").ok_or(FixtureError::Missing)?),
        })
    }

    #[test]
    fn settings_read_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = privacy(EN, &screen(&snapshot), &wakeups()?);
        assert_eq!(text.title, "Privacy");
        assert_eq!((text.paused, text.paused_detail), (None, None));
        assert_eq!(text.wakeups, "Wake-ups");
        assert_eq!(text.wakeups_summary, "On · through ntfy");
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
        let text = privacy(EN, &screen(&snapshot), &wakeups()?);
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

    #[test]
    fn a_downgrade_is_said_under_the_delivery_row_unless_pushes_are_off_or_fine()
    -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        assert_eq!(
            privacy(EN, &screen(&snapshot), &wakeups()?).push_delivery,
            None
        );
        snapshot.push_delivery = PushDelivery::Downgraded(PushDowngrade::WakePushesOff);
        assert_eq!(
            privacy(EN, &screen(&snapshot), &wakeups()?).push_delivery,
            None
        );
        snapshot.push_delivery = PushDelivery::Downgraded(PushDowngrade::PriorityLowered);
        assert_eq!(
            privacy(EN, &screen(&snapshot), &wakeups()?).push_delivery,
            Some(push_downgrade(EN, PushDowngrade::PriorityLowered))
        );
        Ok(())
    }

    #[test]
    fn every_push_downgrade_has_its_own_words() {
        let said: Vec<String> = [
            PushDowngrade::WakePushesOff,
            PushDowngrade::NoPushToken,
            PushDowngrade::NoPushRelay,
            PushDowngrade::RelayKeyMismatch,
            PushDowngrade::PriorityLowered,
        ]
        .into_iter()
        .map(|reason| push_downgrade(EN, reason))
        .collect();
        for (index, text) in said.iter().enumerate() {
            assert!(text.ends_with('.'), "{text}");
            assert!(
                !said
                    .iter()
                    .skip(index.saturating_add(1))
                    .any(|other| other == text)
            );
        }
        assert!(push_downgrade(EN, PushDowngrade::NoPushToken).contains("ntfy"));
    }

    #[test]
    fn your_name_shows_the_name_or_a_placeholder() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let text = your_name(EN, &pingers_screens::settings::your_name(&snapshot));
        assert_eq!(text.title, "Your name");
        assert_eq!(text.name.as_deref(), Some("Sam"));
        assert_eq!(text.avatar.as_deref(), Some("SA"));
        assert_eq!(text.appears_as, "How you appear to members");
        assert_eq!(
            text.help,
            "Up to 80 characters. Members see it, encrypted end to end. Your server never does."
        );
        assert_eq!(
            text.parties_heading.as_deref(),
            Some("Changes in every Party you're in")
        );
        assert_eq!(
            text.parties.first(),
            Some(&PartyChipText {
                initials: "CC".into(),
                name: "Climbing crew".into()
            })
        );
        assert_eq!(text.save, "Save");
        snapshot.display_name = None;
        let unset = your_name(EN, &pingers_screens::settings::your_name(&snapshot));
        assert_eq!(unset.name, None);
        assert_eq!(unset.placeholder, "First name or nickname");
        Ok(())
    }
}
