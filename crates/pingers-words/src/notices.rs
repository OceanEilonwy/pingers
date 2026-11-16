// SPDX-License-Identifier: AGPL-3.0-only
//! The notice at the top of Home, in words (canvas `Notices`).

use pingers_screens::notices::{HomeNotice, NoticeAction, NoticeView};

use crate::Language;

/// A notice, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeText {
    /// "Your phone's clock is wrong".
    pub title: String,
    /// What it means and what to do.
    pub body: String,
    /// The link, if the notice has one: "Open date and time settings".
    pub action: Option<String>,
}

/// A notice, in words.
#[must_use]
pub fn notice(language: Language, view: &NoticeView) -> NoticeText {
    let Language::English = language;
    let party = view.party.as_ref().map(|name| name.as_str().to_owned());
    let (title, body) = match view.notice {
        HomeNotice::ClockWrong => (
            "Your phone's clock is wrong".to_owned(),
            "Pingers has stopped checking in until the time is right. Turn on automatic date and \
             time."
                .to_owned(),
        ),
        HomeNotice::RelayKeyChanged => (
            match &party {
                Some(party) => format!("{party}'s privacy relay key changed"),
                None => "Your privacy relay key changed".to_owned(),
            },
            "The privacy relay's key doesn't match the one your party agreed on. Pingers has \
             stopped anonymous traffic until it does."
                .to_owned(),
        ),
        HomeNotice::ServerMayHide => (
            "Your server may be hiding people".to_owned(),
            "Phones in your last Meetup were shown different check-ins. That can stop friends \
             finding each other. It doesn't tell the server where you are."
                .to_owned(),
        ),
        HomeNotice::DndUnreadable => (
            "Pingers can't see Do Not Disturb".to_owned(),
            "“Pause during Focus” won't work on this phone. Turn off Location sharing yourself \
             when you want a break."
                .to_owned(),
        ),
        HomeNotice::UnansweredKnock { .. } => (
            match &party {
                Some(party) => format!("Someone from {party} seemed to be here"),
                None => "Someone from your Party seemed to be here".to_owned(),
            },
            match &view.spot {
                Some(spot) => format!(
                    "At {}, but their phone didn't answer. It may be asleep, or they've left.",
                    spot.as_str()
                ),
                None => "Their phone didn't answer. It may be asleep, or they've left.".to_owned(),
            },
        ),
        HomeNotice::ServerBusy => (
            "Your server is very busy".to_owned(),
            "Checking for friends is taking longer than usual, so some Meetups may start late."
                .to_owned(),
        ),
    };
    NoticeText {
        title,
        body,
        action: view.action.map(|action| {
            match action {
                NoticeAction::DateTimeSettings => "Open date and time settings",
                NoticeAction::PartySettings(_) => "Party settings",
                NoticeAction::PrivacySettings => "Privacy settings",
            }
            .to_owned()
        }),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::notices::{RaisedNotice, top_notice};
    use pingers_time::UnixSeconds;

    use super::*;

    const EN: Language = Language::English;
    const NOW: UnixSeconds = UnixSeconds::new(10_000);

    fn shown(notice: HomeNotice) -> Result<NoticeText, FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.parties.truncate(1);
        let raised = [RaisedNotice { notice, at: NOW }];
        let view = top_notice(&snapshot, &raised, NOW).ok_or(FixtureError::Missing)?;
        Ok(notice_text(&view))
    }

    fn notice_text(view: &NoticeView) -> NoticeText {
        notice(EN, view)
    }

    #[test]
    fn each_notice_reads_like_the_canvas() -> Result<(), FixtureError> {
        let clock = shown(HomeNotice::ClockWrong)?;
        assert_eq!(clock.title, "Your phone's clock is wrong");
        assert_eq!(clock.action.as_deref(), Some("Open date and time settings"));
        let relay = shown(HomeNotice::RelayKeyChanged)?;
        assert_eq!(relay.title, "Climbing crew's privacy relay key changed");
        assert_eq!(relay.action.as_deref(), Some("Party settings"));
        let hiding = shown(HomeNotice::ServerMayHide)?;
        assert_eq!(hiding.title, "Your server may be hiding people");
        assert_eq!(hiding.action, None);
        let dnd = shown(HomeNotice::DndUnreadable)?;
        assert_eq!(dnd.title, "Pingers can't see Do Not Disturb");
        assert_eq!(dnd.action.as_deref(), Some("Privacy settings"));
        let busy = shown(HomeNotice::ServerBusy)?;
        assert_eq!(busy.title, "Your server is very busy");
        Ok(())
    }

    #[test]
    fn an_unanswered_knock_names_the_party_and_spot() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let party = snapshot.parties.first().ok_or(FixtureError::Missing)?;
        let spot = party.spots.get(1).ok_or(FixtureError::Missing)?.id;
        let text = shown(HomeNotice::UnansweredKnock {
            party: party.id,
            spot,
        })?;
        assert_eq!(text.title, "Someone from Climbing crew seemed to be here");
        assert_eq!(
            text.body,
            "At The Wall climbing gym, but their phone didn't answer. It may be asleep, or \
             they've left."
        );
        Ok(())
    }
}
