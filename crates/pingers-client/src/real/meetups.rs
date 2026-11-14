// SPDX-License-Identifier: AGPL-3.0-only
//! [`MeetupsPort`] and pingers-wake's `MeetupPort` over pingers-meetup-session's handles.
//!
//! The live Meetups are the handles this process holds; pingers-meetup-session cannot resume
//! a Meetup, so `discard_stale` runs once, on the first call (the first snapshot at launch).
//!
//! - `admit` creates a Meetup (or adds to the live one) under the `PSK_meet` pingers-wake
//!   derived from the Spot keys of the record the knock answered (design B.5), with this
//!   device's own Meetup subscription when wake-up pushes are on, and posts the Welcome.
//! - `knock_key_package` makes the knock's Meetup KeyPackage and subscription and keeps the
//!   grant secret, with the subscription, in the store under the knock's reply mailbox;
//!   `welcome` takes them back when the Welcome arrives there and joins with the same key.
//!   Two Meetups at one Spot (two occupants who had not met each created one with this
//!   device): the one with the smaller group id is kept and the other left (design B.5).
//! - Every live Meetup has a watcher (`meetup_watch`) that turns its events into
//!   [`ClientEvent`]s for the client, which republishes the snapshot and tells the shell. A
//!   Meetup that ends on its own, or is left, is reported with its summary ("Meetup ended").
//! - The location mode follows the Meetups: a live Meetup runs the location foreground
//!   service (1 Hz with the compass while the app is on screen); after the last one the
//!   device is back to dwelling, and the next wake decides from there.
//!
//! The `Runtime` takes pingers-sync's group calls and live channel
//! ([`GroupLogs`](super::meetup_network::GroupLogs)); the Welcome is posted through
//! pingers-sync.

use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use pingers_geo::Fix;
use pingers_meetup::{
    ExactLocation, LocationAnswer, MeetPointDraft, MeetupAction, MeetupEvent, TargetPosition,
    coordinate_from_wire,
};
use pingers_meetup_session::{
    AddMember, CreateMeetup, JoinMeetup, Knocking, MeetupHandle, MeetupScreen, MeetupView, Runtime,
    create, discard_stale, join, knock_key_package,
};
use pingers_mls::CipherSuitePolicy;
use pingers_parties::{PartyLifecycle, PostQuantum};
use pingers_presence::{Admission, LEASE_DURATION, MeetupKey, SpotRef};
use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, MeetPoint, Padded,
    PartyId, SeenSegment, Subscription, TimeToLive, WelcomeBytes,
};
use pingers_store::{KnockKey, Session};
use pingers_time::{Clock, UnixSeconds};
use pingers_wake::PortError;
use pingers_wake::ports::{KnockKeyPackage, MeetupAdmission, MeetupPort, ReplyPost};
use tokio::sync::{OnceCell, mpsc};

use super::Backend;
use super::meetup_network::GroupLogs;
use super::meetup_watch::{self, Timeline, Watch, Watched};
use super::records::corrupt;
use super::sync::Net;
use crate::ports::{ClientEvent, MeetupsPort, MeetupsSection};
use crate::snapshot::{
    ChatEntry, ChatMessage, ChatReply, EndedMeetup, LocationRequest, MeetPlace, MeetPointSnapshot,
    Meetup, MeetupPeer, MeetupSelf, Name, PeerAccess,
};
use crate::{ClientError, LocationMode, Platform, Unavailable};

/// How long a Meetup subscription is registered for: far longer than a Meetup lasts (the
/// server drops an idle Meetup's log after two hours); the Meetup's end deletes it.
const MEETUP_SUBSCRIPTION: Duration = Duration::from_hours(24);

/// A live Meetup.
struct Live {
    handle: Arc<MeetupHandle>,
    spot: SpotRef,
    started_at: UnixSeconds,
    timeline: Arc<Mutex<Timeline>>,
    /// Stops the watcher when the entry goes.
    watch: Watch,
}

