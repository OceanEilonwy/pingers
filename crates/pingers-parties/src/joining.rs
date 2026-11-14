// SPDX-License-Identifier: AGPL-3.0-only
//! Invites (inviter side) and joins (joiner side): design D invite flow, design G.1, join
//! flow A.
//!
//! Flow A: the link carries nothing about the Party. The joiner asks to join and both phones
//! show the same 12-digit join code at once (pingers-crypto's `join_code`, over the invite
//! secret and the joiner's key package). The joiner reads it out; the inviter lets them in
//! only if it matches, and the link then stops working. The joiner learns the Party's name,
//! members and Spots from the Welcome and the state snapshot.

use pingers_crypto::{
    InviteSecret, JoinCode, JoinKeyPackages, LogAuthKeyPair, join_code, open_join_request,
    open_state_snapshot, seal_join_request,
};
use pingers_mls::{CipherSuitePolicy, KeyPackagePurpose, MlsContext, MlsError};
use pingers_proto::{
    DisplayName, GroupId, InvitePayload, InvitePost, JoinRequest, KeyPackageBytes, LeafIndex,
    MailboxId, Padded, PartyId, SealedSnapshot, SecretBytes, SecretText, SequenceNumber,
    Subscription, SubscriptionId, WelcomeBytes, WireDecode as _, WireEncode as _,
};
use pingers_store::{MailboxPurpose, Member, MemberName, Party, Session, Verification};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::codec::{Reader, Writer};
use crate::invite::{InviteRecord, StoredStatus};
use crate::lifecycle::{
    Added, CommitParts, PendingCommit, finish_commit, identity, party_row, seal_operation,
    set_subscription, start_commit, stored_settings,
};
use crate::ops::{PartyOperation, Snapshot};
use crate::records::{self, Key};
use crate::settings::StoredSettings;
use crate::{
    INVITE_LIFETIME, Ignored, InviteId, InviteLink, InviteStatus, InviteSummary, JoinId, Outbound,
    OutgoingCommit, PartiesError, PartyLifecycle, PendingJoin, PostQuantum, Refusal, RelayChoice,
    inviter_fingerprint,
};

/// What an invite is made with.
pub struct NewInvite {
    /// The operator's registration code this device registered with, passed on in the link
    /// (pingers-sync's `shared_registration_code`); `None` for a server without codes.
    pub registration_code: Option<SecretText>,
}

/// A new invite.
pub struct CreatedInvite {
    /// The invite, for its status and for cancelling it.
    pub invite: InviteId,
    /// The link (and QR code text) to hand to the joiner out of band.
    pub link: InviteLink,
    /// When it stops accepting join requests.
    pub expires: UnixSeconds,
}

/// What became of a post to an invite mailbox.
#[derive(Debug, PartialEq, Eq)]
pub enum JoinRequestOutcome {
    /// Someone asked to join: show "Ben asked to join" with the code.
    AwaitingApproval(PendingJoin),
    /// The joiner withdrew their waiting request.
    Withdrawn(JoinId),
    /// Turned away without asking the inviter; nothing to send.
    Refused(Refusal),
    /// Nothing changed.
    Ignored(Ignored),
}

/// Letting someone in: the commit that adds them (post it, then its Welcome), and the
/// invite mailbox's deletion, since the link stops working.
pub struct Admission {
    /// The Add commit, with the Welcome for the joiner's reply mailbox.
    pub commit: OutgoingCommit,
    /// [`Outbound::DeleteMailbox`] for the invite's mailbox.
    pub close_invite: Outbound,
}

/// What the joiner supplies besides the link.
pub struct JoinDetails {
    /// The name to show the Party.
    pub display_name: DisplayName,
    /// The joiner's sealed-push subscription for the Party group (`party_sub`), or `None`
    /// with wake-up pushes off.
    pub subscription: Option<Subscription>,
}

/// A join request ready to post.
pub struct PreparedJoin {
    /// The join, for [`PartyLifecycle::accept_welcome`] when the Welcome arrives in its
    /// reply mailbox.
    pub join: JoinId,
    /// The code to read to the inviter.
    pub code: JoinCode,
    /// [`Outbound::MailboxPost`] to the invite mailbox.
    pub outbound: Outbound,
}

/// A join this device asked for and is waiting on ("Asked to join").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AskedJoin {
    /// The join.
    pub join: JoinId,
    /// The code to read to the inviter.
    pub code: JoinCode,
    /// When this device asked.
    pub asked_at: UnixSeconds,
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

