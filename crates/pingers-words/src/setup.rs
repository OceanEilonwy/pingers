// SPDX-License-Identifier: AGPL-3.0-only
//! First run, in words (canvas `SetupWelcome`, `SetupBackground`; the notifications step uses
//! the same layout).

use pingers_screens::setup::{Setup, SetupStep};

use crate::Language;

/// The white card under the welcome: what happens to location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupCard {
    /// "Your location stays on this phone".
    pub title: String,
    /// What the server sees instead.
    pub body: String,
}

/// A first-run screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupText {
    /// "Welcome to Pingers".
    pub heading: String,
    /// The line under the heading.
    pub body: String,
    /// The card, on the welcome only.
    pub card: Option<SetupCard>,
    /// Which option to choose in Android's dialog, on the steps that open one.
    pub hint: Option<String>,
    /// The dialog's options as a picture, the one to choose first.
    pub choices: Vec<String>,
    /// The progress dots, for screen readers: "First of three permissions".
    pub progress: String,
    /// "Allow location", "Continue".
    pub primary: String,
    /// "Not now".
    pub not_now: String,
}

fn ordinal(language: Language, index: usize) -> &'static str {
    let Language::English = language;
    match index {
        0 => "First",
        1 => "Second",
        _ => "Third",
    }
}

/// A first-run screen, in words.
#[must_use]
pub fn setup(language: Language, view: &Setup) -> SetupText {
    let Language::English = language;
    let progress = format!(
        "{} of three permissions",
        ordinal(language, view.position.index)
    );
    let not_now = "Not now".to_owned();
    match view.step {
        SetupStep::Location => SetupText {
            heading: "Welcome to Pingers".into(),
            body: "Find out when friends are nearby, without anyone, not even your server, \
                   knowing where you are."
                .into(),
            card: Some(SetupCard {
                title: "Your location stays on this phone".into(),
                body: "Pingers checks your location against your Party's Spots on this phone. \
                       Your server never sees where you are: it only sees encrypted records it \
                       can't read."
                    .into(),
            }),
            hint: None,
            choices: Vec::new(),
            progress,
            primary: "Allow location".into(),
            not_now,
        },
        SetupStep::Background => SetupText {
            heading: "Notice friends while the app is closed".into(),
            body: "Pingers uses your location in the background, even when the app is closed or \
                   not in use, to tell when you arrive at or leave one of your Party's Spots, and \
                   during a Meetup to share where you are with the people in it. Location is \
                   only checked on this phone and is never sent to your server."
                .into(),
            card: None,
            hint: Some(
                "On the next screen, choose “Allow all the time”. You can change this later in \
                 Settings."
                    .into(),
            ),
            choices: vec![
                "Allow all the time".into(),
                "Allow only while using the app".into(),
                "Don't allow".into(),
            ],
            progress,
            primary: "Continue".into(),
            not_now,
        },
        SetupStep::Notifications => SetupText {
            heading: "Know when friends are at a Spot".into(),
            body: "Pingers tells you when someone from your Party is at the same Spot, and when \
                   a Meetup starts. Notifications carry nothing your server can read."
                .into(),
            card: None,
            hint: Some(
                "On the next screen, choose “Allow”. You can change this later in Settings.".into(),
            ),
            choices: vec!["Allow".into(), "Don't allow".into()],
            progress,
            primary: "Continue".into(),
            not_now,
        },
    }
}

#[cfg(test)]
mod tests {
    use pingers_screens::setup::setup as screen;

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn the_welcome_reads_like_the_canvas() {
        let text = setup(EN, &screen(SetupStep::Location));
        assert_eq!(text.heading, "Welcome to Pingers");
        assert_eq!(
            text.card.as_ref().map(|card| card.title.as_str()),
            Some("Your location stays on this phone")
        );
        assert_eq!(text.primary, "Allow location");
        assert_eq!(text.not_now, "Not now");
        assert_eq!(text.progress, "First of three permissions");
        assert!(text.choices.is_empty());
    }

    #[test]
    fn background_location_says_which_option_to_choose() {
        let text = setup(EN, &screen(SetupStep::Background));
        assert_eq!(text.heading, "Notice friends while the app is closed");
        assert_eq!(
            text.hint.as_deref(),
            Some(
                "On the next screen, choose “Allow all the time”. You can change this later in \
                 Settings."
            )
        );
        assert_eq!(
            text.choices.first().map(String::as_str),
            Some("Allow all the time")
        );
        assert_eq!(text.choices.len(), 3);
        assert_eq!(text.primary, "Continue");
        assert_eq!(text.progress, "Second of three permissions");
        assert_eq!(text.card, None);
    }

    #[test]
    fn notifications_are_the_third_step_in_the_same_layout() {
        let text = setup(EN, &screen(SetupStep::Notifications));
        assert_eq!(text.heading, "Know when friends are at a Spot");
        assert_eq!(text.choices, ["Allow", "Don't allow"]);
        assert_eq!(text.progress, "Third of three permissions");
    }
}
