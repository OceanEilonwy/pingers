// SPDX-License-Identifier: AGPL-3.0-only
//! Party and Meetup groups.

use core::marker::PhantomData;

use openmls::prelude::tls_codec::DeserializeBytes as _;
use openmls::prelude::{
    CommitBuilder, Initial, KeyPackage, KeyPackageIn, LeafNodeIndex, MlsGroup, MlsMessageBodyIn,
    MlsMessageIn, PreSharedKeyProposal, ProcessedMessageContent, ProcessedWelcome, Proposal,
    ProtocolMessage, ProtocolVersion, Sender, StagedCommit, StagedWelcome, Welcome, WelcomeError,
};
use openmls::schedule::{ExternalPsk, PreSharedKeyId, Psk};
use openmls_traits::OpenMlsProvider as _;
use openmls_traits::storage::StorageProvider as _;
use pingers_crypto::{
    AccountPublicKey, AccountSigningKey, ExporterLabel, GrantPublicKey, LogAuthKeyPair, MeetupPsk,
    PresenceRoot, StateKey,
};
use pingers_proto::{
    Epoch, GroupId, KeyPackageBytes, LeafIndex, MlsMessageBytes, Padded, PartyId, SpotId,
    WelcomeBytes,
};
use pingers_time::Bucket;
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::policy::{self, KindTag};
use crate::provider::{InjectedRand, NoRand, Provider, credential, leaf_signer};
use crate::{MlsStorage, Roster, RosterEntry, StorageError};

/// Why an MLS operation failed. Never carries message contents or key material.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MlsError {
    /// The storage backend failed.
    #[error("MLS storage failed")]
    Storage(#[from] StorageError),
    /// A KeyPackage or commit adds a leaf whose signature key is not in the Party roster
    /// (design B.5). The whole commit is rejected.
    #[error("added member is not in the Party roster")]
    NotInPartyRoster,
    /// A Meetup Welcome or commit does not carry the expected external PSK.
    #[error("meetup PSK does not match")]
    PskMismatch,
    /// A message is malformed, or is not a `PrivateMessage` for this group.
    #[error("invalid MLS message")]
    InvalidMessage,
    /// A message is for an epoch this member cannot process (too old, or from the future).
    #[error("message is for epoch {message:?}, group is at {group:?}")]
    WrongEpoch {
        /// The message's epoch.
        message: Epoch,
        /// The group's epoch.
        group: Epoch,
    },
    /// The group is not in storage, or this device is no longer a member.
    #[error("not a member of this group")]
    NotAMember,
    /// The ciphersuite is not available in this build (X-Wing without the `x-wing` feature).
    #[error("ciphersuite not available")]
    UnsupportedSuite,
    /// A commit is pending; merge or discard it before creating another. Also returned when
    /// sealing while received proposals wait to be committed
    /// ([`Group::commit_pending_proposals`]).
    #[error("a commit is already pending")]
    CommitPending,
    /// OpenMLS reported an error the wrapper does not map more precisely.
    #[error("internal MLS error")]
    Internal,
    /// This part of the contract has no implementation yet. Never returned now that every
    /// function is implemented; kept so the contract's variant list stays stable.
    #[error("not implemented")]
    NotImplemented,
}

/// Which ciphersuite a group uses (design B.2, B.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CipherSuitePolicy {
    /// `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` (0x0003), the default.
    Classical,
    /// `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (0x004D), the per-Party
    /// "quantum-resistant" option. Requires the `x-wing` feature.
    QuantumResistant,
}

/// Marker for Party groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Party {}

/// Marker for Meetup groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Meetup {}

/// What a KeyPackage is for.
#[derive(Clone, Copy, Debug)]
pub enum KeyPackagePurpose<'key> {
    /// Joining a Party through an invite.
    Party,
    /// Joining a Meetup through a knock; carries the member's grant key in the
    /// `pingers_grant_key` leaf extension, covered by the leaf signature (design B.6.1).
    Meetup {
        /// The member's fresh grant public key.
        grant_key: &'key GrantPublicKey,
    },
}

/// The external PSK id that binds a Meetup to its Party and Spot: `(party_id, epoch, spot_id,
/// b)` (design B.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeetupPskId {
    /// The Party.
    pub party: PartyId,
    /// The Party epoch the PSK was derived at.
    pub epoch: Epoch,
    /// The Spot.
    pub spot: SpotId,
    /// The bucket the PSK was derived for.
    pub bucket: Bucket,
}

/// The PSK id and value to inject into, or check against, a Meetup group.
pub struct MeetupPskBinding<'psk> {
    /// The PSK id.
    pub id: MeetupPskId,
    /// `PSK_meet(b)` from pingers-crypto.
    pub value: &'psk MeetupPsk,
}

/// Everything needed to create a Meetup group (design B.5).
pub struct MeetupCreation<'creation> {
    /// Random 32-byte group id.
    pub group_id: GroupId,
    /// The PSK binding the group to the Party and Spot.
    pub psk: MeetupPskBinding<'creation>,
    /// The Party roster at the knock's epoch, for the roster check.
    pub party_roster: &'creation Roster,
    /// The knocker's KeyPackage, to add in the first commit.
    pub first_member: &'creation KeyPackageBytes,
    /// The creator's own grant key, for its leaf extension.
    pub own_grant_key: &'creation GrantPublicKey,
    /// The Party's ciphersuite.
    pub suite: CipherSuitePolicy,
}

/// A commit this member created, to post to the delivery service.
///
/// It is pending until the server accepts it: then call [`Group::merge_pending_commit`]; on a
/// conflict (another commit won the epoch) call [`Group::discard_pending_commit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    /// The commit message.
    pub commit: MlsMessageBytes,
    /// The Welcome for added members, if any were added.
    pub welcome: Option<WelcomeBytes>,
}

