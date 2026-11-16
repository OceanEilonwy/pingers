// SPDX-License-Identifier: AGPL-3.0-only
//! The entry point: Party creation, roster changes, commits, incoming messages, settings.
//! Invites and joins are in `joining`, Spots in `spots`.

use pingers_crypto::{AccountPublicKey, LogAuthKeyPair, seal_state_snapshot};
use pingers_mls::{
    CommitSummary, Group, MlsContext, Party as PartyGroup, ProcessedMessage, Roster,
};
use pingers_presence::EpochTransition;
use pingers_proto::{
    AppMessage, CommitRequest, Delivery, Epoch, GroupId, LeafIndex, MailboxId, MlsMessageBytes,
    Padded, PartyId, SizeClass, Subscription, SubscriptionId, Urgency,
};
use pingers_store::{
    Identity, InviteState, LocationVisibility, Member, MemberName, Party, PartyName, Session,
    Verification,
};
use pingers_time::Clock;
use rand_core::CryptoRng;

use crate::codec::{Reader, Writer};
use crate::joining::{Approval, encode_approval, read_approval};
use crate::ops::{PartyOperation, Profile, Snapshot, spot_wins};
use crate::roster::{Myself, RosterMember, RosterView};
use crate::roster_history;
use crate::settings::StoredSettings;
use crate::{
    Outbound, OutgoingCommit, PartiesError, PartySettings, RelayChoice, SafetyCode, WelcomeDelivery,
};

/// Party lifecycle over one store session. Every operation runs in one store transaction, so
/// MLS state and the rows that describe it commit together or not at all (design B.7).
///
/// Operations that must reach the server return [`Outbound`] values for pingers-sync; nothing
/// here does I/O beyond the store.
pub struct PartyLifecycle<'session> {
    pub(crate) session: &'session Session,
    pub(crate) clock: &'session dyn Clock,
}

/// Everything needed to create a Party.
pub struct NewParty {
    /// Its name.
    pub name: PartyName,
    /// This member's display name in it.
    pub own_name: MemberName,
    /// Server, relays, suite and location-visibility override.
    pub settings: PartySettings,
    /// This device's sealed-push subscription for the group, unless wake-up pushes are off.
    pub subscription: Option<Subscription>,
}

/// A created Party and the group registration to send.
pub struct CreatedParty {
    /// The new Party.
    pub party: PartyId,
    /// [`Outbound::CreateGroup`].
    pub outbound: Outbound,
}

/// What a merged commit changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitApplied {
    /// The Party.
    pub party: PartyId,
    /// The new epoch.
    pub epoch: Epoch,
    /// Leaves added.
    pub added: Vec<LeafIndex>,
    /// Leaves removed.
    pub removed: Vec<LeafIndex>,
    /// For pingers-presence's epoch grace (design B.4.7).
    pub transition: EpochTransition,
}

/// What an incoming group message did.
#[non_exhaustive]
pub enum Incoming {
    /// A Spot was created, edited or deleted.
    SpotChanged {
        /// The Party.
        party: PartyId,
        /// The Spot.
        spot: pingers_proto::SpotId,
    },
    /// A member published or changed their roster profile.
    ProfileUpdated {
        /// The Party.
        party: PartyId,
        /// The member.
        leaf: LeafIndex,
    },
    /// An operation lost to a newer one (last writer wins) and changed nothing.
    Superseded {
        /// The Party.
        party: PartyId,
    },
    /// A commit from another member was merged.
    RosterChanged(CommitApplied),
    /// A member proposed to leave; commit it with [`PartyLifecycle::commit_pending_proposals`].
    LeaveProposed {
        /// The Party.
        party: PartyId,
        /// The leaver.
        leaf: LeafIndex,
    },
    /// Another member removed this device. The Party and its keys are deleted; `outbound`
    /// deletes its invite mailboxes.
    Removed {
        /// The Party that is gone.
        party: PartyId,
        /// Requests to send.
        outbound: Vec<Outbound>,
    },
}

/// What this device's in-flight commit does.
pub(crate) enum PendingCommit {
    /// Adds a joiner (boxed: a join request carries a whole KeyPackage).
    Add(Box<Approval>),
    /// Removes a member.
    Remove,
    /// Commits stored proposals (leavers).
    Proposals,
    /// Updates this device's leaf.
    Update,
}