/// How this device came into a Party it joined: who let it in, and the code that was checked
/// ("Ana let you in"; the Party's security details).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinedVia {
    /// The inviter's leaf.
    pub inviter: LeafIndex,
    /// The code both phones showed.
    pub code: JoinCode,
}

/// A Party this device has joined.
pub struct Joined {
    /// The Party.
    pub party: PartyId,
    /// Who let it in and the code that was checked.
    pub via: JoinedVia,
    /// This device's roster profile for the group, and the reply mailbox's deletion.
    pub outbound: Vec<Outbound>,
}

impl PartyLifecycle<'_> {
    /// Creates an invite: takes a pooled invite mailbox (registered by pingers-sync with an
    /// alert-urgency sealed push), draws the invite secret, and returns the link. The invite
    /// accepts join requests for 24 hours, or until the mailbox expires if that is sooner,
    /// and stops once someone is let in.
    ///
    /// A registration code in `invite` goes into the link (design B.1), so the joiner's first
    /// registration on this server works without typing one.
    ///
    /// # Errors
    ///
    /// [`PartiesError::NoPooledMailbox`] if pingers-sync has to register an invite mailbox
    /// first; [`PartiesError::UnknownParty`]; [`PartiesError::NoAccount`];
    /// [`PartiesError::Store`].
    pub fn create_invite(
        &self,
        party: &PartyId,
        invite: NewInvite,
        rng: &mut impl CryptoRng,
    ) -> Result<CreatedInvite, PartiesError> {
        let NewInvite { registration_code } = invite;
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
                shared_at: now,
                expires,
                status: StoredStatus::Open,
                read_position: None,
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
                registration_code,
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
                    shared_at: record.shared_at,
                    expires: record.expires,
                    status: record.status_at(now),
                })
            })
            .collect()
    }

    /// Where to resume reading an invite's mailbox: after the last post already handed to
    /// [`Self::receive_join_request`] (`None`: from the start).
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownInvite`]; [`PartiesError::Malformed`].
    pub fn invite_read_position(
        &self,
        invite: InviteId,
    ) -> Result<Option<SequenceNumber>, PartiesError> {
        let party = invite_party(self.session, &invite.mailbox())?;
        Ok(invite_record(self.session, &party, &invite.mailbox())?.read_position)
    }

    /// Cancels an invite: no further join requests are accepted, the ones waiting are dropped,
    /// and the mailbox is deleted on the server ("a link is a bearer capability: revoke by
    /// deleting the invite mailbox").
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
            drop_approvals_of(session, &party, &mailbox)?;
            Ok(Outbound::DeleteMailbox { mailbox })
        })
    }

    /// Handles post `sequence` of an invite's mailbox. Posts are read in order and each is
    /// handled once: the invite's read position moves past it whatever it holds.
    ///
    /// A join request to an open invite waits for [`Self::approve_join`] with its join code;
    /// one to an expired, used or cancelled invite is refused. A withdrawal drops the waiting
    /// request it names. A post that does not open under the invite's secret is ignored.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownInvite`]; [`PartiesError::Crypto`] if the join code cannot be
    /// derived; [`PartiesError::Store`].
    pub fn receive_join_request(
        &self,
        invite: InviteId,
        sequence: SequenceNumber,
        post: &Padded,
    ) -> Result<JoinRequestOutcome, PartiesError> {
        let now = self.clock.now();
        self.session.transaction(|session| {
            let mailbox = invite.mailbox();
            let party = invite_party(session, &mailbox)?;
            let mut record = invite_record(session, &party, &mailbox)?;
            if record
                .read_position
                .is_some_and(|read| sequence.get() <= read.get())
            {
                return Ok(JoinRequestOutcome::Ignored(Ignored::AlreadySeen));
            }
            record.read_position = Some(sequence);
            records::write(session, &Key::Invite(&party, &mailbox), &record.encode())?;
            let secret = InviteSecret::from_bytes(Zeroizing::new(*record.secret));
            let Some(post) = open_post(&secret, post) else {
                return Ok(JoinRequestOutcome::Ignored(Ignored::Unreadable));
            };
            let request = match post {
                InvitePost::Withdraw { reply_mailbox } => {
                    return withdraw(session, &party, reply_mailbox);
                }
                InvitePost::Join(request) => *request,
            };
            match record.status_at(now) {
                InviteStatus::Open => {}
                InviteStatus::Used => return Ok(JoinRequestOutcome::Refused(Refusal::AlreadyUsed)),
                InviteStatus::Expired => return Ok(JoinRequestOutcome::Refused(Refusal::Expired)),
                InviteStatus::Revoked => return Ok(JoinRequestOutcome::Refused(Refusal::Revoked)),
            }
            if stored_settings(session, &party)?.post_quantum == PostQuantum::QuantumResistant
                && request.quantum_resistant_key_package.is_none()
            {
                return Ok(JoinRequestOutcome::Refused(
                    Refusal::NoQuantumResistantKeyPackage,
                ));
            }
            let pending = PendingJoin {
                join: JoinId::new(request.reply_mailbox),
                invite,
                name: request.display_name.clone(),
                code: join_code(&secret, key_packages(&request))?,
                asked_at: now,
            };
            store_approval(
                session,
                &party,
                &Approval {
                    invite: mailbox,
                    asked_at: now,
                    request,
                },
            )?;
            Ok(JoinRequestOutcome::AwaitingApproval(pending))
        })
    }

    /// Join requests waiting for the inviter in this Party, in the order they arrived.
    ///
    /// # Errors
    ///
    /// [`PartiesError::Malformed`] if a record is corrupt; [`PartiesError::Crypto`];
    /// [`PartiesError::Store`].
    pub fn pending_joins(&self, party: &PartyId) -> Result<Vec<PendingJoin>, PartiesError> {
        records::read_mailbox_list(self.session, &Key::Approvals(party))?
            .into_iter()
            .map(|reply| {
                let approval = load_approval(self.session, party, &reply)?;
                pending_join(self.session, party, approval)
            })
            .collect()
    }

    /// Lets in a waiting joiner, once the codes matched: commits the Add and closes the
    /// invite (the link stops working, and the other requests through it are dropped). The
    /// invite must not have expired or been cancelled since.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownJoin`]; [`PartiesError::InviteExpired`] if the invite expired,
    /// was cancelled or let someone else in since; [`PartiesError::CommitInFlight`];
    /// [`PartiesError::Mls`]; [`PartiesError::Store`].
    pub fn approve_join(
        &self,
        party: &PartyId,
        join: JoinId,
        rng: &mut impl CryptoRng,
    ) -> Result<Admission, PartiesError> {
        let now = self.clock.now();
        self.session.transaction(|session| {
            let reply = join.reply_mailbox();
            let approval = load_approval(session, party, &reply)?;
            let invite = approval.invite;
            let mut record = invite_record(session, party, &invite)?;
            match (record.status_at(now), record.status) {
                // A rejected commit puts its request back while the invite stays used by it.
                (InviteStatus::Open, _) => {}
                (InviteStatus::Used, StoredStatus::Used(admitted)) if admitted == reply => {}
                _ => return Err(PartiesError::InviteExpired),
            }
            record.status = StoredStatus::Used(reply);
            records::write(session, &Key::Invite(party, &invite), &record.encode())?;
            drop_approvals_of(session, party, &invite)?;
            let commit = admit(session, party, approval, rng)?;
            Ok(Admission {
                commit,
                close_invite: Outbound::DeleteMailbox { mailbox: invite },
            })
        })
    }

    /// Turns away a waiting join request ("Don't let in"); nothing is sent, and the joiner
    /// never hears back.
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

    /// Joiner: builds the join request for a link: a fresh `KeyPackage`, the display name, a
    /// pooled reply mailbox and, if given, the Party subscription, sealed under the invite
    /// secret and padded to a mailbox size class. Returns the join code to read out.
    ///
    /// # Errors
    ///
    /// [`PartiesError::InvalidLink`] or [`PartiesError::Proto`] for a bad link;
    /// [`PartiesError::InviteExpired`]; [`PartiesError::NoPooledMailbox`] if pingers-sync has
    /// to register a reply mailbox first; [`PartiesError::NoAccount`]; [`PartiesError::Mls`];
    /// [`PartiesError::Crypto`]; [`PartiesError::Store`].
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
            // The link does not say which suite the Party uses, so the request carries a classical
            // key package and, where this build can make one, an X-Wing one.
            let key_package = context.create_key_package(
                KeyPackagePurpose::Party,
                CipherSuitePolicy::Classical,
                rng,
            )?;
            let quantum_resistant = match context.create_key_package(
                KeyPackagePurpose::Party,
                CipherSuitePolicy::QuantumResistant,
                rng,
            ) {
                Ok(key_package) => Some(key_package),
                Err(MlsError::UnsupportedSuite) => None,
                Err(error) => return Err(error.into()),
            };
            let reply_mailbox = reply.id();
            let secret = InviteSecret::from_bytes(Zeroizing::new(*payload.invite_secret.expose()));
            let subscription = details
                .subscription
                .as_ref()
                .map(|subscription| subscription.id);
            let request = JoinRequest {
                key_package,
                quantum_resistant_key_package: quantum_resistant,
                display_name: details.display_name.clone(),
                reply_mailbox,
                party_subscription: details.subscription,
            };
            let code = join_code(&secret, key_packages(&request))?;
            let key_package = request.key_package.clone();
            let quantum_resistant = request.quantum_resistant_key_package.clone();
            let post = InvitePost::Join(Box::new(request));
            let sealed = seal_join_request(&secret, &post.encode()?, rng)?;
            let joining = Joining {
                payload: payload.clone(),
                name: details.display_name,
                subscription,
                key_package,
                quantum_resistant,
                asked_at: now,
            };
            records::write(session, &Key::Joining(&reply_mailbox), &joining.encode()?)?;
            Ok(PreparedJoin {
                join: JoinId::new(reply_mailbox),
                code,
                outbound: Outbound::MailboxPost {
                    mailbox: payload.invite_mailbox,
                    body: Padded::from_wire(sealed)?,
                },
            })
        })
    }

    /// Joiner: the join this device asked for, with its code ("Asked to join").
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownJoin`]; [`PartiesError::Malformed`]; [`PartiesError::Crypto`].
    pub fn asked_join(&self, join: JoinId) -> Result<AskedJoin, PartiesError> {
        let joining = load_joining(self.session, join)?;
        Ok(AskedJoin {
            join,
            code: joining.code()?,
            asked_at: joining.asked_at,
        })
    }

    /// Joiner: withdraws the request ("Withdraw request"): posts the withdrawal to the
    /// invite mailbox, deletes the reply mailbox and forgets the join and its key package.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownJoin`]; [`PartiesError::Mls`]; [`PartiesError::Crypto`];
    /// [`PartiesError::Store`].
    pub fn withdraw_join(
        &self,
        join: JoinId,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<Outbound>, PartiesError> {
        self.session.transaction(|session| {
            let reply = join.reply_mailbox();
            let joining = load_joining(session, join)?;
            let identity = identity(session)?;
            MlsContext::new(session, &identity.signing_key)
                .delete_key_package(&joining.key_package)?;
            if let Some(key_package) = &joining.quantum_resistant {
                MlsContext::new(session, &identity.signing_key).delete_key_package(key_package)?;
            }
            records::delete(session, &Key::Joining(&reply))?;
            let secret =
                InviteSecret::from_bytes(Zeroizing::new(*joining.payload.invite_secret.expose()));
            let post = InvitePost::Withdraw {
                reply_mailbox: reply,
            };
            let sealed = seal_join_request(&secret, &post.encode()?, rng)?;
            Ok(vec![
                Outbound::MailboxPost {
                    mailbox: joining.payload.invite_mailbox,
                    body: Padded::from_wire(sealed)?,
                },
                Outbound::DeleteMailbox { mailbox: reply },
            ])
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
    /// fingerprint), loads the snapshot (Party, members' names, Spots), records who let this
    /// device in with the code, and publishes this device's roster profile. If the inviter is
    /// missing nothing is kept.
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
            let joining = load_joining(session, arrived.join)?;
            let code = joining.code()?;
            let identity = identity(session)?;
            let own_key = identity.signing_key.public_key();
            let context = MlsContext::new(session, &identity.signing_key);
            let welcome = WelcomeBytes::from_bytes(arrived.welcome.payload()?.to_vec());
            let mut group = context.join_party_group(&welcome)?;
            let roster = group.roster();
            // An error rolls the whole transaction back, the joined group included.
            let inviter = roster
                .entries()
                .iter()
                .find(|entry| {
                    inviter_fingerprint(&entry.signature_key) == joining.payload.inviter_fingerprint
                })
                .ok_or(PartiesError::InviterNotInGroup)?
                .leaf;
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
                    set_subscription(session, &party, profile.key, profile.subscription)?;
                }
            }
            for spot in &snapshot.spots {
                session.spots().put(spot)?;
            }
            let settings = StoredSettings {
                server: joining.payload.server.clone(),
                relays: RelayChoice::ServerDefault,
                post_quantum: PostQuantum::of_suite(group.suite()?)?,
            };
            settings.write(session, &party)?;
            records::record_roster(session, &party, group.epoch(), &roster)?;
            set_subscription(session, &party, own_key, joining.subscription)?;
            records::delete(session, &Key::Joining(&reply))?;
            let via = JoinedVia { inviter, code };
            records::write(session, &Key::JoinedVia(&party), &encode_via(via))?;
            let profile = seal_operation(
                &mut group,
                &PartyOperation::Profile {
                    name: own_name,
                    subscription: joining.subscription,
                },
                rng,
            )?;
            Ok(Joined {
                party,
                via,
                outbound: vec![profile, Outbound::DeleteMailbox { mailbox: reply }],
            })
        })
    }

    /// Who let this device into a Party it joined, and the code that was checked; `None` for
    /// a Party it created.
    ///
    /// # Errors
    ///
    /// [`PartiesError::Malformed`]; [`PartiesError::Store`].
    pub fn joined_via(&self, party: &PartyId) -> Result<Option<JoinedVia>, PartiesError> {
        records::read(self.session, &Key::JoinedVia(party))?
            .map(|bytes| decode_via(&bytes))
            .transpose()
    }
}