/// Whether a processed commit removed this member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelfRemoval {
    /// This member was removed; delete the group's state.
    Removed,
    /// This member is still in the group.
    StillMember,
}

/// What a processed commit changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitSummary {
    /// The new epoch.
    pub epoch: Epoch,
    /// Leaves added.
    pub added: Vec<LeafIndex>,
    /// Leaves removed.
    pub removed: Vec<LeafIndex>,
    /// Whether this member was among them.
    pub self_removal: SelfRemoval,
}

/// The result of processing one incoming message.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProcessedMessage {
    /// Application data (a Party state operation, or a padded Meetup frame).
    Application {
        /// The sender's leaf.
        sender: LeafIndex,
        /// The plaintext, zeroised on drop.
        plaintext: Zeroizing<Vec<u8>>,
    },
    /// A commit, already merged.
    Commit(CommitSummary),
    /// A proposal (for example a leaver's self-remove), stored until someone commits it.
    Proposal {
        /// The proposer's leaf.
        sender: LeafIndex,
    },
}

/// A member's grant key, read from its leaf extension (design B.6.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberGrantKey {
    /// The member.
    pub leaf: LeafIndex,
    /// Its grant public key.
    pub grant_key: GrantPublicKey,
}

/// A newly created Meetup group and the commit (with the knocker's Welcome) to post.
pub struct CreatedMeetup<'context, S: MlsStorage> {
    /// The group, with the commit pending.
    pub group: Group<'context, S, Meetup>,
    /// The commit and Welcome.
    pub commit: Commit,
}

/// The entry point: storage plus this device's signing key.
pub struct MlsContext<'context, S: MlsStorage> {
    storage: &'context S,
    signer: &'context AccountSigningKey,
}

impl<'context, S: MlsStorage> MlsContext<'context, S> {
    /// A context over `storage`, signing as `signer` (the account key is the leaf signature
    /// key in every group, design B.1).
    #[must_use]
    pub const fn new(storage: &'context S, signer: &'context AccountSigningKey) -> Self {
        Self { storage, signer }
    }

    /// A fresh KeyPackage with this device's credential.
    ///
    /// # Errors
    ///
    /// [`MlsError::UnsupportedSuite`], [`MlsError::Storage`] (the private part is stored),
    /// [`MlsError::Internal`].
    pub fn create_key_package(
        &self,
        purpose: KeyPackagePurpose<'_>,
        suite: CipherSuitePolicy,
        rng: &mut impl CryptoRng,
    ) -> Result<KeyPackageBytes, MlsError> {
        let suite = policy::ciphersuite(suite)?;
        let provider = Provider::new(self.storage, InjectedRand::new(rng));
        let builder = KeyPackage::builder();
        let builder = match purpose {
            KeyPackagePurpose::Party => {
                builder.leaf_node_capabilities(policy::capabilities(KindTag::Party, suite))
            }
            KeyPackagePurpose::Meetup { grant_key } => builder
                .leaf_node_capabilities(policy::capabilities(KindTag::Meetup, suite))
                .leaf_node_extensions(policy::grant_key_extensions(grant_key)?),
        };
        let bundle = builder
            .build(
                suite,
                &provider,
                &leaf_signer(self.signer),
                credential(self.signer),
            )
            .map_err(|_| provider.adapter().error_or(MlsError::Internal))?;
        serialize(bundle.key_package()).map(KeyPackageBytes::from_bytes)
    }

    /// Deletes the private part (init and leaf encryption keys) of a KeyPackage made by
    /// [`Self::create_key_package`] that will not be used, such as one sent with a knock that
    /// was never answered or one replaced by a fresh KeyPackage. A Welcome deletes it on its
    /// own; nothing else does, so without this the keys stay on the device for good. Deleting
    /// a KeyPackage that has no stored keys succeeds.
    ///
    /// # Errors
    ///
    /// [`MlsError::InvalidMessage`] if the bytes are not a valid KeyPackage of a supported
    /// suite; [`MlsError::Storage`]; [`MlsError::Internal`].
    pub fn delete_key_package(&self, key_package: &KeyPackageBytes) -> Result<(), MlsError> {
        let provider = Provider::new(self.storage, NoRand);
        let hash_ref = parse_key_package(key_package, &provider)?
            .hash_ref(provider.crypto())
            .map_err(|_| MlsError::Internal)?;
        provider.storage().delete_key_package(&hash_ref)?;
        Ok(())
    }