impl PendingCommit {
    fn encode(&self) -> Result<Vec<u8>, PartiesError> {
        let mut writer = Writer::new();
        match self {
            Self::Add(approval) => {
                writer.byte(0);
                writer.variable(&encode_approval(approval)?)?;
            }
            Self::Remove => writer.byte(1),
            Self::Proposals => writer.byte(2),
            Self::Update => writer.byte(3),
        }
        Ok(writer.finish())
    }

    fn decode(bytes: &[u8]) -> Result<Self, PartiesError> {
        let mut reader = Reader::new(bytes, "pending commit");
        let pending = match reader.byte()? {
            0 => {
                let mut inner = Reader::new(reader.variable()?, "pending join");
                let approval = read_approval(&mut inner)?;
                inner.end()?;
                Self::Add(Box::new(approval))
            }
            1 => Self::Remove,
            2 => Self::Proposals,
            3 => Self::Update,
            _ => return Err(reader.malformed()),
        };
        reader.end()?;
        Ok(pending)
    }
}

impl<'session> PartyLifecycle<'session> {
    /// Party operations over `session`, reading wall time (invite expiry, creation times)
    /// from `clock`.
    #[must_use]
    pub const fn new(session: &'session Session, clock: &'session dyn Clock) -> Self {
        Self { session, clock }
    }

    /// Creates a Party with this device as its only member (design G.1, first step).
    ///
    /// # Errors
    ///
    /// [`PartiesError::NoAccount`]; [`PartiesError::Mls`] (for example
    /// `UnsupportedSuite` for a quantum-resistant Party on a build without X-Wing);
    /// [`PartiesError::Store`].
    pub fn create_party(
        &self,
        new: NewParty,
        rng: &mut impl CryptoRng,
    ) -> Result<CreatedParty, PartiesError> {
        let now = self.clock.now();
        self.session.transaction(|session| {
            let identity = identity(session)?;
            let party_id = PartyId::from_bytes(random(rng));
            let group_id = GroupId::from_bytes(random(rng));
            let context = MlsContext::new(session, &identity.signing_key);
            let group =
                context.create_party_group(group_id, new.settings.post_quantum.suite(), rng)?;
            let own_key = identity.signing_key.public_key();
            session.parties().insert(&Party {
                id: party_id,
                group: group_id,
                name: new.name,
                own_leaf: group.own_leaf(),
                created_at: now,
                location_visibility: new.settings.location_visibility,
            })?;
            session.members().put(&Member {
                party: party_id,
                leaf: group.own_leaf(),
                signature_key: own_key,
                name: new.own_name,
                avatar: None,
                verification: Verification::Unverified,
            })?;
            let stored = StoredSettings {
                server: new.settings.server,
                relays: new.settings.relays,
                post_quantum: new.settings.post_quantum,
            };
            stored.write(session, &party_id)?;
            roster_history::record_roster(session, &party_id, group.epoch(), &group.roster())?;
            if let Some(subscription) = &new.subscription {
                set_subscription(session, &party_id, own_key, Some(subscription.id))?;
            }
            let snapshot = snapshot(session, &party_id, &[])?;
            let state = seal_state_snapshot(&group.state_key()?, &snapshot.encode()?, rng)?;
            Ok(CreatedParty {
                party: party_id,
                outbound: Outbound::CreateGroup(pingers_proto::CreateGroup {
                    group_id,
                    auth_key: group.log_auth_key_pair()?.public_key(),
                    subscriptions: new.subscription.into_iter().collect(),
                    state: Some(state),
                }),
            })
        })
    }

    /// The Party's settings.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::Malformed`] if the record is corrupt.
    pub fn settings(&self, party: &PartyId) -> Result<PartySettings, PartiesError> {
        let row = party_row(self.session, party)?;
        let stored = stored_settings(self.session, party)?;
        Ok(PartySettings {
            server: stored.server,
            relays: stored.relays,
            post_quantum: stored.post_quantum,
            location_visibility: row.location_visibility,
        })
    }