/// What the port and the watchers share.
pub(super) struct Shared {
    backend: Arc<Backend>,
    net: Arc<Net>,
    platform: Arc<dyn Platform>,
    live: Mutex<Vec<Live>>,
    /// The app on screen or not, for the cadence and location mode of new Meetups.
    screen: Mutex<MeetupScreen>,
    /// The Meetup the co-presence alert is for: the latest one started, while it lasts.
    alert: Mutex<Option<GroupId>>,
    /// The last Meetup that ended, for the "Meetup ended" screen.
    ended: Mutex<Option<EndedMeetup>>,
    events: mpsc::UnboundedSender<ClientEvent>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What is needed to start watching a Meetup.
struct Started {
    handle: MeetupHandle,
    events: mpsc::UnboundedReceiver<MeetupEvent>,
    spot: SpotRef,
}

/// What is kept of a Meetup for its summary once it ends.
struct Ending {
    group: GroupId,
    spot: SpotRef,
    started_at: UnixSeconds,
    timeline: Timeline,
}

/// A Meetup taken off the live list.
struct Taken {
    handle: Arc<MeetupHandle>,
    ending: Ending,
    watch: Watch,
}

impl Live {
    /// Takes it apart, keeping what the summary needs.
    fn taken(self) -> Taken {
        let Self {
            handle,
            spot,
            started_at,
            timeline,
            watch,
        } = self;
        let ending = Ending {
            group: handle.group(),
            spot,
            started_at,
            timeline: meetup_watch::lock(&timeline).clone(),
        };
        Taken {
            handle,
            ending,
            watch,
        }
    }
}

impl Shared {
    fn handles(&self) -> Vec<Arc<MeetupHandle>> {
        lock(&self.live)
            .iter()
            .map(|live| Arc::clone(&live.handle))
            .collect()
    }

    fn take(&self, meetup: GroupId) -> Option<Live> {
        let mut live = lock(&self.live);
        let index = live.iter().position(|live| live.handle.group() == meetup)?;
        Some(live.swap_remove(index))
    }

    fn tell(&self, event: ClientEvent) {
        let _sent = self.events.send(event);
    }

    /// The location mode for live Meetups on this screen state.
    fn meetup_mode(&self) -> LocationMode {
        match *lock(&self.screen) {
            MeetupScreen::Open => LocationMode::MeetupForeground,
            MeetupScreen::Closed => LocationMode::MeetupBackground,
        }
    }

    /// Keeps a new Meetup live and starts watching it.
    async fn start(self: &Arc<Self>, started: Started) {
        let Started {
            handle,
            events,
            spot,
        } = started;
        let group = handle.group();
        let screen = *lock(&self.screen);
        if screen == MeetupScreen::Open {
            // A Meetup starts at the background cadence.
            let _set = handle.screen(screen).await;
        }
        let view = handle.view();
        let timeline = Arc::new(Mutex::new(Timeline::starting(&view.borrow())));
        let watch = meetup_watch::watch(
            Watched {
                group,
                events,
                view,
                timeline: Arc::clone(&timeline),
                clock: Arc::clone(&self.backend.outside.clock),
                sink: self.events.clone(),
            },
            Arc::downgrade(self),
        );
        lock(&self.live).push(Live {
            handle: Arc::new(handle),
            spot,
            started_at: self.backend.outside.clock.now(),
            timeline,
            watch,
        });
        *lock(&self.alert) = Some(group);
        self.platform.set_location_mode(self.meetup_mode());
        self.tell(ClientEvent::MeetupStarted(group));
        self.keep_one_per_spot(spot).await;
    }

    /// Two Meetups at one Spot: keeps the smaller group id and leaves the other (design B.5).
    async fn keep_one_per_spot(&self, spot: SpotRef) {
        let mut groups: Vec<GroupId> = lock(&self.live)
            .iter()
            .filter(|live| live.spot == spot)
            .map(|live| live.handle.group())
            .collect();
        groups.sort_unstable();
        for extra in groups.into_iter().skip(1) {
            if let Some(live) = self.take(extra) {
                let _left = self.leave(live).await;
            }
        }
    }

    /// Leaves a Meetup (the user, the exit, the gate, a duplicate) and reports its end.
    async fn leave(&self, live: Live) -> Result<(), ClientError> {
        let Taken {
            handle,
            ending,
            watch,
        } = live.taken();
        // The end is reported here, not by the watcher.
        drop(watch);
        let left = match Arc::try_unwrap(handle) {
            Ok(handle) => handle.leave().await.map(drop).map_err(ClientError::from),
            // Borrowed elsewhere for a moment: dropping the last reference stops its task,
            // and its state is discarded at the next launch.
            Err(_borrowed) => Ok(()),
        };
        self.ended(ending).await;
        left
    }

