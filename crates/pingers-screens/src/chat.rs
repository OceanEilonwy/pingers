// SPDX-License-Identifier: AGPL-3.0-only
//! A Meetup's chat: one main thread, and one level of replies.

use pingers_proto::{ChatBody, ChatMessageId, GroupId, MeetPointLabel};

use crate::ScreenError;
use crate::clock::{LocalClock, LocalTime};
use crate::common::{Name, Person};
use crate::home::meetup_names;
use crate::snapshot::{ChatEntry, ChatMessage, ClientSnapshot, Meetup};

/// A message as the chat shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessageView {
    /// The message.
    pub id: ChatMessageId,
    /// Who wrote it.
    pub author: Person,
    /// What it says.
    pub body: ChatBody,
    /// When, local time.
    pub at: LocalTime,
    /// Its replies; always empty for a reply.
    pub replies: Vec<ChatMessageView>,
}

/// One item in the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatItem {
    /// "Ben joined · 18:40".
    Joined {
        /// Who.
        person: Person,
        /// When, local time.
        at: LocalTime,
    },
    /// A message and its thread.
    Message(ChatMessageView),
    /// "Set the meet point: the bandstand".
    MeetPointSet {
        /// Who.
        author: Person,
        /// The label, if any.
        label: Option<MeetPointLabel>,
    },
}

/// The chat screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatView {
    /// The Spot's name.
    pub spot_name: Name,
    /// Everyone in the Meetup, others first and this device last ("Ben, Chloé and you").
    pub participants: Vec<Person>,
    /// The chat, oldest first. Entries by members who have left are not shown: their names
    /// went with them.
    pub items: Vec<ChatItem>,
    /// The message whose thread the composer replies in; `None` for the main thread.
    pub replying_to: Option<ChatMessageView>,
}

fn message(meetup: &Meetup, message: &ChatMessage, clock: LocalClock) -> Option<ChatMessageView> {
    Some(ChatMessageView {
        id: message.id,
        author: meetup.person(message.author).ok()?,
        body: message.body.clone(),
        at: clock.time_of(message.at),
        replies: message
            .replies
            .iter()
            .filter_map(|reply| {
                Some(ChatMessageView {
                    id: reply.id,
                    author: meetup.person(reply.author).ok()?,
                    body: reply.body.clone(),
                    at: clock.time_of(reply.at),
                    replies: Vec::new(),
                })
            })
            .collect(),
    })
}

/// The chat screen.
///
/// # Errors
///
/// [`ScreenError::UnknownMeetup`] if the snapshot has no such Meetup, or no longer has its
/// Party or Spot.
pub fn chat(
    snapshot: &ClientSnapshot,
    id: GroupId,
    replying_to: Option<ChatMessageId>,
    clock: LocalClock,
) -> Result<ChatView, ScreenError> {
    let meetup = snapshot.meetup(id)?;
    let names = meetup_names(snapshot, meetup).ok_or(ScreenError::UnknownMeetup)?;
    let items: Vec<ChatItem> = meetup
        .chat
        .iter()
        .filter_map(|entry| match entry {
            ChatEntry::Joined { leaf, at } => Some(ChatItem::Joined {
                person: meetup.person(*leaf).ok()?,
                at: clock.time_of(*at),
            }),
            ChatEntry::Message(entry) => message(meetup, entry, clock).map(ChatItem::Message),
            ChatEntry::MeetPointSet { author, label } => Some(ChatItem::MeetPointSet {
                author: meetup.person(*author).ok()?,
                label: label.clone(),
            }),
        })
        .collect();
    let replying_to = replying_to.and_then(|wanted| {
        items.iter().find_map(|item| match item {
            ChatItem::Message(message) if message.id == wanted => Some(message.clone()),
            ChatItem::Message(_) | ChatItem::Joined { .. } | ChatItem::MeetPointSet { .. } => None,
        })
    });
    let mut participants = meetup.people();
    participants.rotate_left(1);
    Ok(ChatView {
        spot_name: names.spot,
        participants,
        items,
        replying_to,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::common::Me;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn the_chat_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = chat(&snapshot, fixtures::meetup_id(), None, fixtures::clock())?;
        assert_eq!(view.spot_name.as_str(), "Riverside Park");
        let names: Vec<_> = view
            .participants
            .iter()
            .map(|person| (person.name.as_str(), person.me))
            .collect();
        assert_eq!(
            names,
            [("Ben", Me::Other), ("Chloé", Me::Other), ("Sam", Me::Me)]
        );
        let [joined, _, coffee, set, mine] = view.items.as_slice() else {
            return Err(FixtureError::Missing);
        };
        assert!(
            matches!(joined, ChatItem::Joined { person, at } if person.name.as_str() == "Ben" && (at.hour(), at.minute()) == (18, 40))
        );
        let ChatItem::Message(coffee) = coffee else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(coffee.body.as_str(), "Coffee after?");
        assert_eq!((coffee.at.hour(), coffee.at.minute()), (18, 45));
        let replies: Vec<_> = coffee
            .replies
            .iter()
            .map(|reply| {
                (
                    reply.author.name.as_str(),
                    reply.body.as_str(),
                    reply.at.minute(),
                )
            })
            .collect();
        assert_eq!(replies, [("Chloé", "Yes please", 46)]);
        assert!(
            matches!(set, ChatItem::MeetPointSet { author, label: Some(label) } if author.name.as_str() == "Ben" && label.as_str() == "the bandstand")
        );
        assert!(matches!(mine, ChatItem::Message(message) if message.author.me == Me::Me));
        assert_eq!(view.replying_to, None);
        Ok(())
    }

    #[test]
    fn the_composer_replies_in_the_chosen_thread() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = chat(
            &snapshot,
            fixtures::meetup_id(),
            Some(fixtures::coffee_message()),
            fixtures::clock(),
        )?;
        assert_eq!(
            view.replying_to.map(|message| message.id),
            Some(fixtures::coffee_message())
        );
        let gone = chat(
            &snapshot,
            fixtures::meetup_id(),
            Some(ChatMessageId::from_bytes([0; 16])),
            fixtures::clock(),
        )?;
        assert_eq!(gone.replying_to, None);
        Ok(())
    }

    #[test]
    fn entries_by_members_who_left_are_not_shown() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for meetup in &mut snapshot.meetups {
            meetup.others.retain(|peer| peer.leaf != fixtures::chloe());
        }
        let view = chat(&snapshot, fixtures::meetup_id(), None, fixtures::clock())?;
        assert_eq!(view.items.len(), 4);
        let coffee = view.items.iter().find_map(|item| match item {
            ChatItem::Message(message) if message.id == fixtures::coffee_message() => Some(message),
            ChatItem::Message(_) | ChatItem::Joined { .. } | ChatItem::MeetPointSet { .. } => None,
        });
        assert_eq!(coffee.map(|message| message.replies.len()), Some(0));

        for meetup in &mut snapshot.meetups {
            meetup.others.clear();
        }
        let view = chat(&snapshot, fixtures::meetup_id(), None, fixtures::clock())?;
        assert_eq!(view.items.len(), 1, "only Sam's own message is left");
        Ok(())
    }

    #[test]
    fn a_missing_meetup_or_party_is_an_error() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        assert_eq!(
            chat(
                &snapshot,
                GroupId::from_bytes([0; 32]),
                None,
                fixtures::clock()
            ),
            Err(ScreenError::UnknownMeetup)
        );
        snapshot.parties.clear();
        assert_eq!(
            chat(&snapshot, fixtures::meetup_id(), None, fixtures::clock()),
            Err(ScreenError::UnknownMeetup)
        );
        Ok(())
    }
}