    /// Chooses the relays for the Party's anonymous requests.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::Store`].
    pub fn set_relays(&self, party: &PartyId, relays: RelayChoice) -> Result<(), PartiesError> {
        self.session.transaction(|session| {
            let mut stored = stored_settings(session, party)?;
            stored.relays = relays;
            stored.write(session, party)
        })
    }

    /// Sets or clears (`None`) the Party's location-visibility override (design B.6.1).
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::Store`].
    pub fn set_location_visibility(
        &self,
        party: &PartyId,
        visibility: Option<LocationVisibility>,
    ) -> Result<(), PartiesError> {
        party_row(self.session, party)?;
        Ok(self
            .session
            .parties()
            .set_location_visibility(party, visibility)?)
    }

    /// The location visibility that applies in this Party: its override, or the device's
    /// setting.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::Store`].
    pub fn effective_location_visibility(
        &self,
        party: &PartyId,
    ) -> Result<LocationVisibility, PartiesError> {
        match party_row(self.session, party)?.location_visibility {
            Some(visibility) => Ok(visibility),
            None => Ok(self.session.settings().location_visibility()?),
        }
    }

    /// The Party's rosters for knock verification (`pingers_presence::RosterLookup`).
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::Malformed`] if a record is corrupt.
    pub fn roster_view(&self, party: &PartyId) -> Result<RosterView, PartiesError> {
        party_row(self.session, party)?;
        let epochs = roster_history::epochs(self.session, party)?;
        let current = *epochs.first().ok_or(PartiesError::Internal)?;
        let mut rosters = Vec::new();
        for epoch in epochs {
            if let Some(roster) = roster_history::roster_at(self.session, party, epoch)? {
                rosters.push((epoch, roster));
            }
        }
        Ok(RosterView::new(current, rosters))
    }

    /// The members at the current epoch, with names and verification.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::Store`].
    pub fn roster(&self, party: &PartyId) -> Result<Vec<RosterMember>, PartiesError> {
        let row = party_row(self.session, party)?;
        let roster = current_roster(self.session, party)?;
        let members = self.session.members().list(party)?;
        Ok(roster
            .entries()
            .iter()
            .map(|entry| {
                // Member rows follow every merged commit (removed leaves lose theirs), so the
                // row at a leaf is the current occupant's.
                let profile = members.iter().find(|member| member.leaf == entry.leaf);
                RosterMember {
                    leaf: entry.leaf,
                    signature_key: entry.signature_key,
                    name: profile.map(|member| member.name.clone()),
                    verification: profile
                        .map_or(Verification::Unverified, |member| member.verification),
                    is_self: if entry.leaf == row.own_leaf {
                        Myself::Yes
                    } else {
                        Myself::No
                    },
                }
            })
            .collect())
    }

    /// The safety code between this device and the member at `leaf` (design B.1), to compare
    /// in person or over a trusted channel.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`], [`PartiesError::UnknownMember`], [`PartiesError::NoAccount`].
    pub fn safety_code(
        &self,
        party: &PartyId,
        leaf: LeafIndex,
    ) -> Result<SafetyCode, PartiesError> {
        party_row(self.session, party)?;
        let own = identity(self.session)?.signing_key.public_key();
        let roster = current_roster(self.session, party)?;
        let theirs = roster
            .signature_key(leaf)
            .ok_or(PartiesError::UnknownMember)?;
        Ok(SafetyCode::between(&own, theirs))
    }

    /// Marks the member at `leaf` verified if `entered` is the safety code this device
    /// computes for the pair.
    ///
    /// # Errors
    ///
    /// [`PartiesError::SafetyCodeMismatch`] (nothing changes);
    /// [`PartiesError::UnknownMember`] if there is no such member or their profile has not
    /// arrived yet; [`PartiesError::Store`].
    pub fn verify_member(
        &self,
        party: &PartyId,
        leaf: LeafIndex,
        entered: &SafetyCode,
    ) -> Result<(), PartiesError> {
        let expected = self.safety_code(party, leaf)?;
        if expected != *entered {
            return Err(PartiesError::SafetyCodeMismatch);
        }
        let member = self
            .session
            .members()
            .get(party, leaf)?
            .ok_or(PartiesError::UnknownMember)?;
        Ok(self
            .session
            .members()
            .set_verification(party, member.leaf, Verification::Verified)?)
    }

    /// The group's current log-auth key pair, which signs pingers-sync's group requests
    /// (design B.2, D).
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::NoAccount`]; [`PartiesError::Mls`].
    pub fn log_auth_key(&self, party: &PartyId) -> Result<LogAuthKeyPair, PartiesError> {
        let row = party_row(self.session, party)?;
        let identity = identity(self.session)?;
        let context = MlsContext::new(self.session, &identity.signing_key);
        Ok(context.party_group(&row.group)?.log_auth_key_pair()?)
    }

    /// Removes the member at `leaf` (design B.2): a Remove commit that rotates the group's
    /// keys, so the removed member cannot derive the next epoch's presence, state or log-auth
    /// keys. Their subscription is dropped from the server with the commit.
    ///
    /// # Errors
    ///
    /// [`PartiesError::CommitInFlight`]; [`PartiesError::Mls`] (`NotAMember` for an empty
    /// leaf); [`PartiesError::UnknownParty`]; [`PartiesError::Store`].
    pub fn remove_member(
        &self,
        party: &PartyId,
        leaf: LeafIndex,
        rng: &mut impl CryptoRng,
    ) -> Result<OutgoingCommit, PartiesError> {
        self.session.transaction(|session| {
            let (row, identity) = start_commit(session, party)?;
            let context = MlsContext::new(session, &identity.signing_key);
            let mut group = context.party_group(&row.group)?;
            let removed_key = group.roster().signature_key(leaf).copied();
            let epoch = group.epoch();
            let commit = group.remove_member(leaf, rng)?;
            let removed_subscription = match removed_key {
                Some(key) => subscription_of(session, party, &key)?,
                None => None,
            };
            let excluded: Vec<AccountPublicKey> = removed_key.into_iter().collect();
            finish_commit(
                session,
                &group,
                CommitParts {
                    row: &row,
                    epoch,
                    commit,
                    pending: &PendingCommit::Remove,
                    excluded: &excluded,
                    added: None,
                    removed: removed_subscription.into_iter().collect(),
                },
                rng,
            )
        })
    }

    /// Commits stored proposals: leavers' self-removes (design G.6 for Parties).
    ///
    /// # Errors
    ///
    /// As [`Self::remove_member`].
    pub fn commit_pending_proposals(
        &self,
        party: &PartyId,
        rng: &mut impl CryptoRng,
    ) -> Result<OutgoingCommit, PartiesError> {
        self.simple_commit(party, &PendingCommit::Proposals, rng)
    }

    /// Commits an Update of this device's leaf, at least weekly (design B.2).
    ///
    /// # Errors
    ///
    /// As [`Self::remove_member`].
    pub fn self_update(
        &self,
        party: &PartyId,
        rng: &mut impl CryptoRng,
    ) -> Result<OutgoingCommit, PartiesError> {
        self.simple_commit(party, &PendingCommit::Update, rng)
    }

    fn simple_commit(
        &self,
        party: &PartyId,
        pending: &PendingCommit,
        rng: &mut impl CryptoRng,
    ) -> Result<OutgoingCommit, PartiesError> {
        self.session.transaction(|session| {
            let (row, identity) = start_commit(session, party)?;
            let context = MlsContext::new(session, &identity.signing_key);
            let mut group = context.party_group(&row.group)?;
            let epoch = group.epoch();
            let commit = match pending {
                PendingCommit::Update => group.self_update(rng)?,
                PendingCommit::Proposals | PendingCommit::Remove | PendingCommit::Add(_) => {
                    group.commit_pending_proposals(rng)?
                }
            };
            // Leavers' self-removes: their subscriptions go with the commit (design D), in case
            // a leaver could not delete its own, and their profiles leave the snapshot.
            let roster = group.roster();
            let leavers: Vec<AccountPublicKey> = group
                .pending_removals()
                .into_iter()
                .filter_map(|leaf| roster.signature_key(leaf).copied())
                .collect();
            let mut removed = Vec::new();
            for key in &leavers {
                removed.extend(subscription_of(session, party, key)?);
            }
            finish_commit(
                session,
                &group,
                CommitParts {
                    row: &row,
                    epoch,
                    commit,
                    pending,
                    excluded: &leavers,
                    added: None,
                    removed,
                },
                rng,
            )
        })
    }

    /// The server accepted this device's commit: merge it and record the new roster.
    ///
    /// # Errors
    ///
    /// [`PartiesError::NoPendingCommit`]; [`PartiesError::UnknownParty`];
    /// [`PartiesError::Mls`]; [`PartiesError::Store`].
    pub fn commit_accepted(&self, party: &PartyId) -> Result<CommitApplied, PartiesError> {
        self.session.transaction(|session| {
            let row = party_row(session, party)?;
            let pending = take_pending(session, party)?;
            let identity = identity(session)?;
            let context = MlsContext::new(session, &identity.signing_key);
            let mut group = context.party_group(&row.group)?;
            let summary = group.merge_pending_commit()?;
            let added_profile = match &pending {
                PendingCommit::Add(approval) => Some(AddedProfile {
                    key: pingers_mls::inspect_key_package(&approval.request.key_package)?
                        .signature_key,
                    name: MemberName::new(approval.request.display_name.as_str().to_owned())?,
                    subscription: approval
                        .request
                        .party_subscription
                        .as_ref()
                        .map(|subscription| subscription.id),
                }),
                PendingCommit::Remove | PendingCommit::Proposals | PendingCommit::Update => None,
            };
            apply_roster_change(
                session,
                &row,
                &RosterChange {
                    roster: group.roster(),
                    summary,
                    added_profile,
                },
            )
        })
    }

    /// The server refused this device's commit (another commit won the epoch): drop it. An
    /// approved join request goes back to waiting for approval, so it can be committed again
    /// once the device has caught up with the log.
    ///
    /// # Errors
    ///
    /// [`PartiesError::NoPendingCommit`]; [`PartiesError::UnknownParty`];
    /// [`PartiesError::Mls`]; [`PartiesError::Store`].
    pub fn commit_rejected(&self, party: &PartyId) -> Result<(), PartiesError> {
        self.session.transaction(|session| {
            let row = party_row(session, party)?;
            let pending = take_pending(session, party)?;
            let identity = identity(session)?;
            let context = MlsContext::new(session, &identity.signing_key);
            context.party_group(&row.group)?.discard_pending_commit()?;
            if let PendingCommit::Add(approval) = pending {
                crate::joining::store_approval(session, party, &approval)?;
            }
            Ok(())
        })
    }

    /// Applies one message from the Party's group log (fetched by pingers-sync).
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`] if no Party has this group; [`PartiesError::Mls`]
    /// (`WrongEpoch`, `InvalidMessage`); [`PartiesError::Malformed`] for an operation that
    /// does not decode or misattributes a Spot's creator; [`PartiesError::Store`].
    pub fn process_group_message(
        &self,
        group: &GroupId,
        message: &MlsMessageBytes,
    ) -> Result<Incoming, PartiesError> {
        self.session.transaction(|session| {
            let row = session
                .parties()
                .by_group(group)?
                .ok_or(PartiesError::UnknownParty)?;
            let identity = identity(session)?;
            let context = MlsContext::new(session, &identity.signing_key);
            let mut mls = context.party_group(group)?;
            match mls.process(message)? {
                ProcessedMessage::Application { sender, plaintext } => {
                    let padded = Padded::from_wire(plaintext.to_vec())?;
                    let operation = PartyOperation::decode(padded.payload()?, row.id)?;
                    let sender_key = mls
                        .roster()
                        .signature_key(sender)
                        .copied()
                        .ok_or(PartiesError::UnknownMember)?;
                    apply_operation(
                        session,
                        &row,
                        Sender {
                            leaf: sender,
                            key: sender_key,
                        },
                        operation,
                    )
                }
                ProcessedMessage::Commit(summary) => {
                    if summary.self_removal == pingers_mls::SelfRemoval::Removed {
                        drop(mls);
                        let outbound = delete_party(session, &context, &row)?;
                        return Ok(Incoming::Removed {
                            party: row.id,
                            outbound,
                        });
                    }
                    Ok(Incoming::RosterChanged(apply_roster_change(
                        session,
                        &row,
                        &RosterChange {
                            roster: mls.roster(),
                            summary,
                            added_profile: None,
                        },
                    )?))
                }
                ProcessedMessage::Proposal { sender } => Ok(Incoming::LeaveProposed {
                    party: row.id,
                    leaf: sender,
                }),
                _ => Err(PartiesError::Malformed {
                    what: "group message",
                }),
            }
        })
    }

    /// Leaves a Party: a self-remove proposal for another member to commit, then this
    /// device deletes the Party, its keys, Spots and invites at once.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::Mls`]; [`PartiesError::Store`].
    pub fn leave_party(
        &self,
        party: &PartyId,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<Outbound>, PartiesError> {
        self.session.transaction(|session| {
            let row = party_row(session, party)?;
            let identity = identity(session)?;
            let context = MlsContext::new(session, &identity.signing_key);
            let mut group = context.party_group(&row.group)?;
            let auth = group.log_auth_key_pair()?;
            let epoch = group.epoch();
            let proposal = group.propose_leave(rng)?;
            drop(group);
            let subscription = subscription_of(session, party, &identity.signing_key.public_key())?;
            let mut outbound = vec![Outbound::Leave(crate::LeaveRequest {
                group: row.group,
                proposal: AppMessage {
                    epoch,
                    body: proposal,
                    delivery: Delivery::Stored,
                    wake: Some(Urgency::Quiet),
                },
                subscription,
                auth,
            })];
            outbound.extend(delete_party(session, &context, &row)?);
            Ok(outbound)
        })
    }
}