/// The key packages a request carries, for its join code.
fn key_packages(request: &JoinRequest) -> JoinKeyPackages<'_> {
    JoinKeyPackages {
        classical: request.key_package.as_bytes(),
        quantum_resistant: request
            .quantum_resistant_key_package
            .as_ref()
            .map(KeyPackageBytes::as_bytes),
    }
}

/// Opens and decodes an invite post; `None` if it is not from a holder of the link.
fn open_post(secret: &InviteSecret, post: &Padded) -> Option<InvitePost> {
    let plaintext = open_join_request(secret, post.as_bytes()).ok()?;
    InvitePost::decode(&plaintext).ok()
}

/// Drops the waiting request whose Welcome would go to `reply`, if there is one.
fn withdraw(
    session: &Session,
    party: &PartyId,
    reply: MailboxId,
) -> Result<JoinRequestOutcome, PartiesError> {
    if records::read(session, &Key::Approval(party, &reply))?.is_none() {
        return Ok(JoinRequestOutcome::Ignored(Ignored::NotPending));
    }
    drop_approval(session, party, &reply)?;
    Ok(JoinRequestOutcome::Withdrawn(JoinId::new(reply)))
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
    // The key package of the Party's suite; a request without the one a quantum-resistant
    // Party needs was refused when it arrived.
    let key_package = match stored_settings(session, party)?.post_quantum {
        PostQuantum::Classical => &approval.request.key_package,
        PostQuantum::QuantumResistant => approval
            .request
            .quantum_resistant_key_package
            .as_ref()
            .ok_or(PartiesError::Internal)?,
    };
    let commit = group.add_members(core::slice::from_ref(key_package), rng)?;
    let added = Some(Added {
        subscription: approval.request.party_subscription.clone(),
        reply: approval.request.reply_mailbox,
    });
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

/// A join request, the invite it came through and when it was read.
pub(crate) struct Approval {
    pub(crate) invite: MailboxId,
    pub(crate) asked_at: UnixSeconds,
    pub(crate) request: JoinRequest,
}

impl Approval {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, PartiesError> {
        let mut writer = Writer::new();
        writer.fixed(self.invite.as_bytes());
        writer.u64(self.asked_at.get());
        writer.variable(&self.request.encode()?)?;
        Ok(writer.finish())
    }

    pub(crate) fn read(reader: &mut Reader<'_>) -> Result<Self, PartiesError> {
        Ok(Self {
            invite: MailboxId::from_bytes(reader.fixed()?),
            asked_at: UnixSeconds::new(reader.u64()?),
            request: JoinRequest::decode(reader.variable()?)?,
        })
    }
}

/// Files a join request for approval, by the joiner's reply mailbox. A second request with
/// the same reply mailbox replaces the first.
pub(crate) fn store_approval(
    session: &Session,
    party: &PartyId,
    approval: &Approval,
) -> Result<(), PartiesError> {
    let reply = approval.request.reply_mailbox;
    records::write(session, &Key::Approval(party, &reply), &approval.encode()?)?;
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
    let approval = Approval::read(&mut reader)?;
    reader.end()?;
    Ok(approval)
}

/// A stored request as the inviter's screen shows it, with its code.
fn pending_join(
    session: &Session,
    party: &PartyId,
    approval: Approval,
) -> Result<PendingJoin, PartiesError> {
    let record = invite_record(session, party, &approval.invite)?;
    let secret = InviteSecret::from_bytes(Zeroizing::new(*record.secret));
    Ok(PendingJoin {
        join: JoinId::new(approval.request.reply_mailbox),
        invite: InviteId::for_mailbox(approval.invite),
        code: join_code(&secret, key_packages(&approval.request))?,
        name: approval.request.display_name,
        asked_at: approval.asked_at,
    })
}

fn drop_approval(
    session: &Session,
    party: &PartyId,
    reply: &MailboxId,
) -> Result<(), PartiesError> {
    records::delete(session, &Key::Approval(party, reply))?;
    records::remove_from_list(session, &Key::Approvals(party), reply)
}

/// Drops the waiting requests that came through `invite`.
fn drop_approvals_of(
    session: &Session,
    party: &PartyId,
    invite: &MailboxId,
) -> Result<(), PartiesError> {
    for reply in records::read_mailbox_list(session, &Key::Approvals(party))? {
        if load_approval(session, party, &reply)?.invite == *invite {
            drop_approval(session, party, &reply)?;
        }
    }
    Ok(())
}

fn encode_via(via: JoinedVia) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.u32(via.inviter.get());
    writer.u64(via.code.value());
    writer.finish()
}

