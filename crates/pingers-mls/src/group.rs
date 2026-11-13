// SPDX-License-Identifier: AGPL-3.0-only
//! Party and Meetup groups.

use core::marker::PhantomData;

use pingers_crypto::{
    AccountSigningKey, GrantPublicKey, LogAuthKeyPair, MeetupPsk, PresenceRoot, StateKey,
};
use pingers_proto::{
    Epoch, GroupId, KeyPackageBytes, LeafIndex, MlsMessageBytes, Padded, PartyId, SpotId,
    WelcomeBytes,
};
use pingers_time::Bucket;
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::stub::{Unimplemented, not_implemented};
use crate::{MlsStorage, Roster, StorageError};

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
    /// A commit is pending; merge or discard it before creating another.
    #[error("a commit is already pending")]
    CommitPending,
    /// OpenMLS reported an error the wrapper does not map more precisely.
    #[error("internal MLS error")]
    Internal,
    /// This part of the contract has no implementation yet.
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
        not_implemented((self.storage, self.signer, purpose, suite, rng))
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
        not_implemented((self.storage, self.signer, group_id, suite, rng))
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
        not_implemented((self.storage, self.signer, welcome))
    }

    /// Loads a Party group this device is in.
    ///
    /// # Errors
    ///
    /// [`MlsError::NotAMember`] if it is not in storage; [`MlsError::Storage`].
    pub fn party_group(&self, group_id: &GroupId) -> Result<Group<'context, S, Party>, MlsError> {
        not_implemented((self.storage, self.signer, group_id))
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
        not_implemented((self.storage, self.signer, creation, rng))
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
        not_implemented((self.storage, self.signer, welcome, psk, party_roster))
    }

    /// Loads a Meetup group this device is in.
    ///
    /// # Errors
    ///
    /// [`MlsError::NotAMember`] if it is not in storage; [`MlsError::Storage`].
    pub fn meetup_group(&self, group_id: &GroupId) -> Result<Group<'context, S, Meetup>, MlsError> {
        not_implemented((self.storage, self.signer, group_id))
    }

    /// Deletes all state of a group: on leaving, on removal, and at the end of a Meetup
    /// (design B.5: MLS group state and epoch secrets are deleted).
    ///
    /// # Errors
    ///
    /// [`MlsError::Storage`].
    pub fn delete_group(&self, group_id: &GroupId) -> Result<(), MlsError> {
        not_implemented((self.storage, self.signer, group_id))
    }
}

/// A loaded group. `Kind` is [`Party`] or [`Meetup`].
pub struct Group<'context, S: MlsStorage, Kind> {
    context: PhantomData<&'context S>,
    kind: PhantomData<Kind>,
    unimplemented: Unimplemented,
}

impl<S: MlsStorage, Kind> Group<'_, S, Kind> {
    /// The group's id.
    #[must_use]
    pub fn group_id(&self) -> GroupId {
        match self.unimplemented {}
    }

    /// The current epoch.
    #[must_use]
    pub fn epoch(&self) -> Epoch {
        match self.unimplemented {}
    }

    /// This device's leaf.
    #[must_use]
    pub fn own_leaf(&self) -> LeafIndex {
        match self.unimplemented {}
    }

    /// The members at the current epoch.
    #[must_use]
    pub fn roster(&self) -> Roster {
        match self.unimplemented {}
    }

    /// Processes one message from the group log or live channel.
    ///
    /// # Errors
    ///
    /// [`MlsError::InvalidMessage`], [`MlsError::WrongEpoch`], [`MlsError::NotInPartyRoster`]
    /// (Meetup commits adding non-members), [`MlsError::Storage`].
    pub fn process(&mut self, message: &MlsMessageBytes) -> Result<ProcessedMessage, MlsError> {
        let _ = message;
        match self.unimplemented {}
    }

    /// Encrypts application data as a `PrivateMessage`. Meetup frames must already be padded.
    ///
    /// # Errors
    ///
    /// [`MlsError::CommitPending`], [`MlsError::Storage`], [`MlsError::Internal`].
    pub fn seal_application(
        &mut self,
        plaintext: &Padded,
        rng: &mut impl CryptoRng,
    ) -> Result<MlsMessageBytes, MlsError> {
        let _ = (plaintext, rng);
        match self.unimplemented {}
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
        let _ = (leaf, rng);
        match self.unimplemented {}
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
        let _ = rng;
        match self.unimplemented {}
    }

    /// Commits an Update of this member's leaf (at least weekly in Party groups, design B.2).
    ///
    /// # Errors
    ///
    /// [`MlsError::CommitPending`], [`MlsError::Storage`].
    pub fn self_update(&mut self, rng: &mut impl CryptoRng) -> Result<Commit, MlsError> {
        let _ = rng;
        match self.unimplemented {}
    }

    /// A self-remove proposal for leaving; another member commits it. Delete the group with
    /// [`MlsContext::delete_group`] right after sending it.
    ///
    /// # Errors
    ///
    /// [`MlsError::Storage`], [`MlsError::Internal`].
    pub fn propose_leave(&mut self, rng: &mut impl CryptoRng) -> Result<MlsMessageBytes, MlsError> {
        let _ = rng;
        match self.unimplemented {}
    }

    /// Applies this member's pending commit after the server accepted it.
    ///
    /// # Errors
    ///
    /// [`MlsError::Storage`]; [`MlsError::Internal`] if nothing is pending.
    pub fn merge_pending_commit(&mut self) -> Result<CommitSummary, MlsError> {
        match self.unimplemented {}
    }

    /// Drops this member's pending commit after the server rejected it (another commit won the
    /// epoch).
    ///
    /// # Errors
    ///
    /// [`MlsError::Storage`].
    pub fn discard_pending_commit(&mut self) -> Result<(), MlsError> {
        match self.unimplemented {}
    }

    /// The current epoch's state snapshot key (`MLS-Exporter("pingers/v1/state", epoch)`).
    ///
    /// # Errors
    ///
    /// [`MlsError::Internal`] if the exporter fails.
    pub fn state_key(&self) -> Result<StateKey, MlsError> {
        match self.unimplemented {}
    }

    /// The current epoch's log-auth key pair (`MLS-Exporter("pingers/v1/log-auth", group_id)`).
    ///
    /// # Errors
    ///
    /// [`MlsError::Internal`] if the exporter fails.
    pub fn log_auth_key_pair(&self) -> Result<LogAuthKeyPair, MlsError> {
        match self.unimplemented {}
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
        let _ = (key_packages, rng);
        match self.unimplemented {}
    }

    /// `PR_e = MLS-Exporter("pingers/v1/presence", party_id, 32)` for the current epoch.
    ///
    /// # Errors
    ///
    /// [`MlsError::Internal`] if the exporter fails.
    pub fn presence_root(&self, party: &PartyId) -> Result<PresenceRoot, MlsError> {
        let _ = party;
        match self.unimplemented {}
    }
}

impl<S: MlsStorage> Group<'_, S, Meetup> {
    /// Adds newcomers, checking each KeyPackage's signature key against the Party roster
    /// (design B.5).
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
        let _ = (key_packages, party_roster, rng);
        match self.unimplemented {}
    }

    /// Every member's grant key from its `pingers_grant_key` leaf extension.
    ///
    /// # Errors
    ///
    /// [`MlsError::InvalidMessage`] if a leaf lacks the extension (it could not have joined).
    pub fn grant_keys(&self) -> Result<Vec<MemberGrantKey>, MlsError> {
        match self.unimplemented {}
    }
}
