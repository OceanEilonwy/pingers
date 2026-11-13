// SPDX-License-Identifier: AGPL-3.0-only
//! A live Meetup and the Meetup ended summary, in words.

use pingers_screens::clock::Minutes;
use pingers_screens::common::{Me, Person};
use pingers_screens::meetup::{
    MeetPointCard, MeetPointName, MeetupEnded, MeetupView, PeerLocation,
};

use crate::Language;
use crate::common::{
    avatar, direction, distance, list, person_in_sentence, plural, time_range, updated,
};

/// What the meet point is called in a sentence: "the bandstand", "the pin", "Ben".
#[must_use]
pub fn meet_point_name(language: Language, name: &MeetPointName) -> String {
    let Language::English = language;
    match name {
        MeetPointName::Label(label) => label.as_str().to_owned(),
        MeetPointName::Pin => "the pin".into(),
        MeetPointName::AtMember(person) => match person.me {
            Me::Me => "your spot".into(),
            Me::Other => person.name.as_str().to_owned(),
        },
    }
}

/// The meet point card, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetPointText {
    /// "Meet at the bandstand".
    pub title: String,
    /// "Set by Ben · 85 m away".
    pub detail: String,
    /// "Go".
    pub go: String,
}

/// A member row, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberText {
    /// "BE".
    pub avatar: String,
    /// "Ben".
    pub name: String,
    /// "120 m north-east · sharing with you", "Here · exact spot private".
    pub status: String,
    /// "Updated 40 s ago", for a member who shares, when this device has a fix.
    pub updated: Option<String>,
    /// "Ask to see Chloé's exact location", for a member whose location is private.
    pub ask: Option<String>,
    /// "Navigate to Ben", for a member who shares.
    pub navigate: Option<String>,
}

/// The Meetup screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetupText {
    /// "Riverside Park · 3 here".
    pub title: String,
    /// "Climbing crew".
    pub party: String,
    /// The meet point card.
    pub meet_point: Option<MeetPointText>,
    /// The members.
    pub members: Vec<MemberText>,
    /// "Chat · 3".
    pub chat: String,
    /// "Meet point".
    pub meet_point_button: String,
    /// "Compass".
    pub compass: String,
    /// "Only the 3 people here can see this meetup."
    pub only_here: String,
    /// "Your exact spot: shared with Ben", "Your exact spot: private".
    pub shared_with: String,
    /// "Manage".
    pub manage: String,
}

fn set_by(language: Language, person: &Person) -> String {
    let Language::English = language;
    format!("Set by {}", person_in_sentence(language, person))
}

fn meet_point(language: Language, card: &MeetPointCard) -> MeetPointText {
    let Language::English = language;
    let setter = set_by(language, &card.set_by);
    MeetPointText {
        title: format!("Meet at {}", meet_point_name(language, &card.name)),
        detail: match card.distance {
            Some(far) => format!("{setter} · {} away", distance(language, far)),
            None => setter,
        },
        go: "Go".into(),
    }
}

/// A member's status line.
#[must_use]
pub fn peer_status(language: Language, location: &PeerLocation) -> String {
    let Language::English = language;
    match location {
        PeerLocation::Shared {
            distance: Some(far),
            direction: Some(way),
            ..
        } => format!(
            "{} {} · sharing with you",
            distance(language, *far),
            direction(language, *way)
        ),
        PeerLocation::Shared { .. } => "Sharing with you".into(),
        PeerLocation::Private => "Here · exact spot private".into(),
        PeerLocation::Asked => "Here · asked to see exact spot".into(),
    }
}

