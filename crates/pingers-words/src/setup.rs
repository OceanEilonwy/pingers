// SPDX-License-Identifier: AGPL-3.0-only
//! First run, in words (canvas `SetupWelcome`, `SetupBackground`; the notifications step uses
//! the same layout), and the first-phone setup link's step.

use pingers_screens::setup::{Setup, SetupLink, SetupLinkAction, SetupStep};

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

/// The first-phone setup link's step, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupLinkText {
    /// "Set up Pingers on this phone".
    pub heading: String,
    /// What it does, or what happened and what to do next.
    pub body: String,
    /// The main button ("Set up", "Try again", "Continue", "Close"); `None` while the
    /// account is being made.
    pub primary: Option<String>,
    /// "Not now", while using the link is still on offer.
    pub not_now: Option<String>,
}

/// Where the owner finds a new code: the page their appliance shows.
const SCAN_AGAIN: &str = "Open the setup page on your appliance again and scan its code.";

/// The first-phone setup link's step, in words.
#[must_use]
pub fn setup_link(language: Language, view: &SetupLink) -> SetupLinkText {
    let Language::English = language;
    let (heading, body) = match view {
        SetupLink::Confirm { server } => (
            "Set up Pingers on this phone".to_owned(),
            format!(
                "This makes this phone's account on {} with the code from your appliance. This \
                 phone's invite links then carry the code, so the friends you invite can make \
                 their accounts too.",
                server.as_str()
            ),
        ),
        SetupLink::Working { server } => (
            "Setting up this phone".to_owned(),
            format!("Making this phone's account on {}…", server.as_str()),
        ),
        SetupLink::SetUp => (
            "This phone is set up".to_owned(),
            "It has its account now. When you invite friends, your invite links bring them the \
             code."
                .to_owned(),
        ),
        SetupLink::AlreadySetUp => (
            "This phone is already set up".to_owned(),
            "It has its account on this server, so it doesn't need the code.".to_owned(),
        ),
        SetupLink::Refused => (
            "The server didn't accept this code".to_owned(),
            format!("It may have been reset since the setup page showed it. {SCAN_AGAIN}"),
        ),
        SetupLink::OtherServer { app, link } => (
            "This app is set up for another server".to_owned(),
            format!(
                "This link is for {link}, but this app works with {app}. Install the Pingers app \
                 made for {link}, then scan the code again.",
                link = link.as_str(),
                app = app.as_str(),
            ),
        ),
        SetupLink::BuildKeysDiffer { server } => (
            "This app doesn't have your server's keys yet".to_owned(),
            format!(
                "The keys in this link aren't built into this app, so nothing was sent to {}. \
                 Install the app build made after your server's keys were copied into it, then \
                 scan the code again.",
                server.as_str()
            ),
        ),
        SetupLink::ServerKeysDiffer { server } => (
            "The server's keys don't match this link".to_owned(),
            format!(
                "{} answered with other keys than the ones in this link, so the code wasn't \
                 sent. Check that you scanned your own appliance's setup page, then try again \
                 later.",
                server.as_str()
            ),
        ),
        SetupLink::Unreachable { server } => (
            "Couldn't reach the server".to_owned(),
            format!(
                "This phone couldn't reach {}. Check that it's online, then try again.",
                server.as_str()
            ),
        ),
        SetupLink::Broken => (
            "This setup link doesn't work".to_owned(),
            format!("Part of it may be missing. {SCAN_AGAIN}"),
        ),
    };
    let primary = view.primary().map(|action| {
        match (action, view) {
            (SetupLinkAction::SetUp, _) => "Set up",
            (SetupLinkAction::TryAgain, _) => "Try again",
            (SetupLinkAction::Leave, SetupLink::SetUp | SetupLink::AlreadySetUp) => "Continue",
            (SetupLinkAction::Leave, _) => "Close",
        }
        .to_owned()
    });
    SetupLinkText {
        heading,
        body,
        primary,
        not_now: view.can_put_off().then(|| "Not now".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_client::snapshot::Host;
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

    fn host(text: &str) -> Result<Host, pingers_client::snapshot::SnapshotError> {
        Host::new(text)
    }

    #[test]
    fn the_setup_link_asks_with_the_servers_name()
    -> Result<(), pingers_client::snapshot::SnapshotError> {
        let text = setup_link(
            EN,
            &SetupLink::Confirm {
                server: host("pingers.example.org")?,
            },
        );
        assert_eq!(text.heading, "Set up Pingers on this phone");
        assert_eq!(
            text.body,
            "This makes this phone's account on pingers.example.org with the code from your \
             appliance. This phone's invite links then carry the code, so the friends you invite \
             can make their accounts too."
        );
        assert_eq!(text.primary.as_deref(), Some("Set up"));
        assert_eq!(text.not_now.as_deref(), Some("Not now"));
        Ok(())
    }

    #[test]
    fn the_setup_link_says_what_happened_and_what_to_do()
    -> Result<(), pingers_client::snapshot::SnapshotError> {
        let server = host("pingers.example.org")?;
        let working = setup_link(
            EN,
            &SetupLink::Working {
                server: server.clone(),
            },
        );
        assert_eq!(working.primary, None);
        assert_eq!(working.not_now, None);
        let done = setup_link(EN, &SetupLink::SetUp);
        assert_eq!(done.primary.as_deref(), Some("Continue"));
        assert_eq!(done.not_now, None);
        assert_eq!(
            setup_link(EN, &SetupLink::AlreadySetUp).heading,
            "This phone is already set up"
        );
        let refused = setup_link(EN, &SetupLink::Refused);
        assert_eq!(refused.heading, "The server didn't accept this code");
        assert!(refused.body.ends_with(SCAN_AGAIN));
        assert_eq!(refused.primary.as_deref(), Some("Close"));
        let other = setup_link(
            EN,
            &SetupLink::OtherServer {
                app: server.clone(),
                link: host("friends.example")?,
            },
        );
        assert_eq!(
            other.body,
            "This link is for friends.example, but this app works with pingers.example.org. \
             Install the Pingers app made for friends.example, then scan the code again."
        );
        let keys = setup_link(
            EN,
            &SetupLink::BuildKeysDiffer {
                server: server.clone(),
            },
        );
        assert_eq!(keys.heading, "This app doesn't have your server's keys yet");
        assert!(keys.body.contains("Install the app build made after"));
        let offline = setup_link(EN, &SetupLink::Unreachable { server });
        assert_eq!(offline.primary.as_deref(), Some("Try again"));
        assert_eq!(offline.not_now.as_deref(), Some("Not now"));
        Ok(())
    }
}