    /// A Meetup's task ended without being left here: alone too long, or removed. Called by
    /// its watcher, which stops with the entry.
    pub(super) async fn ended_on_its_own(&self, group: GroupId) {
        if let Some(live) = self.take(group) {
            let Taken {
                handle,
                ending,
                watch,
            } = live.taken();
            drop(handle);
            self.ended(ending).await;
            // The calling watcher is the one stopped here; it awaits nothing more.
            drop(watch);
        }
    }

    /// Records the summary, clears the alert, settles the location mode and tells the shell.
    async fn ended(&self, ending: Ending) {
        let Ending {
            group,
            spot,
            started_at,
            timeline,
        } = ending;
        let spot_name = self
            .backend
            .store(move |session, clock, _rng| {
                let spots = PartyLifecycle::new(session, clock).spots(&spot.party)?;
                let spot = spots
                    .into_iter()
                    .find(|candidate| candidate.id == spot.spot)
                    .ok_or_else(corrupt)?;
                Name::new(spot.name.as_str()).map_err(|_invalid| corrupt())
            })
            .await;
        // A Spot deleted meanwhile has no name to show: no summary then.
        *lock(&self.ended) = spot_name.ok().map(|spot_name| EndedMeetup {
            spot_name,
            started_at,
            ended_at: self.backend.outside.clock.now(),
            people: timeline.people,
            messages: timeline.messages,
        });
        {
            let mut alert = lock(&self.alert);
            if *alert == Some(group) {
                *alert = None;
            }
        }
        if lock(&self.live).is_empty() {
            self.platform.set_location_mode(LocationMode::Dwell);
        }
        self.tell(ClientEvent::MeetupEnded(group));
    }
}

/// The live Meetups of this process.
pub(crate) struct Meetups {
    shared: Arc<Shared>,
    discarded: OnceCell<usize>,
    events: Mutex<Option<mpsc::UnboundedReceiver<ClientEvent>>>,
}

impl Meetups {
    pub(crate) fn new(backend: Arc<Backend>, net: Arc<Net>, platform: Arc<dyn Platform>) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            shared: Arc::new(Shared {
                backend,
                net,
                platform,
                live: Mutex::new(Vec::new()),
                screen: Mutex::new(MeetupScreen::Closed),
                alert: Mutex::new(None),
                ended: Mutex::new(None),
                events: sender,
            }),
            discarded: OnceCell::new(),
            events: Mutex::new(Some(receiver)),
        }
    }

    fn backend(&self) -> &Backend {
        &self.shared.backend
    }

    /// Deletes what a killed process left, once.
    async fn launched(&self) -> Result<(), ClientError> {
        self.discarded
            .get_or_try_init(|| discard_stale(&self.backend().database))
            .await?;
        Ok(())
    }

    fn handle(&self, meetup: GroupId) -> Result<Arc<MeetupHandle>, ClientError> {
        self.shared
            .handles()
            .into_iter()
            .find(|handle| handle.group() == meetup)
            .ok_or_else(|| Unavailable::NoMeetup.into())
    }

    async fn act(&self, meetup: GroupId, action: MeetupAction) -> Result<(), ClientError> {
        self.launched().await?;
        Ok(self.handle(meetup)?.act(action).await?)
    }

    /// The Meetup screen opened or closed, or the app came on screen or left it. Best effort
    /// per Meetup: one that has just ended is past caring, and its watcher reports the end.
    pub(crate) async fn screen(&self, screen: MeetupScreen) {
        *lock(&self.shared.screen) = screen;
        let handles = self.shared.handles();
        for handle in &handles {
            let _set = handle.screen(screen).await;
        }
        if !handles.is_empty() {
            self.shared
                .platform
                .set_location_mode(self.shared.meetup_mode());
        }
    }

    /// A new fix of this device, for every live Meetup. Best effort per Meetup, as for
    /// [`Meetups::screen`]: a fix one Meetup cannot use (an accuracy beyond the frame's range)
    /// must not stop the wake that delivered it.
    pub(crate) async fn own_fix(&self, fix: Fix) {
        for handle in self.shared.handles() {
            let _taken = handle.own_fix(fix).await;
        }
    }

    /// What every Meetup shares. `None` without a configured instance.
    fn runtime(&self) -> Option<Runtime> {
        let backend = self.backend();
        backend.transport.as_ref()?;
        Some(Runtime {
            database: Arc::clone(&backend.database),
            network: Arc::new(GroupLogs(Arc::clone(&self.shared.net))),
            clock: Arc::clone(&backend.outside.clock),
            timer: Arc::clone(&backend.outside.timer),
        })
    }

    /// This device's subscription for a Meetup, sealed to the push relay; `None` with wake-up
    /// pushes off, or without a push token.
    async fn meetup_subscription(&self) -> Option<Subscription> {
        let net = &self.shared.net;
        net.ensure_wake_push().await.ok()?;
        let lifetime = TimeToLive::new(MEETUP_SUBSCRIPTION).ok()?;
        net.engine().ok()?.knock_subscription(lifetime).ok()?
    }

    /// Records `subscription` as this device's in `group`, so its pushes are recognised.
    async fn remember(&self, group: GroupId, subscription: Option<&Subscription>) {
        if let (Some(subscription), Ok(engine)) = (subscription, self.shared.net.engine()) {
            // Best effort: without the record a push for the group reads as unknown, and the
            // live channel carries the Meetup anyway.
            let _remembered = engine.remember_subscription(&group, subscription).await;
        }
    }

    /// The closed board segments this device read (`board_seen`), for a Meetup's first
    /// message. Best effort: none read, or none readable, sends nothing to compare.
    async fn board_seen(&self) -> Vec<SeenSegment> {
        self.backend()
            .store(|session, _clock, _rng| Ok(session.board_seen().list()?))
            .await
            .unwrap_or_default()
    }

    /// The Party's ciphersuite.
    async fn suite(&self, party: PartyId) -> Result<CipherSuitePolicy, ClientError> {
        self.backend()
            .store(move |session, clock, _rng| {
                let settings = PartyLifecycle::new(session, clock).settings(&party)?;
                Ok(match settings.post_quantum {
                    PostQuantum::Classical => CipherSuitePolicy::Classical,
                    PostQuantum::QuantumResistant => CipherSuitePolicy::QuantumResistant,
                })
            })
            .await
    }

    /// Creates a Meetup at the key's Spot with the knocker and keeps it live; returns the
    /// Welcome for the knocker.
    async fn create(&self, key: MeetupKey, knocker: AddMember) -> Result<WelcomeBytes, PortError> {
        let runtime = self.runtime().ok_or(PortError::Unavailable)?;
        let spot = key.id().spot;
        let suite = self
            .suite(spot.party)
            .await
            .map_err(|_failed| PortError::Unavailable)?;
        let group = self
            .backend()
            .store(|_session, _clock, rng| {
                let mut group = [0_u8; GroupId::LENGTH];
                rng.fill_bytes(&mut group);
                Ok(GroupId::from_bytes(group))
            })
            .await
            .map_err(|_failed| PortError::Unavailable)?;
        let own = self.meetup_subscription().await;
        let created = create(
            &runtime,
            CreateMeetup {
                key,
                group,
                knocker: knocker.key_package,
                subscriptions: own.iter().cloned().chain(knocker.subscription).collect(),
                own_subscription: own.as_ref().map(|subscription| subscription.id),
                board_seen: self.board_seen().await,
                suite,
            },
            (self.backend().outside.rng)(),
        )
        .await
        .map_err(|_failed| PortError::Unavailable)?;
        self.remember(group, own.as_ref()).await;
        self.shared
            .start(Started {
                handle: created.handle,
                events: created.events,
                spot,
            })
            .await;
        Ok(created.welcome)
    }
}

