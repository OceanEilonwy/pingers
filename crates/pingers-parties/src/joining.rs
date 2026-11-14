// SPDX-License-Identifier: AGPL-3.0-only
//! Invites (inviter side) and joins (joiner side): design D invite flow, design G.1.

use pingers_crypto::{
    InviteSecret, LogAuthKeyPair, open_join_request, open_state_snapshot, seal_join_request,
};
use pingers_mls::{CipherSuitePolicy, KeyPackagePurpose, MlsContext};
use pingers_proto::{
    DisplayName, GroupId, InvitePayload, InviteUses, JoinRequest, MailboxId, Padded, PartyId,
    SealedSnapshot, SecretBytes, Subscription, WelcomeBytes, WireDecode as _, WireEncode as _,
};
use pingers_store::{MailboxPurpose, Member, MemberName, Party, Session, Verification};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::codec::{Reader, Writer};
use crate::invite::{InviteRecord, StoredStatus};
use crate::lifecycle::{
    CommitParts, PendingCommit, finish_commit, identity, party_row, seal_operation,
    set_subscription, start_commit, stored_settings,
};
use crate::ops::{PartyOperation, Snapshot};
use crate::records::{self, Key};
use crate::settings::StoredSettings;
use crate::{
    INVITE_LIFETIME, InviteId, InviteLink, InviteStatus, InviteSummary, JoinId, Outbound,
    OutgoingCommit, PartiesError, PartyLifecycle, PendingJoin, PostQuantum, Refusal, RelayChoice,
    inviter_fingerprint,
};

/// A new invite.
pub struct CreatedInvite {
    /// The invite, for its status and for revoking it.
    pub invite: InviteId,
    /// The link (and QR code text) to hand to the joiner out of band.
    pub link: InviteLink,
    /// When it stops accepting join requests.
    pub expires: UnixSeconds,
}

/// What became of a join request posted to an invite mailbox.
pub enum JoinRequestOutcome {
    /// A single-use invite admits its joiner at once: post this commit, then the Welcome.
    Admitted(OutgoingCommit),
    /// A multi-use invite waits for the inviter: show "Bob wants to join".
    AwaitingApproval(PendingJoin),
    /// Turned away; nothing to send.
    Refused(Refusal),
}

/// What the joiner supplies besides the link.
pub struct JoinDetails {
    /// The name to show the Party.
    pub display_name: DisplayName,
    /// The joiner's sealed-push subscription for the Party group (`party_sub`).
    pub subscription: Subscription,
}

/// A join request ready to post.
pub struct PreparedJoin {
    /// The join, for [`PartyLifecycle::accept_welcome`] when the Welcome arrives in its
    /// reply mailbox.
    pub join: JoinId,
    /// [`Outbound::MailboxPost`] to the invite mailbox.
    pub outbound: Outbound,
}

/// What pingers-sync collected for a join: the Welcome from the reply mailbox and the group's
/// state snapshot (`GET /v1/groups/{id}/state`).
pub struct ArrivedWelcome {
    /// The join this device started.
    pub join: JoinId,
    /// The reply mailbox's message, padded to a mailbox size class.
    pub welcome: Padded,
    /// The group's latest state snapshot.
    pub snapshot: SealedSnapshot,
}

/// What a joiner needs to read the group's state snapshot before joining: the group and the
/// log-auth key of the epoch its Welcome joins (`GET /v1/groups/{id}/state` is a group request).
pub struct WelcomeAccess {
    /// The group the Welcome is for.
    pub group: GroupId,
    /// That epoch's log-auth key pair.
    pub auth: LogAuthKeyPair,
}

/// A Party this device has joined.
pub struct Joined {
    /// The Party.
    pub party: PartyId,
    /// This device's roster profile for the group, and the reply mailbox's deletion.
    pub outbound: Vec<Outbound>,
}

