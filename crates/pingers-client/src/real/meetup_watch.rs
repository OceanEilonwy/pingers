// SPDX-License-Identifier: AGPL-3.0-only
//! One task per live Meetup that turns what pingers-meetup-session raises into the client's
//! events, and keeps in memory what the snapshot shows beside the stored chat: who joined
//! when, who set the meet point, and the counts for the "Meetup ended" summary. When the
//! session's task ends on its own (alone too long, removed), the watcher reports the end.

use std::sync::{Arc, Mutex, PoisonError, Weak};

use pingers_meetup::MeetupEvent;
use pingers_meetup_session::{ChannelState, MeetupHandle, MeetupView};
use pingers_proto::{GroupId, LeafIndex, MeetPoint, MeetPointLabel, SeenSegment};
use pingers_time::{Clock, UnixSeconds};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::meetups::Shared;
use crate::Notice;
use crate::ports::ClientEvent;

/// A member who joined after this device, and when it saw them arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Arrival {
    pub(super) leaf: LeafIndex,
    pub(super) at: UnixSeconds,
}

/// Someone set the meet point, and when this device saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MeetPointMark {
    pub(super) author: LeafIndex,
    pub(super) label: Option<MeetPointLabel>,
    pub(super) at: UnixSeconds,
}

/// What a Meetup's watcher keeps; in memory only, gone with the Meetup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Timeline {
    /// Members who arrived while this device was in the Meetup.
    pub(super) arrivals: Vec<Arrival>,
    /// Meet points set while this device was in the Meetup.
    pub(super) meet_points: Vec<MeetPointMark>,
    /// Chat messages seen, this device's own included.
    pub(super) messages: usize,
    /// The most members at once, this device included.
    pub(super) people: usize,
}

impl Timeline {
    /// A timeline for a Meetup whose first view is `view`.
    pub(super) fn starting(view: &MeetupView) -> Self {
        Self {
            people: view.peers.len().saturating_add(1),
            ..Self::default()
        }
    }

    /// Takes in a new view: arrivals and the head count. Returns whether the members changed.
    fn view(&mut self, before: &[LeafIndex], view: &MeetupView, now: UnixSeconds) -> bool {
        let after = leaves(view);
        for leaf in after.iter().filter(|leaf| !before.contains(leaf)) {
            self.arrivals.push(Arrival {
                leaf: *leaf,
                at: now,
            });
        }
        self.people = self.people.max(after.len().saturating_add(1));
        after.as_slice() != before
    }

    /// Takes in one event, raised with `view` published, and says what the shell is told.
    fn event(&mut self, event: &MeetupEvent, view: &MeetupView, now: UnixSeconds) -> ClientEvent {
        let group = view.group;
        match event {
            MeetupEvent::Chat { .. } => {
                self.messages = self.messages.saturating_add(1);
                ClientEvent::MeetupMessage(group)
            }
            MeetupEvent::PeerLocation(location) => ClientEvent::PeerLocation {
                meetup: group,
                peer: location.leaf,
            },
            MeetupEvent::LocationRequested { from, request } => ClientEvent::LocationRequest {
                meetup: group,
                from: *from,
                request: *request,
            },
            MeetupEvent::LocationAccessChanged { peer, access } => {
                ClientEvent::LocationAccessChanged {
                    meetup: group,
                    peer: *peer,
                    access: *access,
                }
            }
            MeetupEvent::MeetPointChanged => {
                if let Some(author) = view.meet_point_set_by {
                    let label = match &view.meet_point {
                        Some(MeetPoint::Fixed { label, .. }) => label.clone(),
                        Some(MeetPoint::AtMember { .. } | MeetPoint::Cleared { .. }) | None => None,
                    };
                    self.meet_points.push(MeetPointMark {
                        author,
                        label,
                        at: now,
                    });
                }
                ClientEvent::MeetupUpdated(group)
            }
            MeetupEvent::BoardEquivocation => ClientEvent::Notice(Notice::BoardEquivocation),
            // The session wrote the allow-list; the Party's member rows show it.
            MeetupEvent::AlwaysAllowAdded(_) => ClientEvent::PartiesChanged,
            // A kind of event added later at least refreshes the Meetup's screens.
            _ => ClientEvent::MeetupUpdated(group),
        }
    }
}

/// The other members' leaves in `view`.
fn leaves(view: &MeetupView) -> Vec<LeafIndex> {
    view.peers.iter().map(|peer| peer.leaf).collect()
}

/// The watcher of one Meetup, owned by its entry in the live list and stopped with it.
pub(super) struct Watch(JoinHandle<()>);

