// SPDX-License-Identifier: AGPL-3.0-only
//! What the phone shows outside the app after a wake (pingers-screens' `notifications`), in
//! words. The push itself carried nothing: these are written on the phone from what it
//! decrypted.

use pingers_screens::notifications::Notification;

use crate::Language;

/// What a locked phone shows of any Pingers notification that may show there at all: it names
/// no Party, person, Spot or message.
#[must_use]
pub fn lock_screen(language: Language) -> String {
    let Language::English = language;
    "New activity in a Party".into()
}

/// A notification, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationText {
    /// The first line: the Party, or who wrote and where.
    pub title: String,
    /// The text.
    pub body: String,
    /// What a locked phone shows instead ([`lock_screen`]), or `None` to show nothing there at
    /// all: a chat message or a location ask is not even announced on a locked screen.
    pub lock_screen: Option<String>,
}

/// A notification, in words.
#[must_use]
pub fn notification(language: Language, notification: &Notification) -> NotificationText {
    let Language::English = language;
    match notification {
        Notification::MeetupStarted(alert) => {
            let text = crate::home::alert(language, alert);
            NotificationText {
                title: text.title,
                body: text.body,
                lock_screen: Some(lock_screen(language)),
            }
        }
        Notification::JoinRequest {
            party_name, name, ..
        } => NotificationText {
            title: party_name.as_str().to_owned(),
            body: format!(
                "{} opened your invite. Compare the code on both phones, then let them in.",
                name.as_str()
            ),
            lock_screen: Some(lock_screen(language)),
        },
        Notification::ChatMessage {
            spot_name,
            author,
            body,
            ..
        } => NotificationText {
            title: format!("{} · {}", author.as_str(), spot_name.as_str()),
            body: body.as_str().to_owned(),
            lock_screen: None,
        },
        Notification::LocationRequest {
            spot_name, from, ..
        } => NotificationText {
            title: spot_name.as_str().to_owned(),
            body: format!("{} asks to see where you are.", from.as_str()),
            lock_screen: None,
        },
        // Notification may grow (non_exhaustive); a new kind gets words here.
        _ => NotificationText {
            title: "Pingers".into(),
            body: String::new(),
            lock_screen: Some(lock_screen(language)),
        },
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::notifications::notifications;

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn each_kind_reads_as_the_canvas_alert_does() -> Result<(), FixtureError> {
        let after = fixtures::snapshot()?;
        let mut before = after.clone();
        before.meetups.clear();
        before.location_requests.clear();
        for party in &mut before.parties {
            party.join_requests.clear();
        }
        let text: Vec<NotificationText> = notifications(&before, &after)
            .iter()
            .map(|found| notification(EN, found))
            .collect();
        let expected = [
            (
                "Climbing crew",
                "Ben and Chloé are at Riverside Park too. Say hi?",
            ),
            ("Ben · Riverside Park", "Coffee after?"),
            ("Chloé · Riverside Park", "Yes please"),
            ("Riverside Park", "Ben asks to see where you are."),
            (
                "Climbing crew",
                "Ben opened your invite. Compare the code on both phones, then let them in.",
            ),
        ];
        assert_eq!(
            text.iter()
                .map(|text| (text.title.as_str(), text.body.as_str()))
                .collect::<Vec<_>>(),
            expected
        );
        // Chat and location asks are not announced on a locked screen; the others say only
        // that something happened.
        let generic = Some(lock_screen(EN));
        assert_eq!(generic.as_deref(), Some("New activity in a Party"));
        assert_eq!(
            text.iter()
                .map(|text| text.lock_screen.clone())
                .collect::<Vec<_>>(),
            [generic.clone(), None, None, None, generic]
        );
        Ok(())
    }
}
