// SPDX-License-Identifier: AGPL-3.0-only
//! What a wake led to, for the app: the notifications pingers-screens finds between the
//! snapshot before a wake and the one after, worded by pingers-words, and the matching
//! events. Conversion only.

use pingers_screens::notifications::{Changes, Notification};

use crate::callbacks::Events;
use crate::ids::{MeetupId, PartyId};
use crate::views::LANGUAGE;

/// What a notification is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum NotificationKind {
    /// A Meetup started: open it.
    MeetupStarted,
    /// Someone opened an invite and waits to be let in.
    JoinRequest,
    /// A chat message from another member.
    ChatMessage,
    /// A member asks to see this phone's location.
    LocationRequest,
}

/// A notification to post (design B.7). The push arrived empty: the phone wrote this itself.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreNotification {
    /// The platform's notification id: a later notification with the same id replaces this
    /// one (the next chat message of the same Meetup).
    pub id: i32,
    /// What it is about.
    pub kind: NotificationKind,
    /// The first line.
    pub title: String,
    /// The text.
    pub body: String,
    /// The Meetup to open, for Meetup kinds.
    pub meetup: Option<MeetupId>,
    /// The Party to open, for a join request.
    pub party: Option<PartyId>,
}

impl From<&Notification> for CoreNotification {
    fn from(notification: &Notification) -> Self {
        let text = pingers_words::notifications::notification(LANGUAGE, notification);
        let (kind, meetup, party) = match notification {
            Notification::MeetupStarted(alert) => {
                (NotificationKind::MeetupStarted, Some(alert.meetup), None)
            }
            Notification::JoinRequest { party, .. } => {
                (NotificationKind::JoinRequest, None, Some(*party))
            }
            Notification::ChatMessage { meetup, .. } => {
                (NotificationKind::ChatMessage, Some(*meetup), None)
            }
            Notification::LocationRequest { meetup, .. } => {
                (NotificationKind::LocationRequest, Some(*meetup), None)
            }
            _ => (NotificationKind::MeetupStarted, None, None),
        };
        Self {
            id: i32::from_be_bytes(notification.key().stable_id().to_be_bytes()),
            kind,
            title: text.title,
            body: text.body,
            meetup: meetup.map(Into::into),
            party: party.map(Into::into),
        }
    }
}

/// Tells the app what a wake changed: each notification to post, and the events the screens
/// follow (a Meetup started, a message, a request, a Meetup ended, Parties changed).
pub(crate) fn tell(events: &dyn Events, changes: &Changes) {
    if changes.parties_changed {
        events.parties_changed();
    }
    for meetup in &changes.meetups_ended {
        events.meetup_ended((*meetup).into());
    }
    for notification in &changes.notifications {
        match notification {
            Notification::MeetupStarted(alert) => events.meetup_started(alert.meetup.into()),
            Notification::JoinRequest { party, .. } => events.join_requested((*party).into()),
            Notification::ChatMessage { meetup, .. } => events.meetup_message((*meetup).into()),
            Notification::LocationRequest {
                meetup,
                request,
                asker,
                ..
            } => events.location_request((*meetup).into(), (*asker).into(), (*request).into()),
            _ => {}
        }
        events.notify(notification.into());
    }
}
