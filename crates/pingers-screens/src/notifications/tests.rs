// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_proto::{ChatMessageId, MailboxId};

use super::*;
use crate::fixtures::{self, FixtureError, chloe, location_request_id, meetup_id, sam};
use crate::snapshot::ChatMessage;
use crate::snapshot::Party;

fn names(texts: &[&str]) -> Result<Vec<Name>, FixtureError> {
    texts
        .iter()
        .map(|text| Name::new(text).map_err(|_| FixtureError::Missing))
        .collect()
}

#[test]
fn nothing_new_shows_nothing() -> Result<(), FixtureError> {
    let snapshot = fixtures::snapshot()?;
    assert_eq!(notifications(&snapshot, &snapshot), []);
    Ok(())
}

#[test]
fn a_new_meetup_shows_its_start_and_the_others_messages_but_not_mine() -> Result<(), FixtureError> {
    let after = fixtures::snapshot()?;
    let mut before = after.clone();
    before.meetups.clear();
    before.location_requests.clear();
    let found = notifications(&before, &after);
    let others = names(&["Ben", "Chloé"])?;
    assert_eq!(
        found.first(),
        Some(&Notification::MeetupStarted(CoPresenceAlert {
            meetup: meetup_id(),
            party_name: Name::new("Climbing crew").map_err(|_| FixtureError::Missing)?,
            others,
            spot_name: Name::new("Riverside Park").map_err(|_| FixtureError::Missing)?,
        }))
    );
    let chat: Vec<(&str, &str)> = found
        .iter()
        .filter_map(|notification| match notification {
            Notification::ChatMessage { author, body, .. } => {
                Some((author.as_str(), body.as_str()))
            }
            Notification::MeetupStarted(_)
            | Notification::JoinRequest { .. }
            | Notification::LocationRequest { .. } => None,
        })
        .collect();
    // Ben's message and Chloé's reply; Sam's own "On my way" is left out.
    assert_eq!(chat, [("Ben", "Coffee after?"), ("Chloé", "Yes please")]);
    assert!(matches!(
        found.last(),
        Some(Notification::LocationRequest { from, .. }) if from.as_str() == "Ben"
    ));
    Ok(())
}

#[test]
fn a_new_message_in_a_live_meetup_shows_once_under_the_meetup_s_key() -> Result<(), FixtureError> {
    let before = fixtures::snapshot()?;
    let mut after = before.clone();
    let meetup = after.meetups.first_mut().ok_or(FixtureError::Missing)?;
    meetup.chat.push(ChatEntry::Message(ChatMessage {
        id: ChatMessageId::from_bytes([77; 16]),
        author: chloe(),
        body: ChatBody::new("Here now".to_owned()).map_err(|_| FixtureError::Missing)?,
        at: pingers_time::UnixSeconds::new(1),
        replies: Vec::new(),
    }));
    meetup.chat.push(ChatEntry::Message(ChatMessage {
        id: ChatMessageId::from_bytes([78; 16]),
        author: sam(),
        body: ChatBody::new("Me too".to_owned()).map_err(|_| FixtureError::Missing)?,
        at: pingers_time::UnixSeconds::new(2),
        replies: Vec::new(),
    }));
    let found = notifications(&before, &after);
    assert_eq!(found.len(), 1);
    let only = found.first().ok_or(FixtureError::Missing)?;
    assert_eq!(only.key(), NotificationKey::Chat(meetup_id()));
    assert!(matches!(
        only,
        Notification::ChatMessage { author, spot_name, .. }
            if author.as_str() == "Chloé" && spot_name.as_str() == "Riverside Park"
    ));
    Ok(())
}

#[test]
fn a_join_request_since_shows_a_notification() -> Result<(), FixtureError> {
    let after = fixtures::snapshot()?;
    let mut before = after.clone();
    for party in &mut before.parties {
        party.join_requests.clear();
    }
    let found = notifications(&before, &after);
    assert_eq!(
        found,
        [Notification::JoinRequest {
            party: after
                .parties
                .first()
                .map(|party: &Party| party.id)
                .ok_or(FixtureError::Missing)?,
            invite: MailboxId::from_bytes([50; 16]),
            party_name: Name::new("Climbing crew").map_err(|_| FixtureError::Missing)?,
            name: Name::new("Ben").map_err(|_| FixtureError::Missing)?,
        }]
    );
    assert_eq!(
        found.first().map(Notification::key),
        Some(NotificationKey::Invite(MailboxId::from_bytes([50; 16])))
    );
    // Still waiting on the next wake: not shown again; a second request is.
    assert!(notifications(&after, &after).is_empty());
    let mut later = after.clone();
    if let Some(party) = later.parties.first_mut()
        && let Some(first) = party.join_requests.first().cloned()
    {
        party.join_requests.push(crate::snapshot::JoinRequest {
            id: MailboxId::from_bytes([53; 16]),
            name: Name::new("Dev").map_err(|_| FixtureError::Missing)?,
            ..first
        });
    }
    assert_eq!(notifications(&after, &later).len(), 1);
    Ok(())
}

#[test]
fn a_new_location_request_shows_who_asks() -> Result<(), FixtureError> {
    let after = fixtures::snapshot()?;
    let mut before = after.clone();
    before.location_requests.clear();
    let found = notifications(&before, &after);
    assert_eq!(found.len(), 1);
    assert_eq!(
        found.first().map(Notification::key),
        Some(NotificationKey::LocationRequest(location_request_id()))
    );
    assert!(matches!(
        found.first(),
        Some(Notification::LocationRequest { meetup, .. }) if *meetup == meetup_id()
    ));
    Ok(())
}

#[test]
fn the_stable_id_is_per_kind_and_per_thing() {
    let meetup = NotificationKey::Meetup(meetup_id());
    let chat = NotificationKey::Chat(meetup_id());
    assert_eq!(meetup.stable_id(), 0x0114_1414);
    assert_eq!(chat.stable_id(), 0x0214_1414);
    assert_eq!(
        NotificationKey::Invite(MailboxId::from_bytes([50; 16])).stable_id(),
        0x0332_3232
    );
    assert_eq!(
        NotificationKey::LocationRequest(location_request_id()).stable_id(),
        0x041e_1e1e
    );
}

#[test]
fn ended_meetups_and_party_changes_are_reported() -> Result<(), FixtureError> {
    let before = fixtures::snapshot()?;
    let mut after = before.clone();
    after.meetups.clear();
    let ended = changes(&before, &after);
    assert_eq!(ended.meetups_ended, [meetup_id()]);
    assert!(!ended.parties_changed);
    assert!(ended.notifications.is_empty());
    after.parties.pop();
    assert!(changes(&before, &after).parties_changed);
    assert_eq!(changes(&before, &before).meetups_ended, []);
    Ok(())
}