// --- shared helpers ---

pub(crate) fn random<const N: usize>(rng: &mut impl CryptoRng) -> [u8; N] {
    let mut bytes = [0; N];
    rng.fill_bytes(&mut bytes);
    bytes
}

pub(crate) fn identity(session: &Session) -> Result<Identity, PartiesError> {
    session.identity().load()?.ok_or(PartiesError::NoAccount)
}

pub(crate) fn party_row(session: &Session, party: &PartyId) -> Result<Party, PartiesError> {
    session
        .parties()
        .get(party)?
        .ok_or(PartiesError::UnknownParty)
}

pub(crate) fn stored_settings(
    session: &Session,
    party: &PartyId,
) -> Result<StoredSettings, PartiesError> {
    StoredSettings::read(session, party)?.ok_or(PartiesError::UnknownParty)
}

pub(crate) fn current_roster(session: &Session, party: &PartyId) -> Result<Roster, PartiesError> {
    let epoch = *roster_history::epochs(session, party)?
        .first()
        .ok_or(PartiesError::Internal)?;
    roster_history::roster_at(session, party, epoch)?.ok_or(PartiesError::Internal)
}

/// The Party's state for a new member, leaving out the profiles of `excluded` keys (members
/// a pending commit removes).
pub(crate) fn snapshot(
    session: &Session,
    party: &PartyId,
    excluded: &[AccountPublicKey],
) -> Result<Snapshot, PartiesError> {
    let row = party_row(session, party)?;
    let subscriptions = session.member_subscriptions().list(party)?;
    let profiles = session
        .members()
        .list(party)?
        .into_iter()
        .filter(|member| !excluded.contains(&member.signature_key))
        .map(|member| Profile {
            key: member.signature_key,
            subscription: subscriptions
                .iter()
                .find(|entry| entry.key == member.signature_key)
                .map(|entry| entry.subscription),
            name: member.name,
        })
        .collect();
    Ok(Snapshot {
        party: *party,
        name: row.name,
        profiles,
        spots: session.spots().list(party)?,
    })
}

