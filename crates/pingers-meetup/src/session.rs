// SPDX-License-Identifier: AGPL-3.0-only
//! The Meetup session state machine (design B.5, B.6, B.6.1).

use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};

use pingers_crypto::{
    AccountPublicKey, GrantContext, GrantPublicKey, GrantSecretKey, LocationAad, LocationAsk,
    LocationKey, open_location, open_location_ask, open_location_grant, seal_location_ask,
};
use pingers_geo::{Bearing, Coordinate, Fix, Meters};
use pingers_proto::{
    AccuracyMeters, ChatBody, ChatMessageId, Delivery, Epoch, GroupId, LeafIndex,
    LocationGeneration, LocationPlaintext, LocationRequestId, MeetPoint, MeetupMessage, SealedAsk,
    SealedGrant, SealedLocationFrame, SeenSegment, Urgency,
};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;

use crate::board::{Agreement, BoardCheck};
use crate::cadence::{BACKGROUND_FRAME_INTERVAL, Cadence};
use crate::chat::{Recorded, Threads};
use crate::convert::rounded;
use crate::requests::{Requests, Sighting, expiry_from};
use crate::sharing::{
    FrameContext, GrantEnd, MeetAtMe, Padding, Plan, Roster, Rotation, Sharing, consent, ephemeral,
    stored,
};
use crate::{
    AllowListChoice, AlwaysAllow, AskBack, CadenceMode, CompassReading, CompassTarget,
    ExactLocation, LocationAnswer, LocationVisibility, MeetPointDraft, MeetPointRegister,
    MeetupError, Observer, TargetPosition, compass, coordinate_from_wire, coordinate_to_wire,
};

/// A member sends a location or keep-alive frame at least this often (design B.5). The
/// location cadence (at most [`BACKGROUND_FRAME_INTERVAL`] between frames) always meets it,
/// so the session never needs a separate `KeepAlive`; it accepts one as a sign of life.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_mins(5);

/// A member silent this long is removed by any other member (design B.5).
pub const SILENCE_LIMIT: Duration = Duration::from_mins(10);

/// A lone member ends the Meetup after this long alone (design B.5).
pub const ALONE_LIMIT: Duration = Duration::from_mins(30);

/// An unanswered location request expires after this long (design B.6.1).
pub const ASK_EXPIRY: Duration = Duration::from_mins(10);

/// The short grant scope (design B.6.1).
pub const SHORT_GRANT: Duration = Duration::from_mins(15);

/// The background cadence of `SealedLocation` frames, real or dummy (design B.6.1, F.3).
/// Kept for the contract; the same value as [`BACKGROUND_FRAME_INTERVAL`]. The live cadence
/// is [`crate::LIVE_FRAME_INTERVAL`].
pub const LOCATION_FRAME_INTERVAL: Duration = BACKGROUND_FRAME_INTERVAL;

/// A Meetup member as the MLS group reports it after a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Its leaf in the Meetup group.
    pub leaf: LeafIndex,
    /// Its signature key (the same as in the Party roster).
    pub signature_key: AccountPublicKey,
    /// Its grant key from the `pingers_grant_key` leaf extension.
    pub grant_key: GrantPublicKey,
}

/// What a session starts from.
pub struct MeetupConfig {
    /// The Meetup group.
    pub group: GroupId,
    /// This device's leaf in it.
    pub own_leaf: LeafIndex,
    /// The epoch at joining.
    pub epoch: Epoch,
    /// This device's grant secret (its public half is in its leaf extension).
    pub own_grant_key: GrantSecretKey,
    /// The members at joining, including this device.
    pub members: Vec<Member>,
    /// The visibility setting in force for this Party.
    pub visibility: LocationVisibility,
    /// The Party's local allow-list.
    pub always_allow: AlwaysAllow,
    /// When this device joined.
    pub joined_at: UnixSeconds,
}

/// Something the user did in the Meetup.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeetupAction {
    /// "Ask to see" a member.
    AskLocation(LeafIndex),
    /// Answer an incoming request.
    AnswerRequest {
        /// The request.
        request: LocationRequestId,
        /// The answer.
        answer: LocationAnswer,
    },
    /// Stop sharing with a member (rotates the location key).
    RevokeLocation(LeafIndex),
    /// Send a chat message.
    SendChat {
        /// The text.
        body: ChatBody,
        /// The thread root it replies to.
        parent: Option<ChatMessageId>,
    },
    /// Set or clear the meet point.
    SetMeetPoint(MeetPointDraft),
    /// Change the visibility setting (to `Nobody` rotates the key).
    SetVisibility(LocationVisibility),
}

