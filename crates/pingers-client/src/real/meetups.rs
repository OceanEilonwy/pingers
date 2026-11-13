// SPDX-License-Identifier: AGPL-3.0-only
//! [`MeetupsPort`] and pingers-wake's `MeetupPort` over pingers-meetup-session's handles.
//!
//! The live Meetups are the handles this process holds; pingers-meetup-session cannot resume
//! a Meetup, so `discard_stale` runs once, on the first call (the first snapshot at launch).
//!
//! WAITING: no Meetup can go live yet. Creating one (`admit`) or joining one (`welcome`)
//! needs `PSK_meet`, which pingers-presence does not hand out yet ([`Unavailable::MeetupPsk`]),
//! and a `Runtime` whose transport and [`GroupLogs`](super::sync::GroupLogs) come from
//! pingers-sync. Everything that acts on a live Meetup is wired and finds none until then.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use pingers_geo::Fix;
use pingers_meetup::{
    ASK_EXPIRY, ExactLocation, LocationAnswer, MeetPointDraft, MeetupAction, TargetPosition,
};
use pingers_meetup_session::{MeetupHandle, MeetupScreen, MeetupView, discard_stale};
use pingers_parties::PartyLifecycle;
use pingers_presence::SpotRef;
use pingers_proto::{ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId};
use pingers_store::{LocationRequestState, RequestDirection, Session};
use pingers_time::{Clock, UnixSeconds};
use pingers_wake::PortError;
use pingers_wake::ports::{MeetupAdmission, MeetupPort, ReplyPost};
use tokio::sync::OnceCell;

use super::Backend;
use super::records::corrupt;
use crate::ports::{MeetupsPort, MeetupsSection};
use crate::snapshot::{
    ChatEntry, ChatMessage, ChatReply, LocationRequest, Meetup, MeetupPeer, MeetupSelf, Name,
    PeerAccess,
};
use crate::{ClientError, Unavailable};

/// A live Meetup and the Spot it is at.
struct Live {
    handle: Arc<MeetupHandle>,
    spot: SpotRef,
}

/// The live Meetups of this process.
pub(crate) struct Meetups {
    backend: Arc<Backend>,
    live: Mutex<Vec<Live>>,
    discarded: OnceCell<usize>,
}

impl Meetups {
    pub(crate) fn new(backend: Arc<Backend>) -> Self {
        Self {
            backend,
            live: Mutex::new(Vec::new()),
            discarded: OnceCell::new(),
        }
    }

    /// Deletes what a killed process left, once.
    async fn launched(&self) -> Result<(), ClientError> {
        self.discarded
            .get_or_try_init(|| discard_stale(&self.backend.database))
            .await?;
        Ok(())
    }

    fn live(&self) -> std::sync::MutexGuard<'_, Vec<Live>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn handles(&self) -> Vec<Arc<MeetupHandle>> {
        self.live()
            .iter()
            .map(|live| Arc::clone(&live.handle))
            .collect()
    }

    fn handle(&self, meetup: GroupId) -> Result<Arc<MeetupHandle>, ClientError> {
        self.handles()
            .into_iter()
            .find(|handle| handle.group() == meetup)
            .ok_or_else(|| Unavailable::NoMeetup.into())
    }

    fn take(&self, meetup: GroupId) -> Option<Arc<MeetupHandle>> {
        let mut live = self.live();
        let index = live.iter().position(|live| live.handle.group() == meetup)?;
        Some(live.swap_remove(index).handle)
    }

    async fn act(&self, meetup: GroupId, action: MeetupAction) -> Result<(), ClientError> {
        self.launched().await?;
        Ok(self.handle(meetup)?.act(action).await?)
    }

    /// The Meetup screen opened or closed, or the app came on screen or left it.
    pub(crate) async fn screen(&self, screen: MeetupScreen) -> Result<(), ClientError> {
        for handle in self.handles() {
            handle.screen(screen).await?;
        }
        Ok(())
    }

    /// A new fix of this device, for every live Meetup.
    pub(crate) async fn own_fix(&self, fix: Fix) -> Result<(), ClientError> {
        for handle in self.handles() {
            handle.own_fix(fix).await?;
        }
        Ok(())
    }
}