/// The Meetup screen, in words.
#[must_use]
pub fn meetup(language: Language, view: &MeetupView) -> MeetupText {
    let Language::English = language;
    let shared: Vec<String> = view
        .shared_with
        .iter()
        .map(|name| name.as_str().to_owned())
        .collect();
    MeetupText {
        title: format!("{} · {} here", view.spot_name.as_str(), view.here_count),
        party: view.party_name.as_str().to_owned(),
        meet_point: view
            .meet_point
            .as_ref()
            .map(|card| meet_point(language, card)),
        members: view
            .members
            .iter()
            .map(|member| {
                let name = member.person.name.as_str();
                MemberText {
                    avatar: avatar(language, &member.person),
                    name: name.to_owned(),
                    status: peer_status(language, &member.location),
                    updated: match member.location {
                        PeerLocation::Shared { age: Some(age), .. } => Some(updated(language, age)),
                        PeerLocation::Shared { age: None, .. }
                        | PeerLocation::Private
                        | PeerLocation::Asked => None,
                    },
                    ask: matches!(member.location, PeerLocation::Private)
                        .then(|| format!("Ask to see {name}'s exact location")),
                    navigate: matches!(member.location, PeerLocation::Shared { .. })
                        .then(|| format!("Navigate to {name}")),
                }
            })
            .collect(),
        chat: format!("Chat · {}", view.chat_count),
        meet_point_button: "Meet point".into(),
        compass: "Compass".into(),
        only_here: if view.here_count == 1 {
            "Only the 1 person here can see this meetup.".into()
        } else {
            format!(
                "Only the {} people here can see this meetup.",
                view.here_count
            )
        },
        shared_with: if shared.is_empty() {
            "Your exact spot: private".into()
        } else {
            format!("Your exact spot: shared with {}", list(language, &shared))
        },
        manage: "Manage".into(),
    }
}

/// A Meetup's length: "1 h 25", "45 min".
#[must_use]
pub fn duration(language: Language, length: Minutes) -> String {
    let Language::English = language;
    if length.hours() == 0 {
        format!("{} min", length.minutes_past_hour())
    } else {
        format!("{} h {:02}", length.hours(), length.minutes_past_hour())
    }
}

/// The Meetup ended screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetupEndedText {
    /// "Meetup ended".
    pub title: String,
    /// "Everyone left Riverside Park".
    pub subtitle: String,
    /// "1 h 25".
    pub duration: String,
    /// "18:40–20:05".
    pub range: String,
    /// "3".
    pub people: String,
    /// "people met".
    pub people_label: String,
    /// "24".
    pub messages: String,
    /// "messages".
    pub messages_label: String,
    /// "Chat and locations erased".
    pub erased: String,
    /// What erased means.
    pub erased_detail: String,
    /// "Done".
    pub done: String,
}