fn decode_via(bytes: &[u8]) -> Result<JoinedVia, PartiesError> {
    let mut reader = Reader::new(bytes, "joined via");
    let inviter = LeafIndex::new(reader.u32()?);
    let code = JoinCode::from_value(reader.u64()?).ok_or_else(|| reader.malformed())?;
    reader.end()?;
    Ok(JoinedVia { inviter, code })
}

/// A join this device started and is waiting to complete.
struct Joining {
    payload: InvitePayload,
    name: DisplayName,
    subscription: Option<SubscriptionId>,
    key_package: KeyPackageBytes,
    quantum_resistant: Option<KeyPackageBytes>,
    asked_at: UnixSeconds,
}

fn load_joining(session: &Session, join: JoinId) -> Result<Joining, PartiesError> {
    Joining::decode(
        &records::read(session, &Key::Joining(&join.reply_mailbox()))?
            .ok_or(PartiesError::UnknownJoin)?,
    )
}

impl Joining {
    fn code(&self) -> Result<JoinCode, PartiesError> {
        let secret = InviteSecret::from_bytes(Zeroizing::new(*self.payload.invite_secret.expose()));
        let key_packages = JoinKeyPackages {
            classical: self.key_package.as_bytes(),
            quantum_resistant: self
                .quantum_resistant
                .as_ref()
                .map(KeyPackageBytes::as_bytes),
        };
        Ok(join_code(&secret, key_packages)?)
    }