impl Watch {
    /// Stops the watcher and waits until it has, so it holds no reference to the Meetup any
    /// more. Never called from the watcher itself.
    pub(super) async fn stop(mut self) {
        self.0.abort();
        let _stopped = (&mut self.0).await;
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What a watcher reads and writes.
pub(super) struct Watched {
    pub(super) group: GroupId,
    pub(super) events: mpsc::UnboundedReceiver<MeetupEvent>,
    pub(super) view: watch::Receiver<MeetupView>,
    pub(super) timeline: Arc<Mutex<Timeline>>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) sink: mpsc::UnboundedSender<ClientEvent>,
    /// The Meetup, to send it what needs the channel up.
    pub(super) handle: Weak<MeetupHandle>,
    /// The board segments this device saw, sent as its first message once the channel is up
    /// (design B.4.3, G.4); empty if it read no closed segment.
    pub(super) board_seen: Vec<SeenSegment>,
}

/// Starts watching a Meetup. `shared` is weak: the live list owns the watcher, not the other
/// way round, so dropping the client stops both.
pub(super) fn watch(watched: Watched, shared: Weak<Shared>) -> Watch {
    Watch(tokio::spawn(run(watched, shared)))
}

async fn run(watched: Watched, shared: Weak<Shared>) {
    let Watched {
        group,
        mut events,
        mut view,
        timeline,
        clock,
        sink,
        handle,
        mut board_seen,
    } = watched;
    let mut members = leaves(&view.borrow_and_update());
    let mut view_open = true;
    loop {
        tokio::select! {
            // Events first: each comes after the view that shows it, so a view change that
            // races it carries nothing the event needs.
            biased;
            event = events.recv() => {
                // The session's task ended: every event it sent has been read.
                let Some(event) = event else { break };
                let current = view.borrow().clone();
                let told = lock(&timeline).event(&event, &current, clock.now());
                let _sent = sink.send(told);
            }
            changed = view.changed(), if view_open => {
                if changed.is_err() {
                    view_open = false;
                    continue;
                }
                let current = view.borrow_and_update().clone();
                if current.channel == ChannelState::Connected && !board_seen.is_empty() {
                    send_board_seen(&handle, core::mem::take(&mut board_seen)).await;
                }
                let roster_changed = lock(&timeline).view(&members, &current, clock.now());
                if roster_changed {
                    members = leaves(&current);
                    let _sent = sink.send(ClientEvent::MeetupUpdated(group));
                }
            }
        }
    }
    if let Some(shared) = shared.upgrade() {
        shared.ended_on_its_own(group).await;
    }
}

/// Sends the board segments this device saw as its first message. Best effort: a Meetup
/// that ended meanwhile, or a frame refused, only loses this comparison.
async fn send_board_seen(handle: &Weak<MeetupHandle>, segments: Vec<SeenSegment>) {
    if let Some(handle) = handle.upgrade() {
        let _sent = handle.board_seen(segments).await;
    }
}

pub(super) fn lock(timeline: &Mutex<Timeline>) -> std::sync::MutexGuard<'_, Timeline> {
    timeline.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_crypto::AccountSigningKey;
    use pingers_meetup::{CadenceMode, ExactLocation, LocationVisibility, PeerLocation};
    use pingers_meetup_session::{ChannelState, PeerView};
    use pingers_proto::{E7Coordinate, Lamport, LocationRequestId, ProtoError};
    use rand_chacha::ChaCha20Rng;
    use rand_core::SeedableRng as _;

    use super::*;

    const GROUP: GroupId = GroupId::from_bytes([9; 32]);

    fn peer(leaf: u32) -> PeerView {
        PeerView {
            leaf: LeafIndex::new(leaf),
            signature_key: AccountSigningKey::generate(&mut ChaCha20Rng::seed_from_u64(u64::from(
                leaf,
            )))
            .public_key(),
            access: ExactLocation::Private,
            holds_own_key: false,
            request_pending_until: None,
            position: None,
        }
    }

    fn view(peers: &[u32], meet_point: Option<MeetPoint>, set_by: Option<u32>) -> MeetupView {
        MeetupView {
            group: GROUP,
            own_leaf: LeafIndex::new(0),
            cadence: CadenceMode::Background,
            visibility: LocationVisibility::AskEachTime,
            channel: ChannelState::Connected,
            meet_point,
            meet_point_set_by: set_by.map(LeafIndex::new),
            peers: peers.iter().copied().map(peer).collect(),
        }
    }

