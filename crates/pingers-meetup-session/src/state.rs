// SPDX-License-Identifier: AGPL-3.0-only
//! The synchronous half of a live Meetup: the pingers-meetup state machine, its MLS group and
//! its rows in the store. Every method runs inside one store transaction (see `live`), takes
//! the current time, and returns what the async half must do on the network.

use std::collections::BTreeMap;

use pingers_crypto::{GrantSecretKey, LogAuthKeyPair};
use pingers_meetup::{
    EndReason, ExactLocation, MeetupConfig, MeetupEvent, MeetupInput, MeetupOutput, MeetupSession,
    Member, PeerLocation,
};
use pingers_mls::{MlsError, ProcessedMessage, SelfRemoval};
use pingers_proto::{
    CommitRequest, Epoch, GroupId, KeyPackageBytes, LeafIndex, MlsMessageBytes, PartyId,
    Subscription, Urgency, WelcomeBytes,
};
use pingers_store::{Session, StoredChat};
use pingers_time::UnixSeconds;
use zeroize::Zeroizing;

use crate::group::{self, Local, MeetupGroup, Outgoing};
use crate::view::{ChannelState, MeetupView, PeerView};
use crate::{SessionError, SessionRng};

/// What the async half must do after a step, in order.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    /// Frames to send, in the order the session produced them.
    pub(crate) outgoing: Vec<Outgoing>,
    /// Events for the shell.
    pub(crate) events: Vec<MeetupEvent>,
    /// Silent members to remove, one commit each.
    pub(crate) removals: Vec<LeafIndex>,
    /// A received proposal (a leaver) waits to be committed.
    pub(crate) commit_proposals: Proposals,
    /// The Meetup is over for this device.
    pub(crate) end: Option<EndReason>,
}

/// Whether a proposal waits for a commit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Proposals {
    /// Nothing waits.
    #[default]
    None,
    /// Commit the stored proposals.
    Waiting,
}

/// A commit this device made, pending until the server answers.
pub(crate) struct PendingCommit {
    /// The request to post.
    pub(crate) request: CommitRequest,
    /// The Welcome for an added member.
    pub(crate) welcome: Option<WelcomeBytes>,
}

/// What a session is started from, once its group exists locally.
pub(crate) struct CoreSetup {
    pub(crate) group: GroupId,
    pub(crate) party: PartyId,
    pub(crate) grant_key: GrantSecretKey,
    pub(crate) rng: SessionRng,
}

/// One live Meetup on this device.
pub(crate) struct Core {
    meetup: MeetupSession,
    group: GroupId,
    party: PartyId,
    own_leaf: LeafIndex,
    epoch: Epoch,
    auth: LogAuthKeyPair,
    /// Every member, this device included, from the MLS roster.
    members: BTreeMap<LeafIndex, Member>,
    /// The latest decrypted position of each peer whose key is held. Overwritten, never
    /// appended: there is no location history (design C.2).
    positions: BTreeMap<LeafIndex, PeerLocation>,
    /// A received proposal waits for a commit; until one lands MLS refuses to send, so every
    /// tick retries the commit (an earlier attempt may have failed on the network).
    proposals: Proposals,
    /// The time of the latest step, for the view.
    now: UnixSeconds,
    rng: SessionRng,
}

impl Core {
    /// Starts the state machine over the group already in the store.
    pub(crate) fn start(
        local: Local<'_>,
        setup: CoreSetup,
        now: UnixSeconds,
    ) -> Result<Self, SessionError> {
        let CoreSetup {
            group,
            party,
            grant_key,
            mut rng,
        } = setup;
        let context = group::party_context(local, &party)?;
        let handle = local.mls().meetup_group(&group)?;
        let members = group::members_of(&handle)?;
        let own_leaf = handle.own_leaf();
        let epoch = handle.epoch();
        let meetup = MeetupSession::start(
            MeetupConfig {
                group,
                own_leaf,
                epoch,
                own_grant_key: grant_key,
                members: members.clone(),
                visibility: context.visibility,
                always_allow: context.always_allow,
                joined_at: now,
            },
            &mut rng,
        )?;
        Ok(Self {
            meetup,
            group,
            party,
            own_leaf,
            epoch,
            auth: handle.log_auth_key_pair()?,
            members: members
                .into_iter()
                .map(|member| (member.leaf, member))
                .collect(),
            positions: BTreeMap::new(),
            proposals: Proposals::None,
            now,
            rng,
        })
    }

    /// The Meetup group.
    pub(crate) const fn group(&self) -> GroupId {
        self.group
    }

    /// A copy of the current epoch's log-auth key, for one network call.
    pub(crate) fn auth(&self) -> LogAuthKeyPair {
        LogAuthKeyPair::from_bytes(Zeroizing::new(*self.auth.expose_bytes()))
    }

    /// When the state machine next needs a tick.
    pub(crate) fn next_deadline(&self) -> Option<UnixSeconds> {
        self.meetup.next_deadline()
    }

