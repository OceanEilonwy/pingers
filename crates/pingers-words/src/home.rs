// SPDX-License-Identifier: AGPL-3.0-only
//! Home, and the co-presence alert, in words.

use pingers_screens::home::{CoPresenceAlert, Home, Sharing};
use pingers_screens::snapshot::PauseInFocus;

use crate::Language;
use crate::common::{avatar, list, members, plural, spots};
use crate::settings::paused_because;

/// The summary card, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharingText {
    /// "Sharing on · 3 parties", "Sharing off", "Paused · Focus is on".
    pub headline: String,
    /// "Pauses in Focus · 2 exclusion zones"; empty when there is nothing to say.
    pub detail: String,
}

/// A Party row, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyRowText {
    /// "Climbing crew".
    pub name: String,
    /// "CC".
    pub initials: String,
    /// "6 members · 3 spots".
    pub detail: String,
    /// "Live", when a Meetup is live.
    pub live: Option<String>,
}

/// The live Meetup card, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveMeetupText {
    /// "Live meetup · Climbing crew".
    pub heading: String,
    /// "Riverside Park".
    pub spot: String,
    /// The avatars: "BE", "CH", "You".
    pub avatars: Vec<String>,
    /// "Open".
    pub open: String,
}

/// The co-presence alert (in the app, and as the notification), in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlertText {
    /// The notification's title: the Party.
    pub title: String,
    /// "Ben and Chloé are at Riverside Park too. Say hi?"
    pub body: String,
    /// "now".
    pub when: String,
    /// "Open meetup".
    pub open: String,
    /// "Hide me here".
    pub hide: String,
    /// "Push arrived empty. Your phone decrypted the names and place itself."
    pub empty_push: String,
}

/// The Home screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeText {
    /// "Pingers".
    pub title: String,
    /// The summary card.
    pub sharing: SharingText,
    /// The live Meetup card.
    pub live_meetup: Option<LiveMeetupText>,
    /// "Your parties".
    pub parties_heading: String,
    /// The Parties.
    pub parties: Vec<PartyRowText>,
    /// "New party".
    pub new_party: String,
    /// The tabs: "Parties", "Meetups", "Privacy".
    pub tabs: [String; 3],
    /// An alert that arrived while the app was open.
    pub alert: Option<AlertText>,
}

/// The co-presence alert, in words.
#[must_use]
pub fn alert(language: Language, alert: &CoPresenceAlert) -> AlertText {
    let Language::English = language;
    let others: Vec<String> = alert
        .others
        .iter()
        .map(|name| name.as_str().to_owned())
        .collect();
    let verb = if others.len() == 1 { "is" } else { "are" };
    AlertText {
        title: alert.party_name.as_str().to_owned(),
        body: format!(
            "{} {verb} at {} too. Say hi?",
            list(language, &others),
            alert.spot_name.as_str()
        ),
        when: "now".into(),
        open: "Open meetup".into(),
        hide: "Hide me here".into(),
        empty_push: "Push arrived empty. Your phone decrypted the names and place itself.".into(),
    }
}

fn sharing(language: Language, home: &Home) -> SharingText {
    let Language::English = language;
    let summary = home.sharing;
    let headline = match summary.sharing {
        Sharing::On => format!(
            "Sharing on · {}",
            plural(language, summary.party_count, "party", "parties")
        ),
        Sharing::Off => "Sharing off".into(),
        Sharing::Paused(reason) => format!("Paused · {}", paused_because(language, reason)),
    };
    let mut parts = Vec::new();
    if matches!(summary.pause_in_focus, PauseInFocus::On) {
        parts.push("Pauses in Focus".to_owned());
    }
    if summary.exclusion_zones > 0 {
        parts.push(plural(
            language,
            summary.exclusion_zones,
            "exclusion zone",
            "exclusion zones",
        ));
    }
    SharingText {
        headline,
        detail: parts.join(" · "),
    }
}