    /// Creates a Party group with this device as its only member.
    ///
    /// # Errors
    ///
    /// [`MlsError::UnsupportedSuite`], [`MlsError::Storage`], [`MlsError::Internal`].
    pub fn create_party_group(
        &self,
        group_id: GroupId,
        suite: CipherSuitePolicy,
        rng: &mut impl CryptoRng,
    ) -> Result<Group<'context, S, Party>, MlsError> {
        self.build_party_group(group_id.as_bytes(), suite, rng)
    }

    /// [`Self::create_party_group`] with an arbitrary OpenMLS group id, so tests can build the
    /// Welcome of a group whose id is not [`GroupId::LENGTH`] bytes.
    pub(crate) fn build_party_group(
        &self,
        raw_group_id: &[u8],
        suite: CipherSuitePolicy,
        rng: &mut impl CryptoRng,
    ) -> Result<Group<'context, S, Party>, MlsError> {
        let suite = policy::ciphersuite(suite)?;
        let provider = Provider::new(self.storage, InjectedRand::new(rng));
        let inner = MlsGroup::builder()
            .with_group_id(openmls::prelude::GroupId::from_slice(raw_group_id))
            .ciphersuite(suite)
            .with_wire_format_policy(openmls::prelude::PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
            .use_ratchet_tree_extension(true)
            .max_past_epochs(0)
            .sender_ratchet_configuration(policy::sender_ratchet(KindTag::Party))
            .with_capabilities(policy::capabilities(KindTag::Party, suite))
            .build(
                &provider,
                &leaf_signer(self.signer),
                credential(self.signer),
            )
            .map_err(|_| provider.adapter().error_or(MlsError::Internal))?;
        Ok(self.wrap(inner))
    }

    /// Joins a Party group from a Welcome.
    ///
    /// # Errors
    ///
    /// [`MlsError::InvalidMessage`] if the Welcome is not for one of our KeyPackages;
    /// [`MlsError::Storage`].
    pub fn join_party_group(
        &self,
        welcome: &WelcomeBytes,
    ) -> Result<Group<'context, S, Party>, MlsError> {
        let provider = Provider::new(self.storage, NoRand);
        let welcome = parse_welcome(welcome)?;
        let processed = ProcessedWelcome::new_from_welcome(
            &provider,
            &policy::join_config(KindTag::Party),
            welcome,
        )
        .map_err(|_| provider.adapter().error_or(MlsError::InvalidMessage))?;
        // A Party Welcome injects no PSK; one that does is a Meetup Welcome.
        if !processed.psks().is_empty() {
            return Err(MlsError::InvalidMessage);
        }
        let staged = processed
            .into_staged_welcome(&provider, None)
            .map_err(|_| provider.adapter().error_or(MlsError::InvalidMessage))?;
        if policy::is_meetup(staged.group_context().extensions()) {
            return Err(MlsError::InvalidMessage);
        }
        check_group_id_length(&staged)?;
        let inner = staged
            .into_group(&provider)
            .map_err(|_| provider.adapter().error_or(MlsError::InvalidMessage))?;
        Ok(self.wrap(inner))
    }

    /// Loads a Party group this device is in.
    ///
    /// # Errors
    ///
    /// [`MlsError::NotAMember`] if it is not in storage; [`MlsError::Storage`].
    pub fn party_group(&self, group_id: &GroupId) -> Result<Group<'context, S, Party>, MlsError> {
        self.load(group_id)
    }

    /// Creates a Meetup group, adds the knocker and injects the PSK in the first commit
    /// (design B.5). The returned commit carries the Welcome for the knocker.
    ///
    /// # Errors
    ///
    /// [`MlsError::NotInPartyRoster`] if the knocker's KeyPackage fails the roster check;
    /// [`MlsError::InvalidMessage`] if it is malformed; [`MlsError::Storage`].
    pub fn create_meetup_group(
        &self,
        creation: &MeetupCreation<'_>,
        rng: &mut impl CryptoRng,
    ) -> Result<CreatedMeetup<'context, S>, MlsError> {
        let suite = policy::ciphersuite(creation.suite)?;
        let provider = Provider::new(self.storage, InjectedRand::new(rng));
        let knocker = parse_key_package(creation.first_member, &provider)?;
        check_meetup_leaf(creation.party_roster, knocker.leaf_node())?;
        let signer = leaf_signer(self.signer);
        let mut inner = MlsGroup::builder()
            .with_group_id(openmls::prelude::GroupId::from_slice(
                creation.group_id.as_bytes(),
            ))
            .ciphersuite(suite)
            .with_wire_format_policy(openmls::prelude::PURE_CIPHERTEXT_WIRE_FORMAT_POLICY)
            .use_ratchet_tree_extension(true)
            .max_past_epochs(0)
            .sender_ratchet_configuration(policy::sender_ratchet(KindTag::Meetup))
            .with_capabilities(policy::capabilities(KindTag::Meetup, suite))
            .with_leaf_node_extensions(policy::grant_key_extensions(creation.own_grant_key)?)
            .map_err(|_| MlsError::Internal)?
            .with_group_context_extensions(policy::meetup_context_extensions()?)
            .build(&provider, &signer, credential(self.signer))
            .map_err(|_| provider.adapter().error_or(MlsError::Internal))?;
        policy::store_party_roster(self.storage, &creation.group_id, creation.party_roster)?;

        let psk_id = PreSharedKeyId::new(
            suite,
            provider.rand(),
            Psk::External(ExternalPsk::new(policy::psk_id_bytes(&creation.psk.id))),
        )
        .map_err(|_| MlsError::Internal)?;
        psk_id
            .store(&provider, creation.psk.value.expose_bytes())
            .map_err(|_| provider.adapter().error_or(MlsError::Internal))?;
        let staged = stage_commit(&mut inner, &provider, &signer, |builder| {
            builder
                .consume_proposal_store(false)
                .propose_adds([knocker])
                .add_proposal(Proposal::PreSharedKey(Box::new(PreSharedKeyProposal::new(
                    psk_id.clone(),
                ))))
        });
        // The PSK is in the pending commit's key schedule; it is not kept in storage.
        provider.storage().delete_psk(psk_id.psk())?;
        let commit = staged?;
        Ok(CreatedMeetup {
            group: self.wrap(inner),
            commit,
        })
    }

    /// Joins a Meetup group from a Welcome, checking the PSK and that every member is in the
    /// Party roster.
    ///
    /// # Errors
    ///
    /// [`MlsError::PskMismatch`], [`MlsError::NotInPartyRoster`], [`MlsError::InvalidMessage`],
    /// [`MlsError::Storage`].
    pub fn join_meetup_group(
        &self,
        welcome: &WelcomeBytes,
        psk: &MeetupPskBinding<'_>,
        party_roster: &Roster,
    ) -> Result<Group<'context, S, Meetup>, MlsError> {
        let provider = Provider::new(self.storage, NoRand);
        let welcome = parse_welcome(welcome)?;
        // OpenMLS reads the PSK while it decrypts the Welcome, so the PSK is stored under the
        // expected id first (and deleted right after): a Welcome naming another id then finds
        // no PSK, and a wrong value gives a joiner secret that does not decrypt the GroupInfo.
        let expected = PreSharedKeyId::external(policy::psk_id_bytes(&psk.id), Vec::new());
        expected
            .store(&provider, psk.value.expose_bytes())
            .map_err(|_| provider.adapter().error_or(MlsError::Internal))?;
        let staged = stage_meetup_welcome(&provider, welcome, expected.psk());
        provider.storage().delete_psk(expected.psk())?;
        let staged = staged?;
        if !policy::is_meetup(staged.group_context().extensions()) {
            return Err(MlsError::InvalidMessage);
        }
        check_group_id_length(&staged)?;
        for member in staged.members() {
            let key = policy::leaf_account_key(member.signature_key.as_slice())
                .ok_or(MlsError::InvalidMessage)?;
            if !party_roster.contains_key(&key) {
                return Err(MlsError::NotInPartyRoster);
            }
        }
        let inner = staged
            .into_group(&provider)
            .map_err(|_| provider.adapter().error_or(MlsError::InvalidMessage))?;
        let mut group: Group<'context, S, Meetup> = self.wrap(inner);
        if group.grant_keys().is_err() {
            self.delete_group(&group.group_id())?;
            return Err(MlsError::InvalidMessage);
        }
        group.update_party_roster(party_roster)?;
        Ok(group)
    }

    /// Loads a Meetup group this device is in.
    ///
    /// # Errors
    ///
    /// [`MlsError::NotAMember`] if it is not in storage; [`MlsError::Storage`].
    pub fn meetup_group(&self, group_id: &GroupId) -> Result<Group<'context, S, Meetup>, MlsError> {
        self.load(group_id)
    }

    /// Deletes all state of a group: on leaving, on removal, and at the end of a Meetup
    /// (design B.5: MLS group state and epoch secrets are deleted).
    ///
    /// # Errors
    ///
    /// [`MlsError::Storage`].
    pub fn delete_group(&self, group_id: &GroupId) -> Result<(), MlsError> {
        // Every row of the group, OpenMLS's (including key pairs of epochs the group has moved
        // past) and the wrapper's own, is under its prefix.
        self.storage
            .delete_prefix(&crate::group_storage_prefix(group_id)?)?;
        Ok(())
    }

    fn wrap<Kind>(&self, inner: MlsGroup) -> Group<'context, S, Kind> {
        Group {
            storage: self.storage,
            signer: self.signer,
            inner,
            kind: PhantomData,
        }
    }

    fn load<Kind: GroupKind>(
        &self,
        group_id: &GroupId,
    ) -> Result<Group<'context, S, Kind>, MlsError> {
        let adapter = crate::adapter::StorageAdapter::new(self.storage);
        let id = openmls::prelude::GroupId::from_slice(group_id.as_bytes());
        let inner = MlsGroup::load(&adapter, &id)?.ok_or(MlsError::NotAMember)?;
        let tag = if policy::is_meetup(inner.extensions()) {
            KindTag::Meetup
        } else {
            KindTag::Party
        };
        if tag != Kind::TAG || !inner.is_active() {
            return Err(MlsError::NotAMember);
        }
        Ok(self.wrap(inner))
    }
}