    fn encode(&self) -> Result<Zeroizing<Vec<u8>>, PartiesError> {
        let mut writer = Writer::new();
        writer.variable(&Zeroizing::new(self.payload.encode()?))?;
        writer.variable(self.name.as_str().as_bytes())?;
        match self.subscription {
            None => writer.byte(0),
            Some(subscription) => {
                writer.byte(1);
                writer.fixed(subscription.as_bytes());
            }
        }
        writer.variable(self.key_package.as_bytes())?;
        match &self.quantum_resistant {
            None => writer.byte(0),
            Some(key_package) => {
                writer.byte(1);
                writer.variable(key_package.as_bytes())?;
            }
        }
        writer.u64(self.asked_at.get());
        Ok(Zeroizing::new(writer.finish()))
    }

    fn decode(bytes: &[u8]) -> Result<Self, PartiesError> {
        let mut reader = Reader::new(bytes, "join record");
        let payload = InvitePayload::decode(reader.variable()?)?;
        let name = DisplayName::new(reader.text()?)?;
        let subscription = match reader.byte()? {
            0 => None,
            1 => Some(SubscriptionId::from_bytes(reader.fixed()?)),
            _ => return Err(reader.malformed()),
        };
        let key_package = KeyPackageBytes::from_bytes(reader.variable()?.to_vec());
        let quantum_resistant = match reader.byte()? {
            0 => None,
            1 => Some(KeyPackageBytes::from_bytes(reader.variable()?.to_vec())),
            _ => return Err(reader.malformed()),
        };
        let asked_at = UnixSeconds::new(reader.u64()?);
        reader.end()?;
        Ok(Self {
            payload,
            name,
            subscription,
            key_package,
            quantum_resistant,
            asked_at,
        })
    }
}