#[async_trait]
impl MeetupsPort for Meetups {
    async fn send_chat(
        &self,
        meetup: GroupId,
        body: ChatBody,
        parent: Option<ChatMessageId>,
    ) -> Result<ChatMessageId, ClientError> {
        let own_leaf = self.handle(meetup)?.view().borrow().own_leaf;
        self.act(meetup, MeetupAction::SendChat { body, parent })
            .await?;
        // pingers-meetup chooses the id; the message is in the store once `act` returns.
        self.backend()
            .store(move |session, _clock, _rng| {
                session
                    .meetup_messages()
                    .list(&meetup)?
                    .into_iter()
                    .rev()
                    .find(|message| message.sender == own_leaf)
                    .map(|message| message.id)
                    .ok_or_else(corrupt)
            })
            .await
    }

    async fn set_meet_point(
        &self,
        meetup: GroupId,
        point: MeetPointDraft,
    ) -> Result<(), ClientError> {
        self.act(meetup, MeetupAction::SetMeetPoint(point)).await
    }

    async fn ask_location(
        &self,
        meetup: GroupId,
        peer: LeafIndex,
    ) -> Result<LocationRequestId, ClientError> {
        let handle = self.handle(meetup)?;
        self.act(meetup, MeetupAction::AskLocation(peer)).await?;
        // pingers-meetup chooses the id; the view shows the pending ask once `act` returns.
        let view = handle.view();
        let pending = view
            .borrow()
            .peer(peer)
            .and_then(|shown| shown.request_pending);
        pending
            .map(|ask| ask.id)
            .ok_or_else(|| ClientError::Internal)
    }