mod sealed {
    /// [`super::Party`] or [`super::Meetup`].
    pub trait GroupKind {
        /// Which one.
        const TAG: crate::policy::KindTag;
    }
}

pub(crate) use sealed::GroupKind;

impl GroupKind for Party {
    const TAG: KindTag = KindTag::Party;
}

impl GroupKind for Meetup {
    const TAG: KindTag = KindTag::Meetup;
}

/// A loaded group. `Kind` is [`Party`] or [`Meetup`].
///
/// Every change is written to storage as it happens. After an [`MlsError::Storage`] the handle
/// may be ahead of the store: drop it, roll back the caller's transaction (design B.7) and load
/// the group again.
pub struct Group<'context, S: MlsStorage, Kind> {
    pub(crate) storage: &'context S,
    pub(crate) signer: &'context AccountSigningKey,
    pub(crate) inner: MlsGroup,
    kind: PhantomData<Kind>,
}

impl<S: MlsStorage, Kind: GroupKind> Group<'_, S, Kind> {
    /// The group's id.
    #[must_use]
    pub fn group_id(&self) -> GroupId {
        let mut bytes = [0; GroupId::LENGTH];
        for (target, source) in bytes.iter_mut().zip(self.inner.group_id().as_slice()) {
            *target = *source;
        }
        GroupId::from_bytes(bytes)
    }

    /// The current epoch.
    #[must_use]
    pub fn epoch(&self) -> Epoch {
        Epoch::new(self.inner.epoch().as_u64())
    }

    /// This device's leaf.
    #[must_use]
    pub fn own_leaf(&self) -> LeafIndex {
        LeafIndex::new(self.inner.own_leaf_index().u32())
    }

    /// The members at the current epoch.
    #[must_use]
    pub fn roster(&self) -> Roster {
        Roster::new(
            self.inner
                .members()
                .filter_map(|member| {
                    policy::leaf_account_key(&member.signature_key).map(|signature_key| {
                        RosterEntry {
                            leaf: LeafIndex::new(member.index.u32()),
                            signature_key,
                        }
                    })
                })
                .collect(),
        )
    }

    /// Processes one message from the group log or live channel.
    ///
    /// # Errors
    ///
    /// [`MlsError::InvalidMessage`], [`MlsError::WrongEpoch`], [`MlsError::NotInPartyRoster`]
    /// (Meetup commits adding non-members), [`MlsError::Storage`].
    pub fn process(&mut self, message: &MlsMessageBytes) -> Result<ProcessedMessage, MlsError> {
        let parsed = MlsMessageIn::tls_deserialize_exact_bytes(message.as_bytes())
            .map_err(|_| MlsError::InvalidMessage)?;
        let protocol = match parsed.extract() {
            MlsMessageBodyIn::PrivateMessage(private) => ProtocolMessage::from(private),
            MlsMessageBodyIn::PublicMessage(_)
            | MlsMessageBodyIn::Welcome(_)
            | MlsMessageBodyIn::GroupInfo(_)
            | MlsMessageBodyIn::KeyPackage(_) => return Err(MlsError::InvalidMessage),
        };
        if protocol.group_id() != self.inner.group_id() {
            return Err(MlsError::InvalidMessage);
        }
        let message_epoch = protocol.epoch();
        if message_epoch != self.inner.epoch() {
            return Err(MlsError::WrongEpoch {
                message: Epoch::new(message_epoch.as_u64()),
                group: self.epoch(),
            });
        }
        let provider = Provider::new(self.storage, NoRand);
        let processed = self
            .inner
            .process_message(&provider, protocol)
            .map_err(|_| provider.adapter().error_or(MlsError::InvalidMessage))?;
        let Sender::Member(sender) = *processed.sender() else {
            return Err(MlsError::InvalidMessage);
        };
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(application) => {
                Ok(ProcessedMessage::Application {
                    sender: LeafIndex::new(sender.u32()),
                    plaintext: Zeroizing::new(application.into_bytes()),
                })
            }
            ProcessedMessageContent::ProposalMessage(proposal) => {
                // The only standalone proposal Pingers sends is a member removing itself.
                let leaving = matches!(proposal.proposal(), Proposal::Remove(remove) if remove.removed() == sender);
                if !leaving {
                    return Err(MlsError::InvalidMessage);
                }
                self.inner
                    .store_pending_proposal(provider.storage(), *proposal)?;
                Ok(ProcessedMessage::Proposal {
                    sender: LeafIndex::new(sender.u32()),
                })
            }
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                if Kind::TAG == KindTag::Meetup {
                    let party_roster = policy::load_party_roster(self.storage, &self.group_id())?;
                    check_meetup_commit(&party_roster, &staged)?;
                }
                let before = self.roster();
                let removed = removed_leaves(&staged);
                let epoch = Epoch::new(staged.epoch().as_u64());
                let self_removal = if staged.self_removed() {
                    SelfRemoval::Removed
                } else {
                    SelfRemoval::StillMember
                };
                self.inner
                    .merge_staged_commit(&provider, *staged)
                    .map_err(|_| provider.adapter().error_or(MlsError::Internal))?;
                Ok(ProcessedMessage::Commit(CommitSummary {
                    epoch,
                    added: added_leaves(&before, &self.roster()),
                    removed,
                    self_removal,
                }))
            }
            ProcessedMessageContent::ExternalJoinProposalMessage(_)
            | ProcessedMessageContent::OwnPendingCommit
            | ProcessedMessageContent::OwnPrivateMessage => Err(MlsError::InvalidMessage),
        }
    }

    /// Encrypts application data as a `PrivateMessage`. Meetup frames must already be padded.
    ///
    /// # Errors
    ///
    /// [`MlsError::CommitPending`] (also while received proposals wait to be committed),
    /// [`MlsError::Storage`], [`MlsError::Internal`].
    pub fn seal_application(
        &mut self,
        plaintext: &Padded,
        rng: &mut impl CryptoRng,
    ) -> Result<MlsMessageBytes, MlsError> {
        if self.inner.pending_commit().is_some() || self.inner.has_pending_proposals() {
            return Err(MlsError::CommitPending);
        }
        let provider = Provider::new(self.storage, InjectedRand::new(rng));
        let message = self
            .inner
            .create_message(&provider, &leaf_signer(self.signer), plaintext.as_bytes())
            .map_err(|_| provider.adapter().error_or(MlsError::Internal))?;
        serialize(&message).map(MlsMessageBytes::from_bytes)
    }

    /// Commits removal of `leaf` (a removed Party member, or a silent Meetup member).
    ///
    /// # Errors
    ///
    /// [`MlsError::NotAMember`] if no such leaf; [`MlsError::CommitPending`];
    /// [`MlsError::Storage`].
    pub fn remove_member(
        &mut self,
        leaf: LeafIndex,
        rng: &mut impl CryptoRng,
    ) -> Result<Commit, MlsError> {
        let index = LeafNodeIndex::new(leaf.get());
        if self.inner.member_at(index).is_none() {
            return Err(MlsError::NotAMember);
        }
        self.commit(rng, |builder| {
            builder
                .consume_proposal_store(false)
                .propose_removals([index])
        })
    }

    /// Commits every stored proposal (for example a leaver's self-remove).
    ///
    /// # Errors
    ///
    /// [`MlsError::CommitPending`], [`MlsError::Storage`].
    pub fn commit_pending_proposals(
        &mut self,
        rng: &mut impl CryptoRng,
    ) -> Result<Commit, MlsError> {
        self.commit(rng, |builder| builder.consume_proposal_store(true))
    }

    /// Commits an Update of this member's leaf (at least weekly in Party groups, design B.2).
    ///
    /// # Errors
    ///
    /// [`MlsError::CommitPending`], [`MlsError::Storage`].
    pub fn self_update(&mut self, rng: &mut impl CryptoRng) -> Result<Commit, MlsError> {
        self.commit(rng, |builder| {
            builder
                .consume_proposal_store(false)
                .force_self_update(true)
        })
    }

    /// A self-remove proposal for leaving; another member commits it. Delete the group with
    /// [`MlsContext::delete_group`] right after sending it.
    ///
    /// # Errors
    ///
    /// [`MlsError::Storage`], [`MlsError::Internal`].
    pub fn propose_leave(&mut self, rng: &mut impl CryptoRng) -> Result<MlsMessageBytes, MlsError> {
        let provider = Provider::new(self.storage, InjectedRand::new(rng));
        let message = self
            .inner
            .leave_group(&provider, &leaf_signer(self.signer))
            .map_err(|_| provider.adapter().error_or(MlsError::Internal))?;
        serialize(&message).map(MlsMessageBytes::from_bytes)
    }

    /// Applies this member's pending commit after the server accepted it.
    ///
    /// # Errors
    ///
    /// [`MlsError::Storage`]; [`MlsError::Internal`] if nothing is pending.
    pub fn merge_pending_commit(&mut self) -> Result<CommitSummary, MlsError> {
        let pending = self.inner.pending_commit().ok_or(MlsError::Internal)?;
        let removed = removed_leaves(pending);
        let epoch = Epoch::new(pending.epoch().as_u64());
        let before = self.roster();
        let provider = Provider::new(self.storage, NoRand);
        self.inner
            .merge_pending_commit(&provider)
            .map_err(|_| provider.adapter().error_or(MlsError::Internal))?;
        Ok(CommitSummary {
            epoch,
            added: added_leaves(&before, &self.roster()),
            removed,
            self_removal: SelfRemoval::StillMember,
        })
    }

    /// Drops this member's pending commit after the server rejected it (another commit won the
    /// epoch).
    ///
    /// # Errors
    ///
    /// [`MlsError::Storage`].
    pub fn discard_pending_commit(&mut self) -> Result<(), MlsError> {
        let provider = Provider::new(self.storage, NoRand);
        self.inner.clear_pending_commit(provider.storage())?;
        Ok(())
    }

    /// The current epoch's state snapshot key (`MLS-Exporter("pingers/v1/state", epoch)`).
    ///
    /// # Errors
    ///
    /// [`MlsError::Internal`] if the exporter fails.
    pub fn state_key(&self) -> Result<StateKey, MlsError> {
        let context = self.inner.epoch().as_u64().to_be_bytes();
        self.export(ExporterLabel::State, &context)
            .map(StateKey::from_bytes)
    }

    /// The current epoch's log-auth key pair (`MLS-Exporter("pingers/v1/log-auth", group_id)`).
    ///
    /// # Errors
    ///
    /// [`MlsError::Internal`] if the exporter fails.
    pub fn log_auth_key_pair(&self) -> Result<LogAuthKeyPair, MlsError> {
        let context = self.group_id();
        self.export(ExporterLabel::LogAuth, context.as_bytes())
            .map(LogAuthKeyPair::from_bytes)
    }

    /// The state snapshot key of the epoch the pending commit moves to: the committer seals
    /// the snapshot it uploads with the commit under it (design B.2).
    ///
    /// # Errors
    ///
    /// [`MlsError::Internal`] if nothing is pending or the exporter fails.
    pub fn pending_state_key(&self) -> Result<StateKey, MlsError> {
        let pending = self.inner.pending_commit().ok_or(MlsError::Internal)?;
        let context = pending.epoch().as_u64().to_be_bytes();
        export_pending(pending, ExporterLabel::State, &context).map(StateKey::from_bytes)
    }

    /// The log-auth key pair of the epoch the pending commit moves to: the committer sends its
    /// public key with the commit (design B.2).
    ///
    /// # Errors
    ///
    /// [`MlsError::Internal`] if nothing is pending or the exporter fails.
    pub fn pending_log_auth_key_pair(&self) -> Result<LogAuthKeyPair, MlsError> {
        let pending = self.inner.pending_commit().ok_or(MlsError::Internal)?;
        let context = self.group_id();
        export_pending(pending, ExporterLabel::LogAuth, context.as_bytes())
            .map(LogAuthKeyPair::from_bytes)
    }

    fn export(
        &self,
        label: ExporterLabel,
        context: &[u8],
    ) -> Result<Zeroizing<[u8; 32]>, MlsError> {
        let crypto = openmls_rust_crypto::RustCrypto::default();
        let secret = self
            .inner
            .export_secret(&crypto, label.label(), context, label.output_length())
            .map_err(|_| MlsError::Internal)?;
        to_key(secret)
    }

    /// Builds, signs and stages a commit; it stays pending until merged or discarded.
    fn commit<R: CryptoRng>(
        &mut self,
        rng: &mut R,
        proposals: impl for<'builder> FnOnce(
            CommitBuilder<'builder, Initial>,
        ) -> CommitBuilder<'builder, Initial>,
    ) -> Result<Commit, MlsError> {
        let provider = Provider::new(self.storage, InjectedRand::new(rng));
        stage_commit(
            &mut self.inner,
            &provider,
            &leaf_signer(self.signer),
            proposals,
        )
    }
}