impl PartyLifecycle<'_> {
    /// Creates an invite: takes a pooled invite mailbox (registered by pingers-sync with an
    /// alert-urgency sealed push), draws the invite secret, and returns the link. The invite
    /// accepts join requests for 24 hours, or until the mailbox expires if that is sooner;
    /// a single-use invite stops after its first joiner.
    ///
    /// # Errors
    ///
    /// [`PartiesError::NoPooledMailbox`] if pingers-sync has to register an invite mailbox
    /// first; [`PartiesError::UnknownParty`]; [`PartiesError::NoAccount`];
    /// [`PartiesError::Store`].
    pub fn create_invite(
        &self,
        party: &PartyId,
        uses: InviteUses,
        rng: &mut impl CryptoRng,
    ) -> Result<CreatedInvite, PartiesError> {
        let now = self.clock.now();
        self.session.transaction(|session| {
            party_row(session, party)?;
            let identity = identity(session)?;
            let settings = stored_settings(session, party)?;
            let mailbox = session
                .mailboxes()
                .take_pooled(MailboxPurpose::Invite, now)?
                .ok_or(PartiesError::NoPooledMailbox)?;
            let secret = InviteSecret::generate(rng);
            let expires = now
                .checked_add(INVITE_LIFETIME)
                .ok_or(PartiesError::Internal)?
                .min(mailbox.expires_at);
            let record = InviteRecord {
                secret: Zeroizing::new(*secret.expose_bytes()),
                uses,
                expires,
                status: StoredStatus::Open,
            };
            let id = mailbox.id();
            records::write(session, &Key::Invite(party, &id), &record.encode())?;
            records::write(session, &Key::InviteIndex(&id), party.as_bytes())?;
            records::add_to_list(session, &Key::Invites(party), id)?;
            let link = InviteLink::new(&InvitePayload {
                server: settings.server,
                invite_mailbox: id,
                invite_secret: SecretBytes::new(*secret.expose_bytes()),
                inviter_fingerprint: inviter_fingerprint(&identity.signing_key.public_key()),
                expires,
                uses,
            })?;
            Ok(CreatedInvite {
                invite: InviteId::for_mailbox(id),
                link,
                expires,
            })
        })
    }

    /// The invite's status at `now`.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownInvite`]; [`PartiesError::Malformed`] if the record is corrupt.
    pub fn invite_status(&self, invite: InviteId) -> Result<InviteStatus, PartiesError> {
        let now = self.clock.now();
        let party = invite_party(self.session, &invite.mailbox())?;
        Ok(invite_record(self.session, &party, &invite.mailbox())?.status_at(now))
    }

    /// The Party's invites, oldest first.
    ///
    /// # Errors
    ///
    /// [`PartiesError::Malformed`] if a record is corrupt; [`PartiesError::Store`].
    pub fn invites(&self, party: &PartyId) -> Result<Vec<InviteSummary>, PartiesError> {
        let now = self.clock.now();
        records::read_mailbox_list(self.session, &Key::Invites(party))?
            .into_iter()
            .map(|mailbox| {
                let record = invite_record(self.session, party, &mailbox)?;
                Ok(InviteSummary {
                    invite: InviteId::for_mailbox(mailbox),
                    uses: record.uses,
                    expires: record.expires,
                    status: record.status_at(now),
                })
            })
            .collect()
    }

    /// Revokes an invite: no further join requests are accepted, and the mailbox is deleted
    /// on the server ("a link is a bearer capability: revoke by deleting the invite mailbox").
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownInvite`]; [`PartiesError::Store`].
    pub fn revoke_invite(&self, invite: InviteId) -> Result<Outbound, PartiesError> {
        self.session.transaction(|session| {
            let mailbox = invite.mailbox();
            let party = invite_party(session, &mailbox)?;
            let mut record = invite_record(session, &party, &mailbox)?;
            record.status = StoredStatus::Revoked;
            records::write(session, &Key::Invite(&party, &mailbox), &record.encode())?;
            Ok(Outbound::DeleteMailbox { mailbox })
        })
    }

    /// Handles a post to an invite mailbox. A request that does not open under the invite's
    /// secret is an error; a request to an expired, used or revoked invite is refused; a
    /// single-use invite admits at once and is then used up; a multi-use invite waits for
    /// [`Self::approve_join`].
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownInvite`]; [`PartiesError::Crypto`] if it was not sealed with
    /// the invite's secret; [`PartiesError::Proto`] if it does not decode;
    /// [`PartiesError::CommitInFlight`]; [`PartiesError::Mls`] (a malformed key package);
    /// [`PartiesError::Store`].
    pub fn receive_join_request(
        &self,
        invite: InviteId,
        post: &Padded,
        rng: &mut impl CryptoRng,
    ) -> Result<JoinRequestOutcome, PartiesError> {
        let now = self.clock.now();
        self.session.transaction(|session| {
            let mailbox = invite.mailbox();
            let party = invite_party(session, &mailbox)?;
            let mut record = invite_record(session, &party, &mailbox)?;
            match record.status_at(now) {
                InviteStatus::Open => {}
                InviteStatus::Used => return Ok(JoinRequestOutcome::Refused(Refusal::AlreadyUsed)),
                InviteStatus::Expired => return Ok(JoinRequestOutcome::Refused(Refusal::Expired)),
                InviteStatus::Revoked => return Ok(JoinRequestOutcome::Refused(Refusal::Revoked)),
            }
            let secret = InviteSecret::from_bytes(Zeroizing::new(*record.secret));
            let plaintext = open_join_request(&secret, post.as_bytes())?;
            let request = JoinRequest::decode(&plaintext)?;
            match record.uses {
                InviteUses::Single => {
                    record.status = StoredStatus::Used;
                    records::write(session, &Key::Invite(&party, &mailbox), &record.encode())?;
                    admit(
                        session,
                        &party,
                        Approval {
                            invite: mailbox,
                            request,
                        },
                        rng,
                    )
                    .map(JoinRequestOutcome::Admitted)
                }
                InviteUses::Multiple => {
                    let pending = PendingJoin {
                        join: JoinId::new(request.reply_mailbox),
                        name: request.display_name.clone(),
                    };
                    store_approval(
                        session,
                        &party,
                        &Approval {
                            invite: mailbox,
                            request,
                        },
                    )?;
                    Ok(JoinRequestOutcome::AwaitingApproval(pending))
                }
            }
        })
    }

    /// Join requests waiting for approval in this Party.
    ///
    /// # Errors
    ///
    /// [`PartiesError::Malformed`] if a record is corrupt; [`PartiesError::Store`].
    pub fn pending_joins(&self, party: &PartyId) -> Result<Vec<PendingJoin>, PartiesError> {
        records::read_mailbox_list(self.session, &Key::Approvals(party))?
            .into_iter()
            .map(|reply| {
                let approval = load_approval(self.session, party, &reply)?;
                Ok(PendingJoin {
                    join: JoinId::new(reply),
                    name: approval.request.display_name,
                })
            })
            .collect()
    }

    /// Approves a waiting join request: commits the Add. The invite must not have expired or
    /// been revoked since.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownJoin`]; [`PartiesError::InviteExpired`] if the invite expired
    /// or was revoked since; [`PartiesError::CommitInFlight`]; [`PartiesError::Mls`];
    /// [`PartiesError::Store`].
    pub fn approve_join(
        &self,
        party: &PartyId,
        join: JoinId,
        rng: &mut impl CryptoRng,
    ) -> Result<OutgoingCommit, PartiesError> {
        let now = self.clock.now();
        self.session.transaction(|session| {
            let reply = join.reply_mailbox();
            let approval = load_approval(session, party, &reply)?;
            let record = invite_record(session, party, &approval.invite)?;
            match record.status_at(now) {
                // A used single-use invite still has its one joiner here after a rejected
                // commit (`commit_rejected` puts the request back).
                InviteStatus::Open | InviteStatus::Used => {}
                InviteStatus::Expired | InviteStatus::Revoked => {
                    return Err(PartiesError::InviteExpired);
                }
            }
            drop_approval(session, party, &reply)?;
            admit(session, party, approval, rng)
        })
    }

    /// Declines a waiting join request; nothing is sent.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownJoin`]; [`PartiesError::Store`].
    pub fn decline_join(&self, party: &PartyId, join: JoinId) -> Result<(), PartiesError> {
        self.session.transaction(|session| {
            let reply = join.reply_mailbox();
            load_approval(session, party, &reply)?;
            drop_approval(session, party, &reply)
        })
    }

    /// Joiner: builds the join request for a link: a fresh KeyPackage, the display name, a
    /// pooled reply mailbox and the Party subscription, sealed under the invite secret and
    /// padded to a mailbox size class.
    ///
    /// # Errors
    ///
    /// [`PartiesError::InvalidLink`] or [`PartiesError::Proto`] for a bad link;
    /// [`PartiesError::InviteExpired`]; [`PartiesError::NoPooledMailbox`] if pingers-sync has
    /// to register a reply mailbox first; [`PartiesError::NoAccount`]; [`PartiesError::Mls`];
    /// [`PartiesError::Store`].
    pub fn prepare_join(
        &self,
        link: &InviteLink,
        details: JoinDetails,
        rng: &mut impl CryptoRng,
    ) -> Result<PreparedJoin, PartiesError> {
        let now = self.clock.now();
        let payload = link.payload()?;
        if now >= payload.expires {
            return Err(PartiesError::InviteExpired);
        }
        self.session.transaction(|session| {
            let identity = identity(session)?;
            let reply = session
                .mailboxes()
                .take_pooled(MailboxPurpose::Reply, now)?
                .ok_or(PartiesError::NoPooledMailbox)?;
            let context = MlsContext::new(session, &identity.signing_key);
            // The link does not say which suite the Party uses (see CONTRACT.md).
            let key_package = context.create_key_package(
                KeyPackagePurpose::Party,
                CipherSuitePolicy::Classical,
                rng,
            )?;
            let reply_mailbox = reply.id();
            let request = JoinRequest {
                key_package,
                display_name: details.display_name,
                reply_mailbox,
                party_subscription: details.subscription,
            };
            let secret = InviteSecret::from_bytes(Zeroizing::new(*payload.invite_secret.expose()));
            let sealed = seal_join_request(&secret, &request.encode()?, rng)?;
            let joining = Joining {
                payload: payload.clone(),
                name: request.display_name,
                subscription: request.party_subscription.id,
            };
            records::write(session, &Key::Joining(&reply_mailbox), &joining.encode()?)?;
            Ok(PreparedJoin {
                join: JoinId::new(reply_mailbox),
                outbound: Outbound::MailboxPost {
                    mailbox: payload.invite_mailbox,
                    body: Padded::from_wire(sealed)?,
                },
            })
        })
    }

    /// Joiner: the group and log-auth key a Welcome in the reply mailbox gives, so pingers-sync
    /// can fetch the group's state snapshot for [`Self::accept_welcome`]. Nothing is kept: the
    /// Welcome is processed in a transaction that is rolled back.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownJoin`]; [`PartiesError::Mls`] (a Welcome not for this device);
    /// [`PartiesError::Proto`] (a post that is not a padded Welcome); [`PartiesError::Store`].
    pub fn welcome_access(
        &self,
        join: JoinId,
        welcome: &Padded,
    ) -> Result<WelcomeAccess, PartiesError> {
        let mut access = None;
        let outcome = self.session.transaction(|session| {
            records::read(session, &Key::Joining(&join.reply_mailbox()))?
                .ok_or(PartiesError::UnknownJoin)?;
            let identity = identity(session)?;
            let context = MlsContext::new(session, &identity.signing_key);
            let welcome = WelcomeBytes::from_bytes(welcome.payload()?.to_vec());
            let group = context.join_party_group(&welcome)?;
            access = Some(WelcomeAccess {
                group: group.group_id(),
                auth: group.log_auth_key_pair()?,
            });
            // Roll back: the join happens in `accept_welcome`, with the snapshot.
            Err::<(), _>(PartiesError::Internal)
        });
        match (access, outcome) {
            (Some(access), Err(PartiesError::Internal)) => Ok(access),
            (_, Err(error)) => Err(error),
            (_, Ok(())) => Err(PartiesError::Internal),
        }
    }

    /// Joiner: the Welcome arrived in the reply mailbox and pingers-sync fetched the group's
    /// state snapshot. Joins the group, checks that it contains the inviter (the link's
    /// fingerprint), loads the snapshot (Party, members' names, Spots) and publishes this
    /// device's roster profile. If the inviter is missing nothing is kept.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownJoin`]; [`PartiesError::InviterNotInGroup`];
    /// [`PartiesError::Mls`] (a Welcome not for this device); [`PartiesError::Crypto`] (a
    /// snapshot not sealed for the joined epoch); [`PartiesError::Malformed`];
    /// [`PartiesError::Store`].
    pub fn accept_welcome(
        &self,
        arrived: &ArrivedWelcome,
        rng: &mut impl CryptoRng,
    ) -> Result<Joined, PartiesError> {
        let now = self.clock.now();
        self.session.transaction(|session| {
            let reply = arrived.join.reply_mailbox();
            let joining = Joining::decode(
                &records::read(session, &Key::Joining(&reply))?.ok_or(PartiesError::UnknownJoin)?,
            )?;
            let identity = identity(session)?;
            let own_key = identity.signing_key.public_key();
            let context = MlsContext::new(session, &identity.signing_key);
            let welcome = WelcomeBytes::from_bytes(arrived.welcome.payload()?.to_vec());
            let mut group = context.join_party_group(&welcome)?;
            let roster = group.roster();
            // An error rolls the whole transaction back, the joined group included.
            if !roster.entries().iter().any(|entry| {
                inviter_fingerprint(&entry.signature_key) == joining.payload.inviter_fingerprint
            }) {
                return Err(PartiesError::InviterNotInGroup);
            }
            let opened = open_state_snapshot(&group.state_key()?, &arrived.snapshot)?;
            let snapshot = Snapshot::decode(&opened)?;
            let party = snapshot.party;
            let own_name = MemberName::new(joining.name.as_str().to_owned())?;
            session.parties().insert(&Party {
                id: party,
                group: group.group_id(),
                name: snapshot.name,
                own_leaf: group.own_leaf(),
                created_at: now,
                location_visibility: None,
            })?;
            session.members().put(&Member {
                party,
                leaf: group.own_leaf(),
                signature_key: own_key,
                name: own_name.clone(),
                avatar: None,
                verification: Verification::Unverified,
            })?;
            // This device's own name is the one it gave, whatever the snapshot says.
            for profile in snapshot
                .profiles
                .into_iter()
                .filter(|profile| profile.key != own_key)
            {
                let leaf = roster
                    .entries()
                    .iter()
                    .find(|entry| entry.signature_key == profile.key)
                    .map(|entry| entry.leaf);
                if let Some(leaf) = leaf {
                    session.members().put(&Member {
                        party,
                        leaf,
                        signature_key: profile.key,
                        name: profile.name,
                        avatar: None,
                        verification: Verification::Unverified,
                    })?;
                }
            }
            for spot in &snapshot.spots {
                session.spots().put(spot)?;
            }
            let settings = StoredSettings {
                server: joining.payload.server.clone(),
                relays: RelayChoice::ServerDefault,
                post_quantum: PostQuantum::Classical,
            };
            records::write(session, &Key::Settings(&party), &settings.encode()?)?;
            records::record_roster(session, &party, group.epoch(), &roster)?;
            set_subscription(session, &party, own_key, Some(joining.subscription))?;
            records::delete(session, &Key::Joining(&reply))?;
            let profile = seal_operation(
                &mut group,
                &PartyOperation::Profile {
                    name: own_name,
                    subscription: Some(joining.subscription),
                },
                rng,
            )?;
            Ok(Joined {
                party,
                outbound: vec![profile, Outbound::DeleteMailbox { mailbox: reply }],
            })
        })
    }
}