/// An input to the session. Every call also passes the current time.
#[derive(Debug)]
#[non_exhaustive]
pub enum MeetupInput {
    /// A decrypted, decoded Meetup frame from the group.
    Received {
        /// The sender's leaf (from MLS).
        sender: LeafIndex,
        /// The epoch it was sent in.
        epoch: Epoch,
        /// The frame.
        message: MeetupMessage,
    },
    /// The group moved to a new epoch with these members (after a commit).
    MembersChanged {
        /// The new epoch.
        epoch: Epoch,
        /// All members now, including this device.
        members: Vec<Member>,
    },
    /// A new fix of this device's position. Coalesced: the next location frame carries the
    /// latest fix; a fix never triggers a frame by itself.
    OwnFix(Fix),
    /// The Meetup screen or compass opened ([`CadenceMode::Live`]) or closed
    /// ([`CadenceMode::Background`]) on this device.
    Cadence(CadenceMode),
    /// The board segment hashes this device saw, to send to the others and compare with theirs
    /// (design B.4.3).
    OwnBoardSeen(Vec<SeenSegment>),
    /// Time passed; send due frames and apply timers.
    Tick,
    /// A user action.
    Action(MeetupAction),
}

/// A frame to encrypt into the group and send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingFrame {
    /// The frame (pad it to its size class before MLS encryption).
    pub message: MeetupMessage,
    /// Stored (chat, meet point, snapshots, grants, asks) or ephemeral (location, board
    /// hashes).
    pub delivery: Delivery,
    /// Whether to wake members whose live channel is down.
    pub wake: Option<Urgency>,
}

/// A peer's decrypted position. Only produced for peers whose location key is held.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeerLocation {
    /// The peer.
    pub leaf: LeafIndex,
    /// Where.
    pub position: Coordinate,
    /// How accurate.
    pub accuracy: Meters,
    /// Which way they face, if known.
    pub heading: Option<Bearing>,
    /// When the fix was taken.
    pub taken_at: UnixSeconds,
}

/// Something the shell or core should know about.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeetupEvent {
    /// A chat message arrived (or this device sent one).
    Chat {
        /// Its id.
        id: ChatMessageId,
        /// Its thread root, if a reply.
        parent: Option<ChatMessageId>,
        /// Who sent it.
        sender: LeafIndex,
        /// The text.
        body: ChatBody,
        /// When it was written.
        written_at: UnixSeconds,
    },
    /// A peer's position updated.
    PeerLocation(PeerLocation),
    /// A member asks to see this device's location (`AskEachTime` only).
    LocationRequested {
        /// Who.
        from: LeafIndex,
        /// The request, for the answer.
        request: LocationRequestId,
    },
    /// Whether this device can see a peer changed (granted, expired, rotated away).
    LocationAccessChanged {
        /// The peer.
        peer: LeafIndex,
        /// The new access.
        access: ExactLocation,
    },
    /// The meet point changed.
    MeetPointChanged,
    /// A member's board segment hashes differ from ours (design B.4.3).
    BoardEquivocation,
    /// "Always allow" added this member's signature key to the Party's allow-list; write it
    /// to the `always_allow` table.
    AlwaysAllowAdded(LeafIndex),
}

/// Why the session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndReason {
    /// This device was alone for [`ALONE_LIMIT`].
    AloneTooLong,
    /// This device was removed from the group.
    Removed,
}

/// What the caller must do.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeetupOutput {
    /// Encrypt and send a frame.
    Send(OutgoingFrame),
    /// Tell the shell.
    Event(MeetupEvent),
    /// Commit removal of a member silent for [`SILENCE_LIMIT`].
    RemoveSilentMember(LeafIndex),
    /// The Meetup is over for this device: delete the group, keys and content.
    End(EndReason),
}

/// The time, randomness and outputs of one `handle` call.
struct Step<'rng, R> {
    now: UnixSeconds,
    rng: &'rng mut R,
    outputs: Vec<MeetupOutput>,
}

/// Who sent a received frame, and in which epoch.
#[derive(Clone, Copy, Debug)]
struct Origin {
    sender: LeafIndex,
    epoch: Epoch,
}

/// A peer's key this device holds, and the last position it opened.
struct HeldKey {
    generation: LocationGeneration,
    key: LocationKey,
    end: GrantEnd,
    last: Option<PeerLocation>,
}