impl<S: MlsStorage> Group<'_, S, Party> {
    /// Adds members from their KeyPackages (an approved invite).
    ///
    /// # Errors
    ///
    /// [`MlsError::InvalidMessage`] for a malformed KeyPackage; [`MlsError::CommitPending`];
    /// [`MlsError::Storage`].
    pub fn add_members(
        &mut self,
        key_packages: &[KeyPackageBytes],
        rng: &mut impl CryptoRng,
    ) -> Result<Commit, MlsError> {
        let key_packages = parse_key_packages(key_packages, self.storage)?;
        self.commit(rng, |builder| {
            builder
                .consume_proposal_store(false)
                .propose_adds(key_packages)
        })
    }

    /// `PR_e = MLS-Exporter("pingers/v1/presence", party_id, 32)` for the current epoch.
    ///
    /// # Errors
    ///
    /// [`MlsError::Internal`] if the exporter fails.
    pub fn presence_root(&self, party: &PartyId) -> Result<PresenceRoot, MlsError> {
        self.export(ExporterLabel::Presence, party.as_bytes())
            .map(PresenceRoot::from_bytes)
    }
}

impl<S: MlsStorage> Group<'_, S, Meetup> {
    /// Adds newcomers, checking each KeyPackage's signature key against the Party roster
    /// (design B.5). The roster becomes the one later commits are checked against, as with
    /// [`Group::update_party_roster`].
    ///
    /// # Errors
    ///
    /// [`MlsError::NotInPartyRoster`], [`MlsError::InvalidMessage`],
    /// [`MlsError::CommitPending`], [`MlsError::Storage`].
    pub fn add_members(
        &mut self,
        key_packages: &[KeyPackageBytes],
        party_roster: &Roster,
        rng: &mut impl CryptoRng,
    ) -> Result<Commit, MlsError> {
        let key_packages = parse_key_packages(key_packages, self.storage)?;
        for key_package in &key_packages {
            check_meetup_leaf(party_roster, key_package.leaf_node())?;
        }
        self.update_party_roster(party_roster)?;
        self.commit(rng, |builder| {
            builder
                .consume_proposal_store(false)
                .propose_adds(key_packages)
        })
    }

    /// Replaces the Party roster that incoming commits are checked against (design B.5:
    /// every member checks each added leaf). Call it when the Party's roster changes, so
    /// adds of new Party members are accepted and adds of removed ones refused.
    ///
    /// # Errors
    ///
    /// [`MlsError::Storage`].
    pub fn update_party_roster(&mut self, party_roster: &Roster) -> Result<(), MlsError> {
        policy::store_party_roster(self.storage, &self.group_id(), party_roster)
    }

    /// Every member's grant key from its `pingers_grant_key` leaf extension.
    ///
    /// # Errors
    ///
    /// [`MlsError::InvalidMessage`] if a leaf lacks the extension (it could not have joined).
    pub fn grant_keys(&self) -> Result<Vec<MemberGrantKey>, MlsError> {
        self.inner
            .members()
            .map(|member| {
                self.inner
                    .public_group()
                    .leaf(member.index)
                    .and_then(policy::leaf_grant_key)
                    .map(|grant_key| MemberGrantKey {
                        leaf: LeafIndex::new(member.index.u32()),
                        grant_key,
                    })
                    .ok_or(MlsError::InvalidMessage)
            })
            .collect()
    }
}