    /// Whether no other member is left.
    pub(crate) fn is_alone(&self) -> bool {
        self.members.keys().all(|leaf| *leaf == self.own_leaf)
    }

    /// Feeds one input to the state machine and carries out its outputs locally.
    pub(crate) fn input(
        &mut self,
        local: Local<'_>,
        input: MeetupInput,
        now: UnixSeconds,
    ) -> Result<Effects, SessionError> {
        self.now = now;
        let retry = if matches!(input, MeetupInput::Tick) {
            self.proposals
        } else {
            Proposals::None
        };
        let outputs = self.meetup.handle(input, now, &mut self.rng)?;
        let mut effects = Effects {
            commit_proposals: retry,
            ..Effects::default()
        };
        self.carry_out(local, outputs, &mut effects)?;
        Ok(effects)
    }

    /// Processes one MLS message from the live channel or the group log.
    ///
    /// # Errors
    ///
    /// Frames for other epochs, own echoes and frames that do not open are errors the caller
    /// drops ([`SessionError::is_fatal`] is false for them).
    pub(crate) fn receive(
        &mut self,
        local: Local<'_>,
        body: &MlsMessageBytes,
        now: UnixSeconds,
    ) -> Result<Effects, SessionError> {
        let mut handle = local.mls().meetup_group(&self.group)?;
        match handle.process(body)? {
            ProcessedMessage::Application { sender, plaintext } => {
                let message = group::open(&plaintext)?;
                let epoch = handle.epoch();
                drop(handle);
                self.input(
                    local,
                    MeetupInput::Received {
                        sender,
                        epoch,
                        message,
                    },
                    now,
                )
            }
            ProcessedMessage::Commit(summary) => {
                if summary.self_removal == SelfRemoval::Removed {
                    return Ok(Effects {
                        end: Some(EndReason::Removed),
                        ..Effects::default()
                    });
                }
                self.members_changed(local, &handle, now)
            }
            ProcessedMessage::Proposal { .. } => {
                self.proposals = Proposals::Waiting;
                Ok(Effects {
                    commit_proposals: Proposals::Waiting,
                    ..Effects::default()
                })
            }
            // A kind of message this version does not handle: ignored.
            _ => Ok(Effects::default()),
        }
    }

    /// Commits removal of a silent member.
    pub(crate) fn remove_member(
        &mut self,
        local: Local<'_>,
        leaf: LeafIndex,
    ) -> Result<PendingCommit, SessionError> {
        let mut handle = local.mls().meetup_group(&self.group)?;
        let commit = handle.remove_member(leaf, &mut self.rng)?;
        pending(&handle, commit, Vec::new())
    }

    /// Commits the stored proposals (a leaver's self-remove).
    pub(crate) fn commit_proposals(
        &mut self,
        local: Local<'_>,
    ) -> Result<PendingCommit, SessionError> {
        let mut handle = local.mls().meetup_group(&self.group)?;
        let commit = handle.commit_pending_proposals(&mut self.rng)?;
        pending(&handle, commit, Vec::new())
    }

    /// Commits adding a newcomer, checked against the current Party roster.
    pub(crate) fn add_member(
        &mut self,
        local: Local<'_>,
        newcomer: &AddMember,
    ) -> Result<PendingCommit, SessionError> {
        let roster = group::party_context(local, &self.party)?.roster;
        let mut handle = local.mls().meetup_group(&self.group)?;
        let commit = handle.add_members(
            core::slice::from_ref(&newcomer.key_package),
            &roster,
            &mut self.rng,
        )?;
        pending(
            &handle,
            commit,
            newcomer.subscription.iter().cloned().collect(),
        )
    }

    /// The server accepted this device's commit: merge it and tell the state machine.
    pub(crate) fn commit_accepted(
        &mut self,
        local: Local<'_>,
        now: UnixSeconds,
    ) -> Result<Effects, SessionError> {
        let mut handle = local.mls().meetup_group(&self.group)?;
        let _summary = handle.merge_pending_commit()?;
        self.members_changed(local, &handle, now)
    }

    /// The server refused this device's commit: drop it.
    pub(crate) fn commit_rejected(&self, local: Local<'_>) -> Result<(), SessionError> {
        let mut handle = local.mls().meetup_group(&self.group)?;
        handle.discard_pending_commit()?;
        Ok(())
    }

    /// This device's self-remove proposal, to send before deleting the state (design G.6).
    pub(crate) fn propose_leave(&mut self, local: Local<'_>) -> Result<Outgoing, SessionError> {
        let mut handle = local.mls().meetup_group(&self.group)?;
        let proposal = handle.propose_leave(&mut self.rng)?;
        Ok(group::stored(handle.epoch(), proposal))
    }

