// SPDX-License-Identifier: AGPL-3.0-only
//! A Meetup's chat, in words.

use pingers_screens::chat::{ChatItem, ChatMessageView, ChatView};

use crate::Language;
use crate::common::{avatar, list, person_in_sentence, person_on_its_own, plural, time};
use crate::meetup::meet_point_name;
use pingers_screens::meetup::MeetPointName;

/// A message, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageText {
    /// The author's avatar: "BE", "You".
    pub avatar: String,
    /// "Ben", "You".
    pub author: String,
    /// What it says.
    pub body: String,
    /// "18:45".
    pub time: String,
    /// "1 reply", "3 replies", when it has replies.
    pub reply_count: Option<String>,
    /// The replies.
    pub replies: Vec<MessageText>,
}

/// A chat item, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatItemText {
    /// "Ben joined · 18:40".
    Event(String),
    /// A message.
    Message(MessageText),
}

/// The chat screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatText {
    /// "Riverside Park".
    pub title: String,
    /// "Ben, Chloé and you · deleted when meetup ends".
    pub subtitle: String,
    /// The chat.
    pub items: Vec<ChatItemText>,
    /// "Replying in thread · Ben", while replying.
    pub replying: Option<String>,
    /// "Message", the composer's placeholder.
    pub placeholder: String,
    /// "Send".
    pub send: String,
    /// "Stop replying in thread".
    pub leave_thread: String,
}

fn message(language: Language, view: &ChatMessageView) -> MessageText {
    MessageText {
        avatar: avatar(language, &view.author),
        author: person_on_its_own(language, &view.author),
        body: view.body.as_str().to_owned(),
        time: time(language, view.at),
        reply_count: (!view.replies.is_empty())
            .then(|| plural(language, view.replies.len(), "reply", "replies")),
        replies: view
            .replies
            .iter()
            .map(|reply| message(language, reply))
            .collect(),
    }
}

/// The chat screen, in words.
#[must_use]
pub fn chat(language: Language, view: &ChatView) -> ChatText {
    let Language::English = language;
    let people: Vec<String> = view
        .participants
        .iter()
        .map(|person| person_in_sentence(language, person))
        .collect();
    ChatText {
        title: view.spot_name.as_str().to_owned(),
        subtitle: format!("{} · deleted when meetup ends", list(language, &people)),
        items: view
            .items
            .iter()
            .map(|item| match item {
                ChatItem::Joined { person, at } => ChatItemText::Event(format!(
                    "{} joined · {}",
                    person_on_its_own(language, person),
                    time(language, *at)
                )),
                ChatItem::Message(view) => ChatItemText::Message(message(language, view)),
                ChatItem::MeetPointSet { author, label } => {
                    let name = label
                        .clone()
                        .map_or(MeetPointName::Pin, MeetPointName::Label);
                    ChatItemText::Event(format!(
                        "{} set the meet point: {}",
                        person_on_its_own(language, author),
                        meet_point_name(language, &name)
                    ))
                }
            })
            .collect(),
        replying: view.replying_to.as_ref().map(|parent| {
            format!(
                "Replying in thread · {}",
                person_on_its_own(language, &parent.author)
            )
        }),
        placeholder: "Message".into(),
        send: "Send".into(),
        leave_thread: "Stop replying in thread".into(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::chat::chat as screen;
    use pingers_screens::fixtures::{self, FixtureError};

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn the_chat_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = chat(
            EN,
            &screen(&snapshot, fixtures::meetup_id(), None, fixtures::clock())?,
        );
        assert_eq!(text.title, "Riverside Park");
        assert_eq!(
            text.subtitle,
            "Ben, Chloé and you · deleted when meetup ends"
        );
        let [joined, _, coffee, set, mine] = text.items.as_slice() else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(joined, &ChatItemText::Event("Ben joined · 18:40".into()));
        let ChatItemText::Message(coffee) = coffee else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(
            (
                coffee.avatar.as_str(),
                coffee.author.as_str(),
                coffee.body.as_str(),
                coffee.time.as_str()
            ),
            ("BE", "Ben", "Coffee after?", "18:45")
        );
        assert_eq!(coffee.reply_count.as_deref(), Some("1 reply"));
        assert_eq!(
            coffee.replies.first().map(|reply| reply.author.as_str()),
            Some("Chloé")
        );
        assert_eq!(
            set,
            &ChatItemText::Event("Ben set the meet point: the bandstand".into())
        );
        let ChatItemText::Message(mine) = mine else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(
            (
                mine.author.as_str(),
                mine.avatar.as_str(),
                mine.reply_count.as_deref()
            ),
            ("You", "You", None)
        );
        assert_eq!(text.replying, None);
        assert_eq!(
            (text.placeholder.as_str(), text.send.as_str()),
            ("Message", "Send")
        );
        assert_eq!(text.leave_thread, "Stop replying in thread");
        Ok(())
    }

    #[test]
    fn replying_names_the_thread() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let text = chat(
            EN,
            &screen(
                &snapshot,
                fixtures::meetup_id(),
                Some(fixtures::coffee_message()),
                fixtures::clock(),
            )?,
        );
        assert_eq!(text.replying.as_deref(), Some("Replying in thread · Ben"));
        for meetup in &mut snapshot.meetups {
            for entry in &mut meetup.chat {
                if let pingers_screens::snapshot::ChatEntry::MeetPointSet { label, .. } = entry {
                    *label = None;
                }
            }
        }
        let text = chat(
            EN,
            &screen(&snapshot, fixtures::meetup_id(), None, fixtures::clock())?,
        );
        assert!(text.items.contains(&ChatItemText::Event(
            "Ben set the meet point: the pin".into()
        )));
        Ok(())
    }
}
