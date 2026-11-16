// SPDX-License-Identifier: AGPL-3.0-only
//! What the live Meetups raise on their own (a chat message, a peer's position, a location
//! request, a member joining or leaving, the end), passed to the shell: the snapshot is
//! republished first, so the screen the event refreshes reads what it announces, then each
//! [`Events`] call is made. Events that arrive together are passed on together, once each.

use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::Events;
use crate::client::assemble;
use crate::ports::{ClientEvent, Crates};
use crate::snapshot::ClientSnapshot;

/// The task that passes Meetup events on, owned by the client and stopped with it.
pub(crate) struct Forwarder(JoinHandle<()>);

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What the forwarding task needs.
pub(crate) struct Forwarding {
    /// The events the Meetups raise.
    pub(crate) incoming: mpsc::UnboundedReceiver<ClientEvent>,
    /// The crates whose sections make the snapshot.
    pub(crate) crates: Crates,
    /// Where the snapshot is published.
    pub(crate) snapshot: watch::Sender<Arc<ClientSnapshot>>,
    /// The shell's sink.
    pub(crate) events: Arc<dyn Events>,
}

/// Starts passing Meetup events on until the Meetups stop raising them.
pub(crate) fn forward(forwarding: Forwarding) -> Forwarder {
    let Forwarding {
        mut incoming,
        crates,
        snapshot,
        events,
    } = forwarding;
    Forwarder(tokio::spawn(async move {
        while let Some(first) = incoming.recv().await {
            let mut batch = vec![first];
            while let Ok(more) = incoming.try_recv() {
                if !batch.contains(&more) {
                    batch.push(more);
                }
            }
            // A section that cannot be read keeps the previous snapshot published, as for a
            // command; the shell is still told, and reads what is there.
            if let Ok(next) = assemble(&crates).await {
                snapshot.send_replace(Arc::new(next));
            }
            for event in batch {
                emit(events.as_ref(), event);
            }
        }
    }))
}

/// Makes the [`Events`] call `event` stands for.
pub(crate) fn emit(events: &dyn Events, event: ClientEvent) {
    match event {
        ClientEvent::PartiesChanged => events.parties_changed(),
        ClientEvent::JoinRequested(party) => events.join_requested(party),
        ClientEvent::LetIn(party) => events.let_in(party),
        ClientEvent::MeetupStarted(meetup) => events.meetup_started(meetup),
        ClientEvent::MeetupUpdated(meetup) => events.meetup_updated(meetup),
        ClientEvent::MeetupMessage(meetup) => events.meetup_message(meetup),
        ClientEvent::PeerLocation { meetup, peer } => events.peer_location(meetup, peer),
        ClientEvent::LocationRequest {
            meetup,
            from,
            request,
        } => events.location_request(meetup, from, request),
        ClientEvent::LocationAccessChanged {
            meetup,
            peer,
            access,
        } => events.location_access_changed(meetup, peer, access),
        ClientEvent::MeetupEnded(meetup) => events.meetup_ended(meetup),
        ClientEvent::Notice(notice) => events.notice(notice),
        ClientEvent::PrivacyStatusChanged => events.privacy_status_changed(),
    }
}