    #[test]
    fn arrivals_are_kept_and_the_head_count_is_the_peak() {
        let mut timeline = Timeline::starting(&view(&[1], None, None));
        assert_eq!(timeline.people, 2);
        let members = [LeafIndex::new(1)];
        assert!(timeline.view(&members, &view(&[1, 2], None, None), UnixSeconds::new(50)));
        assert_eq!(
            timeline.arrivals,
            vec![Arrival {
                leaf: LeafIndex::new(2),
                at: UnixSeconds::new(50)
            }]
        );
        assert_eq!(timeline.people, 3);
        // Someone leaving changes the members but neither arrives nor lowers the peak.
        let members = [LeafIndex::new(1), LeafIndex::new(2)];
        assert!(timeline.view(&members, &view(&[2], None, None), UnixSeconds::new(60)));
        assert_eq!(timeline.arrivals.len(), 1);
        assert_eq!(timeline.people, 3);
        // The same members again: nothing to tell.
        let members = [LeafIndex::new(2)];
        assert!(!timeline.view(&members, &view(&[2], None, None), UnixSeconds::new(70)));
    }

    #[test]
    fn each_meetup_event_tells_the_shell_what_changed() -> Result<(), ProtoError> {
        let mut timeline = Timeline::default();
        let plain = view(&[1], None, None);
        let now = UnixSeconds::new(10);
        let chat = MeetupEvent::Chat {
            id: pingers_proto::ChatMessageId::from_bytes([1; 16]),
            parent: None,
            sender: LeafIndex::new(1),
            body: pingers_proto::ChatBody::new("hi".to_owned())?,
            written_at: now,
        };
        assert_eq!(
            timeline.event(&chat, &plain, now),
            ClientEvent::MeetupMessage(GROUP)
        );
        assert_eq!(timeline.messages, 1);
        let position =
            pingers_geo::Coordinate::new(51.5, -0.1).map_err(|_| ProtoError::OutOfRange {
                field: "coordinate",
            })?;
        let moved = MeetupEvent::PeerLocation(PeerLocation {
            leaf: LeafIndex::new(1),
            position,
            accuracy: pingers_geo::Meters::new(5.0)
                .map_err(|_| ProtoError::OutOfRange { field: "accuracy" })?,
            heading: None,
            taken_at: now,
        });
        assert_eq!(
            timeline.event(&moved, &plain, now),
            ClientEvent::PeerLocation {
                meetup: GROUP,
                peer: LeafIndex::new(1)
            }
        );
        let request = LocationRequestId::from_bytes([4; 16]);
        let asked = MeetupEvent::LocationRequested {
            from: LeafIndex::new(1),
            request,
        };
        assert_eq!(
            timeline.event(&asked, &plain, now),
            ClientEvent::LocationRequest {
                meetup: GROUP,
                from: LeafIndex::new(1),
                request
            }
        );
        let access = ExactLocation::Shared { expires: None };
        let granted = MeetupEvent::LocationAccessChanged {
            peer: LeafIndex::new(1),
            access,
        };
        assert_eq!(
            timeline.event(&granted, &plain, now),
            ClientEvent::LocationAccessChanged {
                meetup: GROUP,
                peer: LeafIndex::new(1),
                access
            }
        );
        assert_eq!(
            timeline.event(&MeetupEvent::BoardEquivocation, &plain, now),
            ClientEvent::Notice(Notice::BoardEquivocation)
        );
        assert_eq!(
            timeline.event(
                &MeetupEvent::AlwaysAllowAdded(LeafIndex::new(1)),
                &plain,
                now
            ),
            ClientEvent::PartiesChanged
        );
        assert!(timeline.meet_points.is_empty());
        Ok(())
    }

    #[test]
    fn a_meet_point_is_marked_with_its_author_and_label() -> Result<(), ProtoError> {
        let mut timeline = Timeline::default();
        let label = MeetPointLabel::new("The fountain".to_owned())?;
        let fixed = MeetPoint::Fixed {
            position: E7Coordinate::new(515_000_000, -1_000_000)?,
            label: Some(label.clone()),
            lamport: Lamport::new(1),
        };
        let now = UnixSeconds::new(20);
        assert_eq!(
            timeline.event(
                &MeetupEvent::MeetPointChanged,
                &view(&[1], Some(fixed), Some(1)),
                now
            ),
            ClientEvent::MeetupUpdated(GROUP)
        );
        let at_member = MeetPoint::AtMember {
            leaf: LeafIndex::new(1),
            lamport: Lamport::new(2),
        };
        let _told = timeline.event(
            &MeetupEvent::MeetPointChanged,
            &view(&[1], Some(at_member), Some(1)),
            now,
        );
        // Cleared: nobody set anything to show.
        let _told = timeline.event(&MeetupEvent::MeetPointChanged, &view(&[1], None, None), now);
        assert_eq!(
            timeline.meet_points,
            vec![
                MeetPointMark {
                    author: LeafIndex::new(1),
                    label: Some(label),
                    at: now
                },
                MeetPointMark {
                    author: LeafIndex::new(1),
                    label: None,
                    at: now
                }
            ]
        );
        Ok(())
    }
}