/// Builds, signs and stages a commit on `group`.
fn stage_commit<S: MlsStorage, R: openmls_traits::random::OpenMlsRand>(
    group: &mut MlsGroup,
    provider: &Provider<'_, S, R>,
    signer: &openmls_basic_credential::SignatureKeyPair,
    proposals: impl for<'builder> FnOnce(
        CommitBuilder<'builder, Initial>,
    ) -> CommitBuilder<'builder, Initial>,
) -> Result<Commit, MlsError> {
    if group.pending_commit().is_some() {
        return Err(MlsError::CommitPending);
    }
    let failed = |_| provider.adapter().error_or(MlsError::Internal);
    let bundle = proposals(group.commit_builder())
        .load_psks(provider.storage())
        .map_err(|_| provider.adapter().error_or(MlsError::Internal))?
        .build(provider.rand(), provider.crypto(), signer, |_| true)
        .map_err(|_| provider.adapter().error_or(MlsError::Internal))?
        .stage_commit(provider)
        .map_err(failed)?;
    let commit = serialize(bundle.commit()).map(MlsMessageBytes::from_bytes)?;
    let welcome = bundle
        .to_welcome_msg()
        .map(|welcome| serialize(&welcome).map(WelcomeBytes::from_bytes))
        .transpose()?;
    Ok(Commit { commit, welcome })
}