/// Seals a Party operation as an application message (stored, quiet wake: design G.2).
pub(crate) fn seal_operation(
    group: &mut Group<'_, Session, PartyGroup>,
    operation: &PartyOperation,
    rng: &mut impl CryptoRng,
) -> Result<Outbound, PartiesError> {
    let padded = Padded::pad_smallest(&operation.encode()?, &SizeClass::MAILBOX_POST)?;
    let body = group.seal_application(&padded, rng)?;
    Ok(Outbound::GroupMessage {
        group: group.group_id(),
        message: AppMessage {
            epoch: group.epoch(),
            body,
            delivery: Delivery::Stored,
            wake: Some(Urgency::Quiet),
        },
    })
}

/// Loads the Party and account for a new commit, refusing if one is already in flight.
pub(crate) fn start_commit(
    session: &Session,
    party: &PartyId,
) -> Result<(Party, Identity), PartiesError> {
    let row = party_row(session, party)?;
    if session.pending_commits().get(party)?.is_some() {
        return Err(PartiesError::CommitInFlight);
    }
    Ok((row, identity(session)?))
}

fn take_pending(session: &Session, party: &PartyId) -> Result<PendingCommit, PartiesError> {
    let bytes = session
        .pending_commits()
        .get(party)?
        .ok_or(PartiesError::NoPendingCommit)?;
    let pending = PendingCommit::decode(&bytes)?;
    session.pending_commits().delete(party)?;
    Ok(pending)
}