/// Commits the Add for an admitted join request.
fn admit(
    session: &Session,
    party: &PartyId,
    approval: Approval,
    rng: &mut impl CryptoRng,
) -> Result<OutgoingCommit, PartiesError> {
    let (row, identity) = start_commit(session, party)?;
    let context = MlsContext::new(session, &identity.signing_key);
    let mut group = context.party_group(&row.group)?;
    let epoch = group.epoch();
    let commit = group.add_members(core::slice::from_ref(&approval.request.key_package), rng)?;
    let added = Some((
        approval.request.party_subscription.clone(),
        approval.request.reply_mailbox,
    ));
    finish_commit(
        session,
        &group,
        CommitParts {
            row: &row,
            epoch,
            commit,
            pending: &PendingCommit::Add(Box::new(approval)),
            excluded: &[],
            added,
            removed: Vec::new(),
        },
        rng,
    )
}

fn invite_party(session: &Session, mailbox: &MailboxId) -> Result<PartyId, PartiesError> {
    let bytes =
        records::read(session, &Key::InviteIndex(mailbox))?.ok_or(PartiesError::UnknownInvite)?;
    let mut reader = Reader::new(&bytes, "invite index");
    let party = PartyId::from_bytes(reader.fixed()?);
    reader.end()?;
    Ok(party)
}