/// What this device tracks about another member.
struct PeerState {
    last_heard: UnixSeconds,
    held: Option<HeldKey>,
    /// The newest generation of this peer's key seen in any frame or grant. A grant of an
    /// older one is stale (it may come late from the group log) and is ignored, even when
    /// no key is held.
    newest: LocationGeneration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifecycle {
    Running,
    Ended,
}

/// One device's view of a live Meetup.
pub struct MeetupSession {
    group: GroupId,
    own_leaf: LeafIndex,
    epoch: Epoch,
    own_grant_key: GrantSecretKey,
    /// The other members, as the group reports them.
    members: BTreeMap<LeafIndex, Member>,
    /// What this device tracks about each of them.
    peers: BTreeMap<LeafIndex, PeerState>,
    sharing: Sharing,
    requests: Requests,
    meet_point: MeetPointRegister,
    threads: Threads,
    board: BoardCheck,
    cadence: Cadence,
    own_position: Option<LocationPlaintext>,
    alone_since: Option<UnixSeconds>,
    lifecycle: Lifecycle,
    /// Outputs prepared by [`MeetupSession::start`], returned by the first `handle`.
    queued: Vec<MeetupOutput>,
}

impl MeetupSession {
    /// Starts a session: generates the first location key, and prepares one grant-sized
    /// frame per other member (real grants under `Everyone`, dummies otherwise, so the
    /// setting does not show), returned by the first [`MeetupSession::handle`]. The first
    /// location frame is due at `joined_at`.
    ///
    /// # Errors
    ///
    /// [`MeetupError::NotAMember`] if `own_leaf` is not among the members;
    /// [`MeetupError::Crypto`] if a grant cannot be sealed.
    pub fn start(config: MeetupConfig, rng: &mut impl CryptoRng) -> Result<Self, MeetupError> {
        let MeetupConfig {
            group,
            own_leaf,
            epoch,
            own_grant_key,
            members,
            visibility,
            always_allow,
            joined_at,
        } = config;
        if !members.iter().any(|member| member.leaf == own_leaf) {
            return Err(MeetupError::NotAMember);
        }
        let others: BTreeMap<LeafIndex, Member> = members
            .into_iter()
            .filter(|member| member.leaf != own_leaf)
            .map(|member| (member.leaf, member))
            .collect();
        let peers = others
            .keys()
            .map(|leaf| {
                (
                    *leaf,
                    PeerState {
                        last_heard: joined_at,
                        held: None,
                        newest: LocationGeneration::new(0),
                    },
                )
            })
            .collect();
        let mut session = Self {
            group,
            own_leaf,
            epoch,
            own_grant_key,
            alone_since: others.is_empty().then_some(joined_at),
            members: others,
            peers,
            sharing: Sharing::new(visibility, always_allow, rng),
            requests: Requests::default(),
            meet_point: MeetPointRegister::new(),
            threads: Threads::default(),
            board: BoardCheck::default(),
            cadence: Cadence::new(joined_at),
            own_position: None,
            lifecycle: Lifecycle::Running,
            queued: Vec::new(),
        };
        let everyone = session.other_leaves();
        let grants = session.reconcile(Padding::Each(&everyone), Rotation::IfRevoked, rng)?;
        session.queued = grants.into_iter().map(MeetupOutput::Send).collect();
        Ok(session)
    }