pub(crate) struct CommitParts<'parts> {
    pub(crate) row: &'parts Party,
    pub(crate) epoch: Epoch,
    pub(crate) commit: pingers_mls::Commit,
    pub(crate) pending: &'parts PendingCommit,
    /// Keys whose profiles the snapshot leaves out.
    pub(crate) excluded: &'parts [AccountPublicKey],
    /// The added member's subscription and reply mailbox.
    pub(crate) added: Option<Added>,
    pub(crate) removed: Vec<SubscriptionId>,
}

/// Records the pending commit and builds the request: the next epoch's log-auth key and a
/// snapshot sealed under the next epoch's state key go with it (design B.2).
pub(crate) fn finish_commit(
    session: &Session,
    group: &Group<'_, Session, PartyGroup>,
    parts: CommitParts<'_>,
    rng: &mut impl CryptoRng,
) -> Result<OutgoingCommit, PartiesError> {
    let row = parts.row;
    session
        .pending_commits()
        .put(&row.id, &parts.pending.encode()?)?;
    let snapshot = snapshot(session, &row.id, parts.excluded)?;
    let state = seal_state_snapshot(&group.pending_state_key()?, &snapshot.encode()?, rng)?;
    let (subscriptions_added, welcome) = match (parts.added, parts.commit.welcome) {
        (Some(added), Some(welcome)) => (
            added.subscription.into_iter().collect(),
            Some(WelcomeDelivery {
                mailbox: added.reply,
                body: Padded::pad_smallest(welcome.as_bytes(), &SizeClass::MAILBOX_POST)?,
            }),
        ),
        (Some(_), None) => return Err(PartiesError::Internal),
        (None, _) => (Vec::new(), None),
    };
    Ok(OutgoingCommit {
        party: row.id,
        group: row.group,
        request: CommitRequest {
            epoch: parts.epoch,
            commit: parts.commit.commit,
            next_auth_key: group.pending_log_auth_key_pair()?.public_key(),
            state: Some(state),
            subscriptions_added,
            subscriptions_removed: parts.removed,
            wake: Urgency::Quiet,
        },
        welcome,
    })
}