fn invite_record(
    session: &Session,
    party: &PartyId,
    mailbox: &MailboxId,
) -> Result<InviteRecord, PartiesError> {
    let bytes =
        records::read(session, &Key::Invite(party, mailbox))?.ok_or(PartiesError::UnknownInvite)?;
    InviteRecord::decode(&bytes)
}

/// A join request and the invite it came through.
pub(crate) struct Approval {
    pub(crate) invite: MailboxId,
    pub(crate) request: JoinRequest,
}

/// Files a join request for approval, by the joiner's reply mailbox.
pub(crate) fn store_approval(
    session: &Session,
    party: &PartyId,
    approval: &Approval,
) -> Result<(), PartiesError> {
    let reply = approval.request.reply_mailbox;
    let mut writer = Writer::new();
    writer.fixed(approval.invite.as_bytes());
    writer.variable(&approval.request.encode()?)?;
    records::write(session, &Key::Approval(party, &reply), &writer.finish())?;
    records::add_to_list(session, &Key::Approvals(party), reply)
}

fn load_approval(
    session: &Session,
    party: &PartyId,
    reply: &MailboxId,
) -> Result<Approval, PartiesError> {
    let bytes =
        records::read(session, &Key::Approval(party, reply))?.ok_or(PartiesError::UnknownJoin)?;
    let mut reader = Reader::new(&bytes, "join approval");
    let invite = MailboxId::from_bytes(reader.fixed()?);
    let request = JoinRequest::decode(reader.variable()?)?;
    reader.end()?;
    Ok(Approval { invite, request })
}

