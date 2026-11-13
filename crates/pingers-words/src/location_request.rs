// SPDX-License-Identifier: AGPL-3.0-only
//! A request to see this device's exact location, in words.

use pingers_meetup::GrantScope;
use pingers_screens::location_request::LocationRequestView;

use crate::Language;

/// A scope choice, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeText {
    /// The scope.
    pub scope: GrantScope,
    /// "This meetup", "15 minutes".
    pub label: String,
}

/// The Location request screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocationRequestText {
    /// "Ben wants to see exactly where you are".
    pub title: String,
    /// "Ben can already see you're in Riverside Park."
    pub already: String,
    /// "Share for".
    pub share_for: String,
    /// The scopes.
    pub scopes: Vec<ScopeText>,
    /// "Ask to see Ben too".
    pub ask_back: String,
    /// "Always allow Ben in Climbing crew".
    pub always: String,
    /// "Only Ben gets the key. …"
    pub note: String,
    /// "Not now".
    pub not_now: String,
    /// "Allow".
    pub allow: String,
    /// "\"Not now\" is silent. Ben only sees the request expire."
    pub silent: String,
}

/// The Location request screen, in words.
#[must_use]
pub fn location_request(language: Language, view: &LocationRequestView) -> LocationRequestText {
    let Language::English = language;
    let from = view.from.name.as_str();
    LocationRequestText {
        title: format!("{from} wants to see exactly where you are"),
        already: format!(
            "{from} can already see you're in {}.",
            view.spot_name.as_str()
        ),
        share_for: "Share for".into(),
        scopes: vec![
            ScopeText {
                scope: GrantScope::ThisMeetup,
                label: "This meetup".into(),
            },
            ScopeText {
                scope: GrantScope::FifteenMinutes,
                label: "15 minutes".into(),
            },
        ],
        ask_back: format!("Ask to see {from} too"),
        always: format!("Always allow {from} in {}", view.party_name.as_str()),
        note: format!(
            "Only {from} gets the key. Others in the meetup and the server can't tell who you \
             share with."
        ),
        not_now: "Not now".into(),
        allow: "Allow".into(),
        silent: format!("\"Not now\" is silent. {from} only sees the request expire."),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::location_request::{AnswerDraft, location_request as screen};

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn the_request_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = location_request(
            EN,
            &screen(
                &snapshot,
                fixtures::location_request_id(),
                AnswerDraft::INITIAL,
            )?,
        );
        assert_eq!(text.title, "Ben wants to see exactly where you are");
        assert_eq!(
            text.already,
            "Ben can already see you're in Riverside Park."
        );
        let scopes: Vec<_> = text
            .scopes
            .iter()
            .map(|scope| scope.label.as_str())
            .collect();
        assert_eq!(scopes, ["This meetup", "15 minutes"]);
        assert_eq!(text.ask_back, "Ask to see Ben too");
        assert_eq!(text.always, "Always allow Ben in Climbing crew");
        assert_eq!(
            text.note,
            "Only Ben gets the key. Others in the meetup and the server can't tell who you share with."
        );
        assert_eq!(
            text.silent,
            "\"Not now\" is silent. Ben only sees the request expire."
        );
        assert_eq!(
            (
                text.share_for.as_str(),
                text.not_now.as_str(),
                text.allow.as_str()
            ),
            ("Share for", "Not now", "Allow")
        );
        Ok(())
    }
}