/// A member a commit adds: their subscription, if they have wake-up pushes on, and where their
/// Welcome goes.
pub(crate) struct Added {
    pub(crate) subscription: Option<Subscription>,
    pub(crate) reply: MailboxId,
}

/// The added member's profile from their join request; `key` is their KeyPackage's signature
/// key, so the name goes to the leaf that key holds.
struct AddedProfile {
    key: AccountPublicKey,
    name: MemberName,
    subscription: Option<SubscriptionId>,
}

/// A merged commit: the roster after it, what it changed, and (for this device's own Add) the
/// added member's profile from their join request.
struct RosterChange {
    roster: Roster,
    summary: CommitSummary,
    added_profile: Option<AddedProfile>,
}

/// Brings member rows, subscriptions and roster history in line with a merged commit.
fn apply_roster_change(
    session: &Session,
    row: &Party,
    change: &RosterChange,
) -> Result<CommitApplied, PartiesError> {
    let RosterChange {
        roster,
        summary,
        added_profile,
    } = change;
    let previous = current_roster(session, &row.id)?;
    for leaf in &summary.removed {
        if session.members().get(&row.id, *leaf)?.is_some() {
            session.members().remove(&row.id, *leaf)?;
        }
        if let Some(key) = previous.signature_key(*leaf) {
            set_subscription(session, &row.id, *key, None)?;
        }
    }
    if let Some(profile) = added_profile {
        for leaf in &summary.added {
            if let Some(key) = roster
                .signature_key(*leaf)
                .filter(|key| **key == profile.key)
            {
                session.members().put(&Member {
                    party: row.id,
                    leaf: *leaf,
                    signature_key: *key,
                    name: profile.name.clone(),
                    avatar: None,
                    verification: Verification::Unverified,
                })?;
                set_subscription(session, &row.id, *key, profile.subscription)?;
            }
        }
    }
    roster_history::record_roster(session, &row.id, summary.epoch, roster)?;
    Ok(CommitApplied {
        party: row.id,
        epoch: summary.epoch,
        added: summary.added.clone(),
        removed: summary.removed.clone(),
        transition: if summary.removed.is_empty() {
            EpochTransition::Other
        } else {
            EpochTransition::Removal
        },
    })
}

