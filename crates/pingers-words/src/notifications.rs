// SPDX-License-Identifier: AGPL-3.0-only
//! What the phone shows outside the app after a wake (pingers-screens' `notifications`), in
//! words. The push itself carried nothing: these are written on the phone from what it
//! decrypted.

use pingers_screens::notifications::Notification;

use crate::Language;

/// A notification, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationText {
    /// The first line: the Party, or who wrote and where.
    pub title: String,
    /// The text.
    pub body: String,
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
        },
        Notification::ChatMessage {
            spot_name,
            author,
            body,
            ..
        } => NotificationText {
            title: format!("{} · {}", author.as_str(), spot_name.as_str()),
            body: body.as_str().to_owned(),
        },
        Notification::LocationRequest {
            spot_name, from, ..
        } => NotificationText {
            title: spot_name.as_str().to_owned(),
            body: format!("{} asks to see where you are.", from.as_str()),
        },
        // Notification may grow (non_exhaustive); a new kind gets words here.
        _ => NotificationText {
            title: "Pingers".into(),
            body: String::new(),
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
            party.invites.clear();
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
        Ok(())
    }
}