/// The Home screen, in words.
#[must_use]
pub fn home(language: Language, home: &Home) -> HomeText {
    let Language::English = language;
    HomeText {
        title: "Pingers".into(),
        sharing: sharing(language, home),
        live_meetup: home.live_meetup.as_ref().map(|card| LiveMeetupText {
            heading: format!("Live meetup · {}", card.party_name.as_str()),
            spot: card.spot_name.as_str().to_owned(),
            avatars: card
                .people
                .iter()
                .map(|person| avatar(language, person))
                .collect(),
            open: "Open".into(),
        }),
        parties_heading: "Your parties".into(),
        parties: home
            .parties
            .iter()
            .map(|row| PartyRowText {
                name: row.name.as_str().to_owned(),
                initials: row.initials.as_str().to_owned(),
                detail: format!(
                    "{} · {}",
                    members(language, row.members),
                    spots(language, row.spots)
                ),
                live: row.live.then(|| "Live".to_owned()),
            })
            .collect(),
        new_party: crate::party::NEW_PARTY.into(),
        tabs: ["Parties".into(), "Meetups".into(), "Privacy".into()],
        alert: home.alert.as_ref().map(|found| alert(language, found)),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_gate::{GateDecision, LocationSharing, SuppressReason};
    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::home::home as screen;

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn home_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = home(EN, &screen(&snapshot));
        assert_eq!(text.title, "Pingers");
        assert_eq!(text.sharing.headline, "Sharing on · 3 parties");
        assert_eq!(text.sharing.detail, "Pauses in Focus · 2 exclusion zones");
        let card = text.live_meetup.ok_or(FixtureError::Missing)?;
        assert_eq!(card.heading, "Live meetup · Climbing crew");
        assert_eq!(card.spot, "Riverside Park");
        assert_eq!(card.avatars, ["BE", "CH", "You"]);
        assert_eq!(card.open, "Open");
        let rows: Vec<_> = text
            .parties
            .iter()
            .map(|row| (row.name.as_str(), row.detail.as_str(), row.live.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Climbing crew", "6 members · 3 spots", Some("Live")),
                ("Uni flat", "4 members · 2 spots", None),
                ("Saturday 5-a-side", "11 members · 1 spot", None),
            ]
        );
        assert_eq!(
            text.parties.first().map(|row| row.initials.as_str()),
            Some("CC")
        );
        assert_eq!(text.parties_heading, "Your parties");
        assert_eq!(text.new_party, "New party");
        assert_eq!(text.tabs, ["Parties", "Meetups", "Privacy"]);
        assert_eq!(text.alert, None);
        Ok(())
    }

    #[test]
    fn the_summary_follows_sharing_and_settings() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.gate = GateDecision::Suppress(SuppressReason::Focus);
        assert_eq!(
            home(EN, &screen(&snapshot)).sharing.headline,
            "Paused · Focus is on"
        );
        snapshot.settings.location_sharing = LocationSharing::Off;
        snapshot.settings.pause_in_focus = PauseInFocus::Off;
        snapshot.zones.truncate(1);
        let text = home(EN, &screen(&snapshot));
        assert_eq!(text.sharing.headline, "Sharing off");
        assert_eq!(text.sharing.detail, "1 exclusion zone");
        snapshot.zones.clear();
        assert_eq!(home(EN, &screen(&snapshot)).sharing.detail, "");
        snapshot.parties.truncate(1);
        snapshot.settings.location_sharing = LocationSharing::On;
        snapshot.gate = GateDecision::Share;
        assert_eq!(
            home(EN, &screen(&snapshot)).sharing.headline,
            "Sharing on · 1 party"
        );
        Ok(())
    }

    #[test]
    fn the_alert_names_who_and_where() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.alert = Some(fixtures::meetup_id());
        let text = home(EN, &screen(&snapshot))
            .alert
            .ok_or(FixtureError::Missing)?;
        assert_eq!(text.title, "Climbing crew");
        assert_eq!(
            text.body,
            "Ben and Chloé are at Riverside Park too. Say hi?"
        );
        assert_eq!(
            (text.when.as_str(), text.open.as_str(), text.hide.as_str()),
            ("now", "Open meetup", "Hide me here")
        );
        assert!(text.empty_push.starts_with("Push arrived empty."));
        for meetup in &mut snapshot.meetups {
            meetup.others.truncate(1);
        }
        let text = home(EN, &screen(&snapshot))
            .alert
            .ok_or(FixtureError::Missing)?;
        assert_eq!(text.body, "Ben is at Riverside Park too. Say hi?");
        Ok(())
    }
}
