// SPDX-License-Identifier: AGPL-3.0-only
//! What a wake led to, for the app: the notifications pingers-screens finds between the
//! snapshot before a wake and the one after, worded by pingers-words, and the matching
//! events. Conversion only.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use pingers_client::Client;
use pingers_client::snapshot::ClientSnapshot;
use pingers_screens::notifications::{Changes, Notification, changes};

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

/// Where the latest published snapshot is read.
type SnapshotSource = Box<dyn Fn() -> Arc<ClientSnapshot> + Send + Sync>;

/// The snapshot the app was last told about, shared by wakes and the live Meetups, so a change
/// is notified once whichever reported it first. A live Meetup's message arrives on its channel
/// while the app is in the background and the server, seeing the channel, sends no push: the
/// phone notifies it itself, as it does for what a wake brings.
pub(crate) struct Notifier {
    events: Arc<dyn Events>,
    told: Mutex<Option<Arc<ClientSnapshot>>>,
    source: OnceLock<SnapshotSource>,
}

impl Notifier {
    pub(crate) fn new(events: Arc<dyn Events>) -> Arc<Self> {
        Arc::new(Self {
            events,
            told: Mutex::new(None),
            source: OnceLock::new(),
        })
    }

    /// Starts from the client's current snapshot: what it shows at launch is not news.
    pub(crate) fn follow(&self, client: &Client) {
        let snapshots = client.subscribe();
        let _set = self
            .source
            .set(Box::new(move || Arc::clone(&snapshots.borrow())));
        *self.lock() = Some(client.snapshot());
    }

    /// After a wake: the events and notifications for everything since the app was last told.
    pub(crate) fn wake_ended(&self, now: &Arc<ClientSnapshot>) {
        if let Some(changes) = self.since(now) {
            tell(self.events.as_ref(), &changes);
        }
    }

    /// A live Meetup raised an event (which pingers-client passes on itself): the notifications
    /// for what it brought.
    pub(crate) fn live_event(&self) {
        let Some(source) = self.source.get() else {
            return;
        };
        if let Some(changes) = self.since(&source()) {
            for notification in &changes.notifications {
                self.events.notify(notification.into());
            }
        }
    }

    /// What changed between the snapshot last told and `now`, which becomes the last told.
    fn since(&self, now: &Arc<ClientSnapshot>) -> Option<Changes> {
        let before = self.lock().replace(Arc::clone(now))?;
        Some(changes(&before, now))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<ClientSnapshot>>> {
        self.told.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::notifications::notifications;

    use super::*;

    #[test]
    fn each_kind_names_what_it_opens() -> Result<(), FixtureError> {
        let after = fixtures::snapshot()?;
        let mut before = after.clone();
        before.meetups.clear();
        before.location_requests.clear();
        for party in &mut before.parties {
            party.join_requests.clear();
        }
        let meetup = Some(MeetupId::from(fixtures::meetup_id()));
        let posted: Vec<(NotificationKind, Option<MeetupId>, bool)> =
            notifications(&before, &after)
                .iter()
                .map(CoreNotification::from)
                .map(|posted| (posted.kind, posted.meetup, posted.party.is_some()))
                .collect();
        assert_eq!(
            posted,
            [
                (NotificationKind::MeetupStarted, meetup.clone(), false),
                (NotificationKind::ChatMessage, meetup.clone(), false),
                (NotificationKind::ChatMessage, meetup.clone(), false),
                (NotificationKind::LocationRequest, meetup, false),
                (NotificationKind::JoinRequest, None, true),
            ]
        );
        Ok(())
    }
}