/// Decrypts a Meetup Welcome whose PSK is already stored, checking it injects exactly that PSK.
/// Refuses a Welcome whose group id is not [`GroupId::LENGTH`] bytes. [`Group::group_id`]
/// truncates or zero-pads the OpenMLS id, so a longer id such as `victim_id || 0x00` would
/// otherwise make the roster, the storage prefix and `delete_group` address another group.
fn check_group_id_length(staged: &StagedWelcome) -> Result<(), MlsError> {
    if staged.group_context().group_id().as_slice().len() == GroupId::LENGTH {
        Ok(())
    } else {
        Err(MlsError::InvalidMessage)
    }
}

fn stage_meetup_welcome<S: MlsStorage>(
    provider: &Provider<'_, S, NoRand>,
    welcome: Welcome,
    expected: &Psk,
) -> Result<StagedWelcome, MlsError> {
    let failed = |error: WelcomeError<StorageError>| {
        let wrong_psk = matches!(
            error,
            WelcomeError::Psk(_)
                | WelcomeError::GroupInfo(_)
                | WelcomeError::ConfirmationTagMismatch
        );
        provider.adapter().error_or(if wrong_psk {
            MlsError::PskMismatch
        } else {
            MlsError::InvalidMessage
        })
    };
    let processed = ProcessedWelcome::new_from_welcome(
        provider,
        &policy::join_config(KindTag::Meetup),
        welcome,
    )
    .map_err(failed)?;
    // The creation Welcome injects the PSK (design B.5); a later Add's Welcome injects none, and
    // the joiner then relies on the roster check alone. Anything else is refused.
    let psk_matches = match processed.psks() {
        [] => true,
        [only] => only.psk() == expected,
        _ => false,
    };
    if !psk_matches {
        return Err(MlsError::PskMismatch);
    }
    processed
        .into_staged_welcome(provider, None)
        .map_err(failed)
}

/// The roster check of design B.5 for one leaf joining or changing in a Meetup: its signature
/// key is a Party member's, and it carries a grant key (design B.6.1).
fn check_meetup_leaf(
    party_roster: &Roster,
    leaf: &openmls::prelude::LeafNode,
) -> Result<(), MlsError> {
    let key = policy::leaf_account_key(leaf.signature_key().as_slice())
        .ok_or(MlsError::InvalidMessage)?;
    if !party_roster.contains_key(&key) {
        return Err(MlsError::NotInPartyRoster);
    }
    if policy::leaf_grant_key(leaf).is_none() {
        return Err(MlsError::InvalidMessage);
    }
    Ok(())
}