/// The Meetup ended screen, in words.
#[must_use]
pub fn meetup_ended(language: Language, view: &MeetupEnded) -> MeetupEndedText {
    let Language::English = language;
    MeetupEndedText {
        title: "Meetup ended".into(),
        subtitle: format!("Everyone left {}", view.spot_name.as_str()),
        duration: duration(language, view.duration),
        range: time_range(language, view.from, view.to),
        people: view.people.to_string(),
        people_label: if view.people == 1 {
            "person met".into()
        } else {
            "people met".into()
        },
        messages: view.messages.to_string(),
        messages_label: plural(language, view.messages, "message", "messages")
            .trim_start_matches(char::is_numeric)
            .trim_start()
            .to_owned(),
        erased: "Chat and locations erased".into(),
        erased_detail: "Every phone deleted the meetup keys, so nothing can be read again, \
                        including by the server."
            .into(),
        done: crate::common::done(language),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::meetup::{meetup as screen, meetup_ended as ended_screen};
    use pingers_screens::snapshot::{MeetPlace, MeetPointSnapshot, PeerAccess};

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn the_meetup_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = meetup(EN, &screen(&snapshot, fixtures::meetup_id())?);
        assert_eq!(text.title, "Riverside Park · 3 here");
        assert_eq!(text.party, "Climbing crew");
        let point = text.meet_point.ok_or(FixtureError::Missing)?;
        assert_eq!(point.title, "Meet at the bandstand");
        assert_eq!(point.detail, "Set by Ben · 85 m away");
        assert_eq!(point.go, "Go");
        let [ben, chloe] = text.members.as_slice() else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(
            (ben.avatar.as_str(), ben.status.as_str()),
            ("BE", "120 m north-east · sharing with you")
        );
        assert_eq!(
            (ben.ask.as_deref(), ben.navigate.as_deref()),
            (None, Some("Navigate to Ben"))
        );
        assert_eq!(ben.updated.as_deref(), Some("Updated 1 min ago"));
        assert_eq!(chloe.updated, None);
        assert_eq!(chloe.status, "Here · exact spot private");
        assert_eq!(
            chloe.ask.as_deref(),
            Some("Ask to see Chloé's exact location")
        );
        assert_eq!(chloe.navigate, None);
        assert_eq!(text.chat, "Chat · 3");
        assert_eq!(
            text.only_here,
            "Only the 3 people here can see this meetup."
        );
        assert_eq!(text.shared_with, "Your exact spot: shared with Ben");
        assert_eq!(
            (
                text.meet_point_button.as_str(),
                text.compass.as_str(),
                text.manage.as_str()
            ),
            ("Meet point", "Compass", "Manage")
        );
        Ok(())
    }

    #[test]
    fn states_without_a_fix_or_a_grant() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.own_position = None;
        for meetup in &mut snapshot.meetups {
            meetup.sharing_with.clear();
            meetup.others.truncate(1);
            meetup.meet_point = Some(MeetPointSnapshot {
                place: MeetPlace::AtMember(fixtures::sam()),
                set_by: fixtures::sam(),
            });
        }
        let text = meetup(EN, &screen(&snapshot, fixtures::meetup_id())?);
        assert_eq!(
            text.members.first().map(|row| row.status.as_str()),
            Some("Sharing with you")
        );
        assert_eq!(
            text.members.first().map(|row| row.updated.is_none()),
            Some(true),
            "without a fix, no age"
        );
        assert_eq!(text.shared_with, "Your exact spot: private");
        let point = text.meet_point.ok_or(FixtureError::Missing)?;
        assert_eq!(
            (point.title.as_str(), point.detail.as_str()),
            ("Meet at your spot", "Set by you")
        );
        for meetup in &mut snapshot.meetups {
            meetup.others.clear();
            meetup.meet_point = Some(MeetPointSnapshot {
                place: MeetPlace::Fixed {
                    position: fixtures::bandstand()?,
                    label: None,
                },
                set_by: fixtures::sam(),
            });
        }
        let text = meetup(EN, &screen(&snapshot, fixtures::meetup_id())?);
        assert_eq!(
            text.only_here,
            "Only the 1 person here can see this meetup."
        );
        assert_eq!(
            text.meet_point.map(|point| point.title),
            Some("Meet at the pin".to_owned())
        );
        Ok(())
    }

    #[test]
    fn asked_and_named_meet_points() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for meetup in &mut snapshot.meetups {
            for peer in &mut meetup.others {
                peer.access = PeerAccess::Asked;
            }
            meetup.meet_point = Some(MeetPointSnapshot {
                place: MeetPlace::AtMember(fixtures::ben()),
                set_by: fixtures::ben(),
            });
        }
        let text = meetup(EN, &screen(&snapshot, fixtures::meetup_id())?);
        assert!(
            text.members
                .iter()
                .all(|row| row.status == "Here · asked to see exact spot" && row.ask.is_none())
        );
        assert_eq!(
            text.meet_point.map(|point| point.title),
            Some("Meet at Ben".to_owned())
        );
        Ok(())
    }

    #[test]
    fn meetup_ended_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = meetup_ended(
            EN,
            &ended_screen(&snapshot, fixtures::clock()).ok_or(FixtureError::Missing)?,
        );
        assert_eq!(text.title, "Meetup ended");
        assert_eq!(text.subtitle, "Everyone left Riverside Park");
        assert_eq!(
            (text.duration.as_str(), text.range.as_str()),
            ("1 h 25", "18:40–20:05")
        );
        assert_eq!(
            (text.people.as_str(), text.people_label.as_str()),
            ("3", "people met")
        );
        assert_eq!(
            (text.messages.as_str(), text.messages_label.as_str()),
            ("24", "messages")
        );
        assert_eq!(text.erased, "Chat and locations erased");
        assert!(
            text.erased_detail
                .starts_with("Every phone deleted the meetup keys")
        );
        assert_eq!(text.done, "Done");
        Ok(())
    }

    #[test]
    fn short_meetups_and_single_counts() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        if let Some(ended) = &mut snapshot.ended_meetup {
            ended.ended_at = fixtures::at(19, 25);
            ended.people = 1;
            ended.messages = 1;
        }
        let text = meetup_ended(
            EN,
            &ended_screen(&snapshot, fixtures::clock()).ok_or(FixtureError::Missing)?,
        );
        assert_eq!(text.duration, "45 min");
        assert_eq!(
            (text.people_label.as_str(), text.messages_label.as_str()),
            ("person met", "message")
        );
        assert_eq!(duration(EN, Minutes::new(60)), "1 h 00");
        assert_eq!(duration(EN, Minutes::new(125)), "2 h 05");
        Ok(())
    }
}