    async fn answer_location_request(
        &self,
        meetup: GroupId,
        request: LocationRequestId,
        answer: LocationAnswer,
    ) -> Result<(), ClientError> {
        self.act(meetup, MeetupAction::AnswerRequest { request, answer })
            .await
    }

    async fn revoke_location(&self, meetup: GroupId, peer: LeafIndex) -> Result<(), ClientError> {
        self.act(meetup, MeetupAction::RevokeLocation(peer)).await
    }

    async fn leave_meetup(&self, meetup: GroupId) -> Result<(), ClientError> {
        self.launched().await?;
        let live = self.shared.take(meetup).ok_or(Unavailable::NoMeetup)?;
        self.shared.leave(live).await
    }

    async fn section(&self) -> Result<MeetupsSection, ClientError> {
        self.launched().await?;
        let lives: Vec<Shown> = lock(&self.shared.live)
            .iter()
            .map(|live| Shown {
                view: live.handle.view().borrow().clone(),
                timeline: meetup_watch::lock(&live.timeline).clone(),
            })
            .collect();
        let location_requests = lives
            .iter()
            .flat_map(|shown| incoming_requests(&shown.view))
            .collect();
        let meetups = self
            .backend()
            .store(move |session, clock, _rng| {
                lives
                    .iter()
                    .map(|shown| meetup(session, clock, shown))
                    .collect::<Result<Vec<_>, _>>()
            })
            .await?;
        Ok(MeetupsSection {
            meetups,
            alert: *lock(&self.shared.alert),
            location_requests,
            ended_meetup: lock(&self.shared.ended).clone(),
        })
    }

    fn take_events(&self) -> Option<mpsc::UnboundedReceiver<ClientEvent>> {
        lock(&self.events).take()
    }
}

/// A live Meetup as read for the snapshot.
struct Shown {
    view: MeetupView,
    timeline: Timeline,
}

/// One live Meetup as the snapshot shows it: the session's view, names from the Party roster,
/// chat from the store with arrivals and meet points from the watcher.
fn meetup(session: &Session, clock: &dyn Clock, shown: &Shown) -> Result<Meetup, ClientError> {
    let Shown { view, timeline } = shown;
    let row = session
        .meetups()
        .get(&view.group)?
        .ok_or(Unavailable::NoMeetup)?;
    let roster = PartyLifecycle::new(session, clock).roster(&row.party)?;
    let name_of = |key| -> Result<Name, ClientError> {
        let name = roster
            .iter()
            .find(|member| &member.signature_key == key)
            .and_then(|member| member.name.as_ref())
            .ok_or_else(corrupt)?;
        Name::new(name.as_str()).map_err(|_invalid| corrupt())
    };
    let own_key = session
        .identity()
        .load()?
        .ok_or_else(corrupt)?
        .signing_key
        .public_key();
    let others = view
        .peers
        .iter()
        .map(|peer| {
            let access = match (peer.position, peer.access, peer.request_pending) {
                (Some(position), ExactLocation::Shared { .. }, _) => {
                    PeerAccess::Shared(TargetPosition {
                        position: position.position,
                        taken_at: position.taken_at,
                    })
                }
                (_, _, Some(_)) => PeerAccess::Asked,
                _ => PeerAccess::Private,
            };
            Ok(MeetupPeer {
                leaf: peer.leaf,
                name: name_of(&peer.signature_key)?,
                access,
            })
        })
        .collect::<Result<Vec<_>, ClientError>>()?;
    // Who holds this device's current location key, as the session knows it.
    let sharing_with = view
        .peers
        .iter()
        .filter(|peer| peer.holds_own_key)
        .map(|peer| peer.leaf)
        .collect();
    Ok(Meetup {
        id: view.group,
        party: row.party,
        spot: row.spot,
        started_at: row.started_at,
        me: MeetupSelf {
            leaf: view.own_leaf,
            name: name_of(&own_key)?,
        },
        others,
        meet_point: meet_point(view),
        chat: chat(session, view.group, timeline)?,
        sharing_with,
    })
}