fn drop_approval(
    session: &Session,
    party: &PartyId,
    reply: &MailboxId,
) -> Result<(), PartiesError> {
    records::delete(session, &Key::Approval(party, reply))?;
    records::remove_from_list(session, &Key::Approvals(party), reply)
}

/// A join this device started and is waiting to complete.
struct Joining {
    payload: InvitePayload,
    name: DisplayName,
    subscription: pingers_proto::SubscriptionId,
}

impl Joining {
    fn encode(&self) -> Result<Zeroizing<Vec<u8>>, PartiesError> {
        let mut writer = Writer::new();
        writer.variable(&Zeroizing::new(self.payload.encode()?))?;
        writer.variable(self.name.as_str().as_bytes())?;
        writer.fixed(self.subscription.as_bytes());
        Ok(Zeroizing::new(writer.finish()))
    }

    fn decode(bytes: &[u8]) -> Result<Self, PartiesError> {
        let mut reader = Reader::new(bytes, "join record");
        let payload = InvitePayload::decode(reader.variable()?)?;
        let name = DisplayName::new(reader.text()?)?;
        let subscription = pingers_proto::SubscriptionId::from_bytes(reader.fixed()?);
        reader.end()?;
        Ok(Self {
            payload,
            name,
            subscription,
        })
    }
}