/// The member who sent an application message.
#[derive(Clone, Copy)]
pub(crate) struct Sender {
    pub(crate) leaf: LeafIndex,
    pub(crate) key: AccountPublicKey,
}

pub(crate) fn apply_operation(
    session: &Session,
    row: &Party,
    from: Sender,
    operation: PartyOperation,
) -> Result<Incoming, PartiesError> {
    let Sender {
        leaf: sender,
        key: sender_key,
    } = from;
    match operation {
        PartyOperation::Profile { name, subscription } => {
            let verification = match session.members().get(&row.id, sender)? {
                Some(member) if member.signature_key == sender_key => member.verification,
                Some(_) | None => Verification::Unverified,
            };
            session.members().put(&Member {
                party: row.id,
                leaf: sender,
                signature_key: sender_key,
                name,
                avatar: None,
                verification,
            })?;
            set_subscription(session, &row.id, sender_key, subscription)?;
            Ok(Incoming::ProfileUpdated {
                party: row.id,
                leaf: sender,
            })
        }
        PartyOperation::Spot(spot) => {
            let malformed = PartiesError::Malformed {
                what: "spot operation",
            };
            let stored = session.spots().get(&spot.id)?;
            let wins = match &stored {
                // A Spot id is global; one filed under another Party is not this Party's.
                Some(stored) if stored.party != row.id => return Err(malformed),
                Some(stored) if stored.created_by != spot.created_by => return Err(malformed),
                Some(stored) => spot_wins(&spot, stored)?,
                None if spot.created_by != sender_key => return Err(malformed),
                None => true,
            };
            if !wins {
                return Ok(Incoming::Superseded { party: row.id });
            }
            put_spot(session, &spot)?;
            Ok(Incoming::SpotChanged {
                party: row.id,
                spot: spot.id,
            })
        }
    }
}

/// Deletes a Party with its MLS group and rows; returns requests deleting the invite
/// mailboxes still open on the server (a used or cancelled invite's mailbox went already).
fn delete_party(
    session: &Session,
    context: &MlsContext<'_, Session>,
    row: &Party,
) -> Result<Vec<Outbound>, PartiesError> {
    let mut outbound = Vec::new();
    for invite in session.party_invites().list(&row.id)? {
        if invite.state == InviteState::Open {
            outbound.push(Outbound::DeleteMailbox {
                mailbox: invite.mailbox,
            });
        }
    }
    context.delete_group(&row.group)?;
    session.parties().delete(&row.id)?;
    Ok(outbound)
}

// --- members' subscriptions ---

pub(crate) fn subscription_of(
    session: &Session,
    party: &PartyId,
    key: &AccountPublicKey,
) -> Result<Option<SubscriptionId>, PartiesError> {
    Ok(session.member_subscriptions().get(party, key)?)
}

/// Sets (or with `None`, forgets) a member's subscription.
pub(crate) fn set_subscription(
    session: &Session,
    party: &PartyId,
    key: AccountPublicKey,
    subscription: Option<SubscriptionId>,
) -> Result<(), PartiesError> {
    Ok(session
        .member_subscriptions()
        .set(party, &key, subscription)?)
}

/// Stores a Spot; a tombstone also drops the Spot's presence state, so a deleted Spot leaves
/// nothing about this device's whereabouts behind.
pub(crate) fn put_spot(session: &Session, spot: &pingers_store::Spot) -> Result<(), PartiesError> {
    session.spots().put(spot)?;
    if spot.status == pingers_store::SpotStatus::Deleted {
        session.presence().clear(&spot.id)?;
    }
    Ok(())
}