/// The meet point and who set it. A fixed point whose position does not read as a
/// coordinate is not shown.
fn meet_point(view: &MeetupView) -> Option<MeetPointSnapshot> {
    let set_by = view.meet_point_set_by?;
    let place = match view.meet_point.as_ref()? {
        MeetPoint::Fixed {
            position, label, ..
        } => MeetPlace::Fixed {
            position: coordinate_from_wire(*position).ok()?,
            label: label.clone(),
        },
        MeetPoint::AtMember { leaf, .. } => MeetPlace::AtMember(*leaf),
        MeetPoint::Cleared { .. } => return None,
    };
    Some(MeetPointSnapshot { place, set_by })
}

/// One chat entry and when it happened, for ordering.
struct Dated {
    at: UnixSeconds,
    entry: ChatEntry,
}

/// The chat in time order: stored messages with their replies under them, members who
/// arrived, and meet points set.
fn chat(
    session: &Session,
    meetup: GroupId,
    timeline: &Timeline,
) -> Result<Vec<ChatEntry>, ClientError> {
    let stored = session.meetup_messages().list(&meetup)?;
    let messages = stored
        .iter()
        .filter(|message| message.parent.is_none())
        .map(|message| Dated {
            at: message.written_at,
            entry: ChatEntry::Message(ChatMessage {
                id: message.id,
                author: message.sender,
                body: message.body.clone(),
                at: message.written_at,
                replies: stored
                    .iter()
                    .filter(|reply| reply.parent == Some(message.id))
                    .map(|reply| ChatReply {
                        id: reply.id,
                        author: reply.sender,
                        body: reply.body.clone(),
                        at: reply.written_at,
                    })
                    .collect(),
            }),
        });
    let arrivals = timeline.arrivals.iter().map(|arrival| Dated {
        at: arrival.at,
        entry: ChatEntry::Joined {
            leaf: arrival.leaf,
            at: arrival.at,
        },
    });
    let meet_points = timeline.meet_points.iter().map(|mark| Dated {
        at: mark.at,
        entry: ChatEntry::MeetPointSet {
            author: mark.author,
            label: mark.label.clone(),
        },
    });
    let mut entries: Vec<Dated> = messages.chain(arrivals).chain(meet_points).collect();
    // Stable: within one second, messages come before the notes about members and places.
    entries.sort_by_key(|dated| dated.at);
    Ok(entries.into_iter().map(|dated| dated.entry).collect())
}

/// Requests from other members waiting for an answer, as the session holds them.
fn incoming_requests(view: &MeetupView) -> Vec<LocationRequest> {
    view.requests
        .iter()
        .map(|request| LocationRequest {
            id: request.id,
            meetup: view.group,
            from: request.from,
            expires_at: request.expires,
        })
        .collect()
}

impl MeetupPort for Meetups {
    fn current_meetup(&self, spot: SpotRef) -> Option<GroupId> {
        lock(&self.shared.live)
            .iter()
            .find(|live| live.spot == spot)
            .map(|live| live.handle.group())
    }