/// The roster check over every leaf a Meetup commit adds or changes.
fn check_meetup_commit(party_roster: &Roster, staged: &StagedCommit) -> Result<(), MlsError> {
    for add in staged.add_proposals() {
        check_meetup_leaf(party_roster, add.add_proposal().key_package().leaf_node())?;
    }
    for update in staged.update_proposals() {
        check_meetup_leaf(party_roster, update.update_proposal().leaf_node())?;
    }
    if let Some(leaf) = staged.update_path_leaf_node() {
        check_meetup_leaf(party_roster, leaf)?;
    }
    Ok(())
}

/// The leaves a commit removes.
fn removed_leaves(staged: &StagedCommit) -> Vec<LeafIndex> {
    staged
        .remove_proposals()
        .map(|remove| LeafIndex::new(remove.remove_proposal().removed().u32()))
        .collect()
}

/// The leaves in `after` that were not in `before` with the same key.
fn added_leaves(before: &Roster, after: &Roster) -> Vec<LeafIndex> {
    after
        .entries()
        .iter()
        .filter(|entry| !before.entries().contains(entry))
        .map(|entry| entry.leaf)
        .collect()
}

fn export_pending(
    pending: &StagedCommit,
    label: ExporterLabel,
    context: &[u8],
) -> Result<Zeroizing<[u8; 32]>, MlsError> {
    let crypto = openmls_rust_crypto::RustCrypto::default();
    let secret = pending
        .export_secret(&crypto, label.label(), context, label.output_length())
        .map_err(|_| MlsError::Internal)?;
    to_key(secret)
}

/// Moves an exporter output into a fixed-size secret, zeroising the original.
fn to_key(secret: Vec<u8>) -> Result<Zeroizing<[u8; 32]>, MlsError> {
    let secret = Zeroizing::new(secret);
    let mut key = Zeroizing::new([0; 32]);
    if secret.len() != key.len() {
        return Err(MlsError::Internal);
    }
    key.copy_from_slice(&secret);
    Ok(key)
}

fn serialize(value: &impl openmls::prelude::tls_codec::Serialize) -> Result<Vec<u8>, MlsError> {
    value
        .tls_serialize_detached()
        .map_err(|_| MlsError::Internal)
}

fn parse_welcome(bytes: &WelcomeBytes) -> Result<Welcome, MlsError> {
    let message = MlsMessageIn::tls_deserialize_exact_bytes(bytes.as_bytes())
        .map_err(|_| MlsError::InvalidMessage)?;
    match message.extract() {
        MlsMessageBodyIn::Welcome(welcome) => Ok(welcome),
        MlsMessageBodyIn::PublicMessage(_)
        | MlsMessageBodyIn::PrivateMessage(_)
        | MlsMessageBodyIn::GroupInfo(_)
        | MlsMessageBodyIn::KeyPackage(_) => Err(MlsError::InvalidMessage),
    }
}

pub(crate) fn parse_key_package<S: MlsStorage, R: openmls_traits::random::OpenMlsRand>(
    bytes: &KeyPackageBytes,
    provider: &Provider<'_, S, R>,
) -> Result<KeyPackage, MlsError> {
    validate_key_package(bytes, provider.crypto())
}

fn validate_key_package(
    bytes: &KeyPackageBytes,
    crypto: &impl openmls_traits::crypto::OpenMlsCrypto,
) -> Result<KeyPackage, MlsError> {
    // `validate` checks the leaf signature with whatever suite the KeyPackage names, so an
    // untrusted KeyPackage could otherwise reach ECDSA P-256 code (deny.toml,
    // RUSTSEC-2026-0387). The suite is the second field: version (u16), then suite (u16).
    let suite = bytes.as_bytes().get(2..4).ok_or(MlsError::InvalidMessage)?;
    if !policy::is_supported_suite_code(suite) {
        return Err(MlsError::InvalidMessage);
    }
    KeyPackageIn::tls_deserialize_exact_bytes(bytes.as_bytes())
        .map_err(|_| MlsError::InvalidMessage)?
        .validate(crypto, ProtocolVersion::Mls10)
        .map_err(|_| MlsError::InvalidMessage)
}

fn parse_key_packages<S: MlsStorage>(
    key_packages: &[KeyPackageBytes],
    storage: &S,
) -> Result<Vec<KeyPackage>, MlsError> {
    let provider = Provider::new(storage, NoRand);
    key_packages
        .iter()
        .map(|bytes| parse_key_package(bytes, &provider))
        .collect()
}

/// What a validated KeyPackage says about the member it would add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyPackageInfo {
    /// The leaf's signature key, which is the member's account key (design B.1).
    pub signature_key: AccountPublicKey,
    /// The suite the KeyPackage is for; a group of another suite cannot add it.
    pub suite: CipherSuitePolicy,
}

/// Validates a KeyPackage (signature, lifetime, a suite this build runs) and returns its
/// signature key and suite, so a caller can name or refuse a joiner before the Add is merged.
///
/// # Errors
///
/// [`MlsError::InvalidMessage`] for bytes that are not a valid KeyPackage of a supported
/// suite, or whose signature key is not an Ed25519 key.
pub fn inspect_key_package(key_package: &KeyPackageBytes) -> Result<KeyPackageInfo, MlsError> {
    let crypto = openmls_rust_crypto::RustCrypto::default();
    let parsed = validate_key_package(key_package, &crypto)?;
    let signature_key = policy::leaf_account_key(parsed.leaf_node().signature_key().as_slice())
        .ok_or(MlsError::InvalidMessage)?;
    let suite = policy::policy_of(parsed.ciphersuite()).ok_or(MlsError::InvalidMessage)?;
    Ok(KeyPackageInfo {
        signature_key,
        suite,
    })
}