/// Leaves a Meetup; a handle still borrowed elsewhere is dropped, which ends its task.
async fn leave(handle: Arc<MeetupHandle>) -> Result<(), ClientError> {
    if let Ok(handle) = Arc::try_unwrap(handle) {
        handle.leave().await?;
    }
    Ok(())
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
        self.backend
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
        self.act(meetup, MeetupAction::AskLocation(peer)).await?;
        // pingers-meetup chooses the id; the ask is in the store once `act` returns.
        self.backend
            .store(move |session, _clock, _rng| {
                session
                    .location_asks()
                    .list(&meetup)?
                    .into_iter()
                    .rev()
                    .find(|ask| ask.direction == RequestDirection::Outgoing && ask.peer == peer)
                    .map(|ask| ask.id)
                    .ok_or_else(corrupt)
            })
            .await
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
        leave(self.take(meetup).ok_or(Unavailable::NoMeetup)?).await
    }

    async fn section(&self) -> Result<MeetupsSection, ClientError> {
        self.launched().await?;
        let views: Vec<MeetupView> = self
            .handles()
            .iter()
            .map(|handle| handle.view().borrow().clone())
            .collect();
        let (meetups, location_requests) = self
            .backend
            .store(move |session, clock, _rng| {
                let mut meetups = Vec::new();
                let mut requests = Vec::new();
                for view in &views {
                    meetups.push(meetup(session, clock, view)?);
                    requests.extend(incoming_requests(session, view.group)?);
                }
                Ok((meetups, requests))
            })
            .await?;
        Ok(MeetupsSection {
            meetups,
            alert: None,
            location_requests,
            ended_meetup: None,
        })
    }
}

/// One live Meetup as the snapshot shows it: the session's view, names from the Party roster,
/// chat from the store.
///
/// WAITING(pingers-meetup-session): `MeetupView` does not say who set the meet point, so the
/// snapshot shows none yet.
fn meetup(session: &Session, clock: &dyn Clock, view: &MeetupView) -> Result<Meetup, ClientError> {
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
            let access = match (peer.position, peer.access, peer.request_pending_until) {
                (Some(position), ExactLocation::Shared { .. }, _) => PeerAccess::Shared(TargetPosition {
                    position: position.position,
                    taken_at: position.taken_at,
                }),
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
    let sharing_with = session
        .location_grants()
        .list(&view.group)?
        .into_iter()
        .map(|grant| grant.grantee)
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
        meet_point: None,
        chat: chat(session, view.group)?,
        sharing_with,
    })
}

/// The stored chat, replies under their message.
fn chat(session: &Session, meetup: GroupId) -> Result<Vec<ChatEntry>, ClientError> {
    let stored = session.meetup_messages().list(&meetup)?;
    Ok(stored
        .iter()
        .filter(|message| message.parent.is_none())
        .map(|message| {
            ChatEntry::Message(ChatMessage {
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
            })
        })
        .collect())
}

/// Requests from other members waiting for an answer.
fn incoming_requests(
    session: &Session,
    meetup: GroupId,
) -> Result<Vec<LocationRequest>, ClientError> {
    Ok(session
        .location_asks()
        .list(&meetup)?
        .into_iter()
        .filter(|ask| {
            ask.direction == RequestDirection::Incoming
                && ask.state == LocationRequestState::Pending
        })
        .map(|ask| LocationRequest {
            id: ask.id,
            meetup,
            from: ask.peer,
            expires_at: UnixSeconds::new(ask.created_at.get().saturating_add(ASK_EXPIRY.as_secs())),
        })
        .collect())
}

impl MeetupPort for Meetups {
    fn current_meetup(&self, spot: SpotRef) -> Option<GroupId> {
        self.live()
            .iter()
            .find(|live| live.spot == spot)
            .map(|live| live.handle.group())
    }

    async fn admit(&self, _admission: MeetupAdmission) -> Result<(), PortError> {
        Err(PortError::Unavailable)
    }

    async fn welcome(&self, _reply: ReplyPost) -> Result<(), PortError> {
        Err(PortError::Unavailable)
    }

    async fn leave_all(&self) -> Result<(), PortError> {
        let live = std::mem::take(&mut *self.live());
        for live in live {
            leave(live.handle)
                .await
                .map_err(|_failed| PortError::Unavailable)?;
        }
        Ok(())
    }
}