    /// Processes one input at time `now`.
    ///
    /// # Errors
    ///
    /// [`MeetupError::Ended`] after an [`MeetupOutput::End`]; [`MeetupError::NotAMember`] or
    /// [`MeetupError::CannotTargetSelf`] for an action naming a non-member or this device;
    /// [`MeetupError::UnknownRequest`]; [`MeetupError::Geo`] or [`MeetupError::Proto`] for a
    /// position that does not fit the wire format; [`MeetupError::Overflow`];
    /// [`MeetupError::Crypto`] for internal failures (failed trial-opens of grants, asks and
    /// location frames are not errors: they are how frames for others look).
    pub fn handle(
        &mut self,
        input: MeetupInput,
        now: UnixSeconds,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<MeetupOutput>, MeetupError> {
        if self.lifecycle == Lifecycle::Ended {
            return Err(MeetupError::Ended);
        }
        let mut step = Step {
            now,
            rng,
            outputs: Vec::new(),
        };
        match input {
            MeetupInput::Received {
                sender,
                epoch,
                message,
            } => self.received(Origin { sender, epoch }, message, &mut step)?,
            MeetupInput::MembersChanged { epoch, members } => {
                self.members_changed(epoch, members, &mut step)?;
            }
            MeetupInput::OwnFix(fix) => self.own_position = Some(plaintext_from_fix(&fix)?),
            MeetupInput::Cadence(mode) => self.cadence.set_mode(mode, now)?,
            MeetupInput::OwnBoardSeen(segments) => {
                if self.board.set_own(segments.clone()) == Agreement::Equivocation {
                    step.outputs
                        .push(MeetupOutput::Event(MeetupEvent::BoardEquivocation));
                }
                step.outputs
                    .push(send(ephemeral(MeetupMessage::BoardSeen { segments })));
            }
            MeetupInput::Tick => self.tick(&mut step)?,
            MeetupInput::Action(action) => self.action(action, &mut step)?,
        }
        let mut all = core::mem::take(&mut self.queued);
        all.append(&mut step.outputs);
        Ok(all)
    }

    /// When the next timer falls due (frame cadence, silence, alone, grant expiries); the
    /// caller sends [`MeetupInput::Tick`] then. `None` once the session ended. Request expiry
    /// needs no deadline: an expired request is refused when answered and purged on any tick.
    #[must_use]
    pub fn next_deadline(&self) -> Option<UnixSeconds> {
        if self.lifecycle == Lifecycle::Ended {
            return None;
        }
        let silence = self
            .peers
            .values()
            .filter_map(|peer| peer.last_heard.checked_add(SILENCE_LIMIT));
        let held = self
            .peers
            .values()
            .filter_map(|peer| peer.held.as_ref().and_then(|held| held.end.deadline()));
        let alone = self
            .alone_since
            .and_then(|since| since.checked_add(ALONE_LIMIT));
        core::iter::once(self.cadence.next_due())
            .chain(silence)
            .chain(held)
            .chain(alone)
            .chain(self.sharing.next_expiry())
            .min()
    }

    /// Whether this device can see `peer`.
    #[must_use]
    pub fn exact_location(&self, peer: LeafIndex) -> ExactLocation {
        self.peers
            .get(&peer)
            .and_then(|state| state.held.as_ref())
            .map_or(ExactLocation::Private, |held| held.end.access())
    }

    /// Whether `peer` holds this device's current location key (for "Stop sharing").
    #[must_use]
    pub fn shares_with(&self, peer: LeafIndex) -> bool {
        self.sharing.holds(peer)
    }

    /// When this device's request to see `peer` lapses, while one is pending ("Request
    /// sent").
    #[must_use]
    pub fn pending_request(&self, peer: LeafIndex, now: UnixSeconds) -> Option<UnixSeconds> {
        self.requests.outgoing(peer, now)
    }

    /// The generation of this device's current location key.
    #[cfg(test)]
    pub(crate) const fn generation(&self) -> LocationGeneration {
        self.sharing.generation()
    }

    /// The visibility setting in force.
    #[must_use]
    pub const fn visibility(&self) -> LocationVisibility {
        self.sharing.visibility()
    }

    /// The location-frame cadence in force.
    #[must_use]
    pub const fn cadence(&self) -> CadenceMode {
        self.cadence.mode()
    }

    /// The current meet point.
    #[must_use]
    pub fn meet_point(&self) -> Option<&MeetPoint> {
        self.meet_point.current()
    }

    /// The compass towards `target`, or `None` if the target's position is not decryptable
    /// (design B.6.1: never computed for a member whose key is not held), not yet known, or
    /// is this device itself.
    #[must_use]
    pub fn compass(
        &self,
        target: CompassTarget,
        observer: &Observer,
        now: UnixSeconds,
    ) -> Option<CompassReading> {
        let position = match target {
            CompassTarget::Member(leaf) => self.peer_position(leaf, now)?,
            CompassTarget::MeetPoint => match self.meet_point.current()? {
                MeetPoint::Fixed { position, .. } => TargetPosition {
                    position: coordinate_from_wire(*position).ok()?,
                    taken_at: now,
                },
                MeetPoint::AtMember { leaf, .. } => self.peer_position(*leaf, now)?,
                MeetPoint::Cleared { .. } => return None,
            },
        };
        Some(compass(observer, &position, now))
    }

    fn peer_position(&self, leaf: LeafIndex, now: UnixSeconds) -> Option<TargetPosition> {
        let held = self.peers.get(&leaf)?.held.as_ref()?;
        if held.end.has_lapsed(now) {
            return None;
        }
        let last = held.last?;
        Some(TargetPosition {
            position: last.position,
            taken_at: last.taken_at,
        })
    }

    fn other_leaves(&self) -> BTreeSet<LeafIndex> {
        self.members.keys().copied().collect()
    }

    fn meet_at_me(&self) -> MeetAtMe {
        match self.meet_point.current() {
            Some(MeetPoint::AtMember { leaf, .. }) if *leaf == self.own_leaf => MeetAtMe::Active,
            Some(_) | None => MeetAtMe::Inactive,
        }
    }

    fn reconcile(
        &mut self,
        padding: Padding<'_>,
        rotation: Rotation,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<OutgoingFrame>, MeetupError> {
        let roster = Roster {
            group: &self.group,
            own_leaf: self.own_leaf,
            others: &self.members,
            meet_at_me: self.meet_at_me(),
        };
        self.sharing
            .reconcile(&roster, Plan { padding, rotation }, rng)
    }

    fn push_reconcile(
        &mut self,
        padding: Padding<'_>,
        rotation: Rotation,
        step: &mut Step<'_, impl CryptoRng>,
    ) -> Result<(), MeetupError> {
        let frames = self.reconcile(padding, rotation, step.rng)?;
        step.outputs
            .extend(frames.into_iter().map(MeetupOutput::Send));
        Ok(())
    }

    fn received(
        &mut self,
        origin: Origin,
        message: MeetupMessage,
        step: &mut Step<'_, impl CryptoRng>,
    ) -> Result<(), MeetupError> {
        let sender = origin.sender;
        // Frames from this device or from someone no longer a member are dropped.
        let Some(peer) = self.peers.get_mut(&sender) else {
            return Ok(());
        };
        peer.last_heard = peer.last_heard.max(step.now);
        match message {
            MeetupMessage::SealedLocation(frame) => {
                self.location_received(origin, &frame, step);
            }
            MeetupMessage::LocationGrant(sealed) => {
                self.grant_received(sender, &sealed, step);
            }
            MeetupMessage::LocationAsk(sealed) => {
                self.ask_received(sender, &sealed, step)?;
            }
            MeetupMessage::Chat {
                id,
                parent,
                body,
                written_at,
            } => {
                if let Recorded::New { parent } = self.threads.record(id, parent) {
                    step.outputs.push(MeetupOutput::Event(MeetupEvent::Chat {
                        id,
                        parent,
                        sender,
                        body,
                        written_at,
                    }));
                }
            }
            MeetupMessage::MeetPoint(point) => {
                self.apply_meet_point(sender, point, step)?;
            }
            MeetupMessage::Snapshot {
                meet_point,
                lamport,
            } => {
                // The register accepts an `AtMember` only from that member, so a snapshot
                // cannot vouch for someone else's; the owner re-sends its own to newcomers.
                let point = meet_point.unwrap_or(MeetPoint::Cleared { lamport });
                self.apply_meet_point(sender, point, step)?;
            }
            MeetupMessage::BoardSeen { segments } => {
                if self.board.peer_seen(sender, segments) == Agreement::Equivocation {
                    step.outputs
                        .push(MeetupOutput::Event(MeetupEvent::BoardEquivocation));
                }
            }
            // A keep-alive, or a frame kind this version does not know: a sign of life only.
            MeetupMessage::KeepAlive | _ => {}
        }
        Ok(())
    }

    fn apply_meet_point(
        &mut self,
        sender: LeafIndex,
        point: MeetPoint,
        step: &mut Step<'_, impl CryptoRng>,
    ) -> Result<(), MeetupError> {
        let before = self.meet_point.current().cloned();
        let _update = self.meet_point.apply(sender, point);
        if self.meet_point.current() != before.as_ref() {
            step.outputs
                .push(MeetupOutput::Event(MeetupEvent::MeetPointChanged));
            // Losing "meet at me" ends its implicit grants, which rotates the key.
            self.push_reconcile(Padding::ChangesOnly, Rotation::IfRevoked, step)?;
        }
        Ok(())
    }

    fn location_received(
        &mut self,
        origin: Origin,
        frame: &SealedLocationFrame,
        step: &mut Step<'_, impl CryptoRng>,
    ) {
        let Origin { sender, epoch } = origin;
        let group = self.group;
        let Some(peer) = self.peers.get_mut(&sender) else {
            return;
        };
        peer.newest = peer.newest.max(frame.generation);
        let Some(held) = peer.held.as_mut() else {
            return;
        };
        if held.end.has_lapsed(step.now) || frame.generation > held.generation {
            // Expired, or the sender rotated and did not grant this device the new key.
            peer.held = None;
            step.outputs
                .push(MeetupOutput::Event(MeetupEvent::LocationAccessChanged {
                    peer: sender,
                    access: ExactLocation::Private,
                }));
            return;
        }
        // An older generation's frame fails to open: the generation is in the associated data.
        let aad = LocationAad {
            group: &group,
            epoch,
            sender,
            generation: held.generation,
        };
        // A frame that does not open is a dummy (the sender had no fix yet).
        let Ok(plaintext) = open_location(&held.key, &aad, frame) else {
            return;
        };
        let Ok(location) = peer_location(sender, &plaintext) else {
            return;
        };
        held.last = Some(location);
        step.outputs
            .push(MeetupOutput::Event(MeetupEvent::PeerLocation(location)));
    }

    fn grant_received(
        &mut self,
        sender: LeafIndex,
        sealed: &SealedGrant,
        step: &mut Step<'_, impl CryptoRng>,
    ) {
        let context = GrantContext {
            group: &self.group,
            sender,
        };
        // Most grants are for someone else, or dummies: they do not open.
        let Ok(opened) = open_location_grant(&self.own_grant_key, &context, sealed) else {
            return;
        };
        let Some(peer) = self.peers.get_mut(&sender) else {
            return;
        };
        // A grant can arrive twice (live, then again from the group log) and late (after a
        // newer rotation): an older generation than any seen is stale.
        if opened.generation < peer.newest {
            return;
        }
        peer.newest = opened.generation;
        let previous = peer.held.take();
        let same_generation = previous
            .as_ref()
            .filter(|held| held.generation == opened.generation);
        let previous_end = previous.as_ref().map(|held| held.end);
        // Within one generation the sender only ever extends an end, so the later end wins
        // whatever order the copies arrive in.
        let end = same_generation.map_or(GrantEnd::from_wire(opened.expires_at), |held| {
            held.end.max(GrantEnd::from_wire(opened.expires_at))
        });
        let last = same_generation.and_then(|held| held.last);
        if end.has_lapsed(step.now) {
            peer.held = previous;
            return;
        }
        peer.held = Some(HeldKey {
            generation: opened.generation,
            key: opened.key,
            end,
            last,
        });
        self.requests.answered(sender);
        if previous_end != Some(end) {
            step.outputs
                .push(MeetupOutput::Event(MeetupEvent::LocationAccessChanged {
                    peer: sender,
                    access: end.access(),
                }));
        }
    }

    fn ask_received(
        &mut self,
        sender: LeafIndex,
        sealed: &SealedAsk,
        step: &mut Step<'_, impl CryptoRng>,
    ) -> Result<(), MeetupError> {
        // Asks for someone else, or dummies, do not open.
        let Ok(ask) = open_location_ask(&self.own_grant_key, &self.group, sealed) else {
            return Ok(());
        };
        // The requester inside must be the MLS sender.
        if ask.requester != sender {
            return Ok(());
        }
        // The ask must still be live by its own timestamp (it may be read late from the
        // group log), and is acted on once however often it arrives.
        let Sighting::New { expires } = self.requests.sight(ask.request, ask.asked_at, step.now)?
        else {
            return Ok(());
        };
        if self.sharing.visibility() == LocationVisibility::Nobody {
            return Ok(());
        }
        let Some(member) = self.members.get(&sender) else {
            return Ok(());
        };
        let automatic = !self.sharing.is_excluded(sender)
            && (self.sharing.visibility() == LocationVisibility::Everyone
                || self.sharing.is_always_allowed(&member.signature_key)
                || self.sharing.holds(sender));
        if automatic {
            // A holder who asks lacks the key (a lost grant): re-send it as it was.
            if !self.sharing.holds(sender) {
                self.sharing.approve(sender, GrantEnd::MeetupEnd);
            }
            self.sharing.resend(sender);
            self.push_reconcile(Padding::ChangesOnly, Rotation::IfRevoked, step)?;
        } else {
            self.requests.hold(ask.request, sender, expires);
            step.outputs
                .push(MeetupOutput::Event(MeetupEvent::LocationRequested {
                    from: sender,
                    request: ask.request,
                }));
        }
        Ok(())
    }

    fn members_changed(
        &mut self,
        epoch: Epoch,
        members: Vec<Member>,
        step: &mut Step<'_, impl CryptoRng>,
    ) -> Result<(), MeetupError> {
        self.epoch = epoch;
        if !members.iter().any(|member| member.leaf == self.own_leaf) {
            self.end(EndReason::Removed, &mut step.outputs);
            return Ok(());
        }
        let others: BTreeMap<LeafIndex, Member> = members
            .into_iter()
            .filter(|member| member.leaf != self.own_leaf)
            .map(|member| (member.leaf, member))
            .collect();
        // A leaf whose member changed (reused by someone else) counts as leaving and joining.
        let removed: BTreeSet<LeafIndex> = self
            .members
            .iter()
            .filter(|(leaf, member)| others.get(leaf) != Some(member))
            .map(|(leaf, _member)| *leaf)
            .collect();
        let added: BTreeSet<LeafIndex> = others
            .iter()
            .filter(|(leaf, member)| self.members.get(leaf) != Some(member))
            .map(|(leaf, _member)| *leaf)
            .collect();
        for leaf in &removed {
            self.peers.remove(leaf);
            self.sharing.forget(*leaf);
            self.requests.forget(*leaf);
            self.board.forget(*leaf);
        }
        let lowest_before = self
            .members
            .keys()
            .filter(|leaf| !removed.contains(leaf))
            .chain(core::iter::once(&self.own_leaf))
            .min()
            .copied();
        self.members = others;
        for leaf in &added {
            self.peers.insert(
                *leaf,
                PeerState {
                    last_heard: step.now,
                    held: None,
                    newest: LocationGeneration::new(0),
                },
            );
        }
        self.alone_since = if self.members.is_empty() {
            Some(self.alone_since.unwrap_or(step.now))
        } else {
            None
        };
        if !removed.is_empty() {
            // Any departure rotates, so a rotation does not tell whether the leaver held
            // the key.
            self.push_reconcile(Padding::Each(&added), Rotation::Force, step)?;
        } else if !added.is_empty() {
            self.push_reconcile(Padding::Each(&added), Rotation::IfRevoked, step)?;
        }
        if !added.is_empty() {
            self.welcome_newcomers(lowest_before, &mut step.outputs);
        }
        Ok(())
    }

    /// Late joiners cannot read earlier epochs: the lowest existing member sends the meet
    /// point state, and a member whose position is the meet point re-sends it itself
    /// (others may not vouch for an `AtMember`).
    fn welcome_newcomers(&self, lowest_before: Option<LeafIndex>, outputs: &mut Vec<MeetupOutput>) {
        if lowest_before == Some(self.own_leaf) {
            outputs.push(send(stored(
                MeetupMessage::Snapshot {
                    meet_point: self.meet_point.current().cloned(),
                    lamport: self.meet_point.lamport(),
                },
                None,
            )));
        }
        if let Some(point @ MeetPoint::AtMember { leaf, .. }) = self.meet_point.current()
            && *leaf == self.own_leaf
        {
            outputs.push(send(stored(MeetupMessage::MeetPoint(point.clone()), None)));
        }
    }

    fn tick(&mut self, step: &mut Step<'_, impl CryptoRng>) -> Result<(), MeetupError> {
        self.requests.expire(step.now);
        if self.sharing.expire(step.now) {
            let everyone = self.other_leaves();
            self.push_reconcile(Padding::Each(&everyone), Rotation::IfRevoked, step)?;
        }
        for (leaf, peer) in &mut self.peers {
            if peer
                .held
                .as_ref()
                .is_some_and(|held| held.end.has_lapsed(step.now))
            {
                peer.held = None;
                step.outputs
                    .push(MeetupOutput::Event(MeetupEvent::LocationAccessChanged {
                        peer: *leaf,
                        access: ExactLocation::Private,
                    }));
            }
            if peer
                .last_heard
                .checked_add(SILENCE_LIMIT)
                .is_some_and(|limit| limit <= step.now)
            {
                step.outputs.push(MeetupOutput::RemoveSilentMember(*leaf));
                // Asked once per limit: if the removal does not happen, ask again later.
                peer.last_heard = step.now;
            }
        }
        if self
            .alone_since
            .and_then(|since| since.checked_add(ALONE_LIMIT))
            .is_some_and(|limit| limit <= step.now)
        {
            self.end(EndReason::AloneTooLong, &mut step.outputs);
            return Ok(());
        }
        if self.cadence.is_due(step.now) {
            let context = FrameContext {
                group: &self.group,
                epoch: self.epoch,
                sender: self.own_leaf,
            };
            let frame =
                self.sharing
                    .location_frame(context, self.own_position.as_ref(), step.rng)?;
            step.outputs
                .push(send(ephemeral(MeetupMessage::SealedLocation(frame))));
            self.cadence.sent(step.now)?;
        }
        Ok(())
    }

    fn end(&mut self, reason: EndReason, outputs: &mut Vec<MeetupOutput>) {
        self.lifecycle = Lifecycle::Ended;
        self.queued.clear();
        outputs.push(MeetupOutput::End(reason));
    }

    fn action(
        &mut self,
        action: MeetupAction,
        step: &mut Step<'_, impl CryptoRng>,
    ) -> Result<(), MeetupError> {
        match action {
            MeetupAction::AskLocation(target) => self.ask(target, step),
            MeetupAction::AnswerRequest { request, answer } => self.answer(request, answer, step),
            MeetupAction::RevokeLocation(target) => {
                self.check_other(target)?;
                self.sharing.revoke(target);
                self.push_reconcile(Padding::ChangesOnly, Rotation::Force, step)
            }
            MeetupAction::SendChat { body, parent } => {
                let mut id = [0; ChatMessageId::LENGTH];
                step.rng.fill_bytes(&mut id);
                let id = ChatMessageId::from_bytes(id);
                if let Recorded::New { parent } = self.threads.record(id, parent) {
                    step.outputs.push(send(stored(
                        MeetupMessage::Chat {
                            id,
                            parent,
                            body: body.clone(),
                            written_at: step.now,
                        },
                        Some(Urgency::Alert),
                    )));
                    step.outputs.push(MeetupOutput::Event(MeetupEvent::Chat {
                        id,
                        parent,
                        sender: self.own_leaf,
                        body,
                        written_at: step.now,
                    }));
                }
                Ok(())
            }
            MeetupAction::SetMeetPoint(draft) => {
                let lamport = self
                    .meet_point
                    .lamport()
                    .checked_next()
                    .ok_or(MeetupError::Overflow)?;
                let point = match draft {
                    MeetPointDraft::Fixed { position, label } => MeetPoint::Fixed {
                        position: coordinate_to_wire(position)?,
                        label,
                        lamport,
                    },
                    MeetPointDraft::AtMe => MeetPoint::AtMember {
                        leaf: self.own_leaf,
                        lamport,
                    },
                    MeetPointDraft::Clear => MeetPoint::Cleared { lamport },
                };
                step.outputs.push(send(stored(
                    MeetupMessage::MeetPoint(point.clone()),
                    Some(Urgency::Quiet),
                )));
                self.apply_meet_point(self.own_leaf, point, step)
            }
            MeetupAction::SetVisibility(visibility) => {
                if visibility == self.sharing.visibility() {
                    return Ok(());
                }
                self.sharing.set_visibility(visibility);
                if visibility == LocationVisibility::Nobody {
                    self.requests.drop_incoming();
                }
                let everyone = self.other_leaves();
                self.push_reconcile(Padding::Each(&everyone), Rotation::IfRevoked, step)
            }
        }
    }

    fn check_other(&self, target: LeafIndex) -> Result<&Member, MeetupError> {
        if target == self.own_leaf {
            return Err(MeetupError::CannotTargetSelf);
        }
        self.members.get(&target).ok_or(MeetupError::NotAMember)
    }

    fn ask(
        &mut self,
        target: LeafIndex,
        step: &mut Step<'_, impl CryptoRng>,
    ) -> Result<(), MeetupError> {
        let member = self.check_other(target)?;
        let mut request = [0; LocationRequestId::LENGTH];
        step.rng.fill_bytes(&mut request);
        let ask = LocationAsk {
            group: self.group,
            requester: self.own_leaf,
            request: LocationRequestId::from_bytes(request),
            asked_at: step.now,
        };
        let sealed = seal_location_ask(&member.grant_key, &ask, step.rng)?;
        self.requests.sent(target, expiry_from(step.now)?);
        step.outputs
            .push(send(consent(MeetupMessage::LocationAsk(sealed))));
        Ok(())
    }

    fn answer(
        &mut self,
        request: LocationRequestId,
        answer: LocationAnswer,
        step: &mut Step<'_, impl CryptoRng>,
    ) -> Result<(), MeetupError> {
        let pending = self.requests.take(request, step.now)?;
        match answer {
            // Nothing is sent and nothing is kept: the same as never seeing the request.
            LocationAnswer::NotNow => Ok(()),
            LocationAnswer::Allow {
                scope,
                ask_back,
                always,
            } => {
                let from = pending.from;
                let signature_key = self.check_other(from)?.signature_key;
                self.sharing
                    .approve(from, GrantEnd::from_scope(scope, step.now)?);
                self.sharing.resend(from);
                self.push_reconcile(Padding::ChangesOnly, Rotation::IfRevoked, step)?;
                if always == AllowListChoice::AddToAllowList {
                    self.sharing.allow_always(signature_key);
                    step.outputs
                        .push(MeetupOutput::Event(MeetupEvent::AlwaysAllowAdded(from)));
                }
                if ask_back == AskBack::AskBack {
                    self.ask(from, step)?;
                }
                Ok(())
            }
        }
    }
}

const fn send(frame: OutgoingFrame) -> MeetupOutput {
    MeetupOutput::Send(frame)
}

/// This device's fix in the wire's units. Accuracy saturates at 65,535 m.
fn plaintext_from_fix(fix: &Fix) -> Result<LocationPlaintext, MeetupError> {
    let accuracy = rounded(fix.accuracy.get())
        .ok_or(MeetupError::Overflow)?
        .clamp(0, i64::from(u16::MAX));
    Ok(LocationPlaintext {
        position: coordinate_to_wire(fix.coordinate)?,
        accuracy: AccuracyMeters::new(
            u16::try_from(accuracy).map_err(|_range| MeetupError::Overflow)?,
        ),
        heading: None,
        speed: None,
        taken_at: fix.taken_at,
    })
}

fn peer_location(
    leaf: LeafIndex,
    plaintext: &LocationPlaintext,
) -> Result<PeerLocation, MeetupError> {
    Ok(PeerLocation {
        leaf,
        position: coordinate_from_wire(plaintext.position)?,
        accuracy: Meters::from_whole(u32::from(plaintext.accuracy.get())),
        heading: plaintext
            .heading
            .map(|heading| Bearing::new(f64::from(heading.get())))
            .transpose()?,
        taken_at: plaintext.taken_at,
    })
}