    /// What the screens show now.
    pub(crate) fn view(&self, channel: ChannelState) -> MeetupView {
        let peers = self
            .members
            .values()
            .filter(|member| member.leaf != self.own_leaf)
            .map(|member| {
                let access = self.meetup.exact_location(member.leaf);
                PeerView {
                    leaf: member.leaf,
                    signature_key: member.signature_key,
                    access,
                    holds_own_key: self.meetup.shares_with(member.leaf),
                    request_pending_until: self.meetup.pending_request(member.leaf, self.now),
                    position: match access {
                        ExactLocation::Shared { .. } => self.positions.get(&member.leaf).copied(),
                        ExactLocation::Private => None,
                    },
                }
            })
            .collect();
        MeetupView {
            group: self.group,
            own_leaf: self.own_leaf,
            cadence: self.meetup.cadence(),
            visibility: self.meetup.visibility(),
            channel,
            meet_point: self.meetup.meet_point().cloned(),
            meet_point_set_by: self.meetup.meet_point_set_by(),
            peers,
        }
    }

    /// Re-reads the roster after an epoch change and tells the state machine.
    fn members_changed(
        &mut self,
        local: Local<'_>,
        handle: &MeetupGroup<'_>,
        now: UnixSeconds,
    ) -> Result<Effects, SessionError> {
        let members = group::members_of(handle)?;
        self.epoch = handle.epoch();
        // A new epoch drops every proposal not committed in it.
        self.proposals = Proposals::None;
        self.auth = handle.log_auth_key_pair()?;
        self.members = members
            .iter()
            .map(|member| (member.leaf, member.clone()))
            .collect();
        self.positions
            .retain(|leaf, _position| self.members.contains_key(leaf));
        self.input(
            local,
            MeetupInput::MembersChanged {
                epoch: self.epoch,
                members,
            },
            now,
        )
    }

    fn carry_out(
        &mut self,
        local: Local<'_>,
        outputs: Vec<MeetupOutput>,
        effects: &mut Effects,
    ) -> Result<(), SessionError> {
        let mut handle: Option<MeetupGroup<'_>> = None;
        for output in outputs {
            match output {
                MeetupOutput::Send(frame) => {
                    let group = match handle.as_mut() {
                        Some(group) => group,
                        None => handle.insert(local.mls().meetup_group(&self.group)?),
                    };
                    match group::seal(group, &frame, &mut self.rng) {
                        Ok(outgoing) => effects.outgoing.push(outgoing),
                        // A received proposal waits for its commit; MLS refuses to send until
                        // then. The frame is dropped, as one lost on the network would be.
                        Err(SessionError::Mls(MlsError::CommitPending)) => {}
                        Err(error) => return Err(error),
                    }
                }
                MeetupOutput::Event(event) => {
                    self.note(local.session, &event)?;
                    effects.events.push(event);
                }
                MeetupOutput::RemoveSilentMember(leaf) => effects.removals.push(leaf),
                MeetupOutput::End(reason) => effects.end = Some(reason),
                // An output this version does not know: nothing to carry out.
                _ => {}
            }
        }
        Ok(())
    }

    /// Keeps what an event changes: positions, the allow-list, chat.
    fn note(&mut self, session: &Session, event: &MeetupEvent) -> Result<(), SessionError> {
        match event {
            MeetupEvent::PeerLocation(location) => {
                self.positions.insert(location.leaf, *location);
            }
            MeetupEvent::LocationAccessChanged {
                peer,
                access: ExactLocation::Private,
            } => {
                self.positions.remove(peer);
            }
            MeetupEvent::AlwaysAllowAdded(leaf) => {
                let member = self.members.get(leaf).ok_or(SessionError::Internal)?;
                session
                    .always_allow()
                    .add(&self.party, &member.signature_key)?;
            }
            MeetupEvent::Chat {
                id,
                parent,
                sender,
                body,
                written_at,
            } => session.meetup_messages().insert(&StoredChat {
                group: self.group,
                id: *id,
                parent: *parent,
                sender: *sender,
                body: body.clone(),
                written_at: *written_at,
            })?,
            MeetupEvent::BoardEquivocation | _ => {}
        }
        Ok(())
    }
}

/// A newcomer to add after its knock passed this device's liveness check (design B.5).
#[derive(Clone, Debug)]
pub struct AddMember {
    /// The KeyPackage from its knock (with its grant key extension).
    pub key_package: KeyPackageBytes,
    /// Its sealed-push subscription for this group, if it gave one.
    pub subscription: Option<Subscription>,
}

/// The commit request for a pending commit on `handle`.
fn pending(
    handle: &MeetupGroup<'_>,
    commit: pingers_mls::Commit,
    subscriptions_added: Vec<Subscription>,
) -> Result<PendingCommit, SessionError> {
    Ok(PendingCommit {
        request: CommitRequest {
            epoch: handle.epoch(),
            commit: commit.commit,
            next_auth_key: handle.pending_log_auth_key_pair()?.public_key(),
            state: None,
            subscriptions_added,
            subscriptions_removed: Vec::new(),
            wake: Urgency::Quiet,
        },
        welcome: commit.welcome,
    })
}