    /// A fresh Meetup KeyPackage and subscription; the grant secret and the subscription wait
    /// in the store under the reply mailbox, and knocks too old to be answered are forgotten.
    async fn knock_key_package(
        &self,
        party: PartyId,
        reply_mailbox: MailboxId,
    ) -> Result<KnockKeyPackage, PortError> {
        let suite = self
            .suite(party)
            .await
            .map_err(|_failed| PortError::Unavailable)?;
        let subscription = self.meetup_subscription().await;
        let made = knock_key_package(
            &self.backend().database,
            Knocking {
                suite,
                subscription: subscription.as_ref().map(|subscription| subscription.id),
            },
            (self.backend().outside.rng)(),
        )
        .await
        .map_err(|_failed| PortError::Unavailable)?;
        let kept = KnockKey {
            reply_mailbox,
            party,
            grant_key: made.grant_key,
            subscription: subscription.clone(),
            created_at: self.backend().outside.clock.now(),
        };
        self.backend()
            .store(move |session, _clock, _rng| {
                let cutoff = kept
                    .created_at
                    .checked_sub(LEASE_DURATION)
                    .unwrap_or(UnixSeconds::new(0));
                session.knock_keys().delete_before(cutoff)?;
                session.knock_keys().put(&kept)?;
                Ok(())
            })
            .await
            .map_err(|_failed| PortError::Unavailable)?;
        Ok(KnockKeyPackage {
            key_package: made.key_package,
            subscription,
        })
    }

    /// Creates the Meetup with the knocker, or adds the knocker to the live one, under the
    /// `PSK_meet` pingers-wake derived from the Spot keys, then posts the Welcome.
    async fn admit(&self, admission: MeetupAdmission) -> Result<(), PortError> {
        let MeetupAdmission {
            key,
            admission,
            knock,
        } = admission;
        let reply_mailbox = knock.knock.reply_mailbox;
        let newcomer = AddMember {
            key_package: knock.knock.key_package,
            subscription: knock.knock.meetup_subscription,
        };
        let welcome = match admission {
            Admission::CreateMeetup => self.create(key, newcomer).await?,
            Admission::AddToMeetup(group) => self
                .handle(group)
                .map_err(|_none| PortError::Unavailable)?
                .add_member(newcomer)
                .await
                .map_err(|_failed| PortError::Unavailable)?,
        };
        self.shared.net.post_welcome(reply_mailbox, welcome).await
    }

    /// Joins from the Welcome with the grant secret kept for the knock answered in the reply
    /// mailbox, then forgets it. A post that is not a Welcome for that knock leaves it kept.
    async fn welcome(&self, reply: ReplyPost) -> Result<(), PortError> {
        let ReplyPost {
            key,
            reply_mailbox,
            post,
        } = reply;
        let runtime = self.runtime().ok_or(PortError::Unavailable)?;
        let welcome = Padded::from_wire(post)
            .and_then(|padded| padded.payload().map(<[u8]>::to_vec))
            .map(WelcomeBytes::from_bytes)
            .map_err(|_not_padded| PortError::Unavailable)?;
        let kept = self
            .backend()
            .store(move |session, _clock, _rng| {
                session
                    .knock_keys()
                    .get(&reply_mailbox)?
                    .ok_or_else(|| Unavailable::NoMeetup.into())
            })
            .await
            .map_err(|_unknown| PortError::Unavailable)?;
        let KnockKey {
            grant_key,
            subscription,
            ..
        } = kept;
        let spot = key.id().spot;
        let joined = join(
            &runtime,
            JoinMeetup {
                key,
                welcome,
                grant_key,
                own_subscription: subscription.as_ref().map(|subscription| subscription.id),
                board_seen: self.board_seen().await,
            },
            (self.backend().outside.rng)(),
        )
        .await
        .map_err(|_failed| PortError::Unavailable)?;
        let group = joined.handle.group();
        self.backend()
            .store(move |session, _clock, _rng| Ok(session.knock_keys().delete(&reply_mailbox)?))
            .await
            .map_err(|_failed| PortError::Unavailable)?;
        self.remember(group, subscription.as_ref()).await;
        self.shared
            .start(Started {
                handle: joined.handle,
                events: joined.events,
                spot,
            })
            .await;
        Ok(())
    }

    async fn leave(&self, spot: SpotRef) -> Result<(), PortError> {
        let Some(group) = self.current_meetup(spot) else {
            return Ok(());
        };
        let Some(live) = self.shared.take(group) else {
            return Ok(());
        };
        self.shared
            .leave(live)
            .await
            .map_err(|_failed| PortError::Unavailable)
    }

    async fn leave_all(&self) -> Result<(), PortError> {
        let lives = std::mem::take(&mut *lock(&self.shared.live));
        for live in lives {
            self.shared
                .leave(live)
                .await
                .map_err(|_failed| PortError::Unavailable)?;
        }
        Ok(())
    }
}
