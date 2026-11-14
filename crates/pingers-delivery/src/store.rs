// SPDX-License-Identifier: AGPL-3.0-only
//! The storage trait the delivery service owns, and the rows it reads and writes.
//!
//! Implementations hold only opaque bytes and counters (design C.1, tables `grp`, `grp_msg`,
//! `grp_sub`, `grp_state`). They decide nothing about epochs beyond the one atomic rule that
//! must hold under concurrency: a commit is appended only if its epoch is the log's head epoch.

use pingers_proto::{
    Epoch, GroupAuthKey, GroupId, GroupLogEntry, GroupState, MlsMessageBytes, SealedSnapshot,
    SequenceNumber, Subscription, SubscriptionId,
};
use pingers_time::UnixSeconds;

use crate::{DeliveryError, GroupLifetime};

/// The most log entries one read returns; the client asks again after the last one it got.
pub const PAGE_ENTRIES: usize = 512;

/// A subscription as stored: the sealed push and when the server drops it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSubscription {
    /// The subscription as the member sent it.
    pub subscription: Subscription,
    /// When it stops being woken and is deleted.
    pub expires_at: UnixSeconds,
}

/// A new group (`POST /v1/groups`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewGroup {
    /// Its id.
    pub id: GroupId,
    /// The starting epoch's log-auth key.
    pub auth_key: GroupAuthKey,
    /// The epoch the log starts at (the members' epoch at upload).
    pub epoch: Epoch,
    /// The first snapshot, for the starting epoch.
    pub state: Option<SealedSnapshot>,
    /// The first subscriptions.
    pub subscriptions: Vec<StoredSubscription>,
    /// How long it may be idle; fixed for the group's life.
    pub lifetime: GroupLifetime,
    /// When the group expires unless something touches it first.
    pub idle_expires_at: UnixSeconds,
}

/// What the service needs to know about a group before acting on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupHead {
    /// The epoch the next commit must be made in.
    pub head_epoch: Epoch,
    /// The key that authorises requests in the head epoch.
    pub auth_key: GroupAuthKey,
    /// Its lifetime class, which sets how far each touch moves the idle expiry.
    pub lifetime: GroupLifetime,
}

/// An earlier epoch's log-auth key, with the epoch it signed requests in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PastAuthKey {
    /// The epoch whose requests it signed.
    pub epoch: Epoch,
    /// The key.
    pub key: GroupAuthKey,
}

/// A stored non-commit message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewEntry {
    /// The epoch from its cleartext header.
    pub epoch: Epoch,
    /// The MLS message.
    pub body: MlsMessageBytes,
}

/// A commit to append if its epoch is still the head epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewCommit {
    /// The epoch it was made in; must equal the head epoch.
    pub epoch: Epoch,
    /// `epoch + 1`, which becomes the head epoch.
    pub next_epoch: Epoch,
    /// The commit.
    pub commit: MlsMessageBytes,
    /// The next epoch's log-auth key.
    pub next_auth_key: GroupAuthKey,
    /// A snapshot for the next epoch, replacing the old one.
    pub state: Option<SealedSnapshot>,
    /// Subscriptions to add or replace.
    pub subscriptions_added: Vec<StoredSubscription>,
    /// Subscriptions to delete.
    pub subscriptions_removed: Vec<SubscriptionId>,
}

/// The result of [`DeliveryStore::append_commit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum CommitWrite {
    /// The commit is the epoch's one commit.
    Appended {
        /// Its log position.
        sequence: SequenceNumber,
    },
    /// Another commit already took the epoch; nothing was written.
    EpochTaken {
        /// The log's head epoch now.
        head_epoch: Epoch,
    },
}

/// Durable state of the delivery service. Every method is atomic; implementations are shared
/// between request handlers, hence `&self` and `Send + Sync`.
///
/// Sequence numbers start at 1 and increase by one per stored entry within a group.
pub trait DeliveryStore: Send + Sync {
    /// Creates a group at `group.epoch` with an empty log.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::GroupExists`] if the id is taken; storage errors.
    fn create_group(&self, group: NewGroup) -> Result<(), DeliveryError>;

    /// The group's head epoch, current log-auth key and lifetime class.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn head(&self, group: &GroupId) -> Result<GroupHead, DeliveryError>;

    /// The log-auth keys of the group's earlier epochs with their epochs, newest first: a
    /// member that missed a commit still signs with one of them until it has read and applied
    /// that commit.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn past_auth_keys(&self, group: &GroupId) -> Result<Vec<PastAuthKey>, DeliveryError>;

    /// Moves the group's idle expiry to `idle_expires_at`.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn touch(&self, group: &GroupId, idle_expires_at: UnixSeconds) -> Result<(), DeliveryError>;

    /// Appends an application message and moves the idle expiry.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::CounterOverflow`]; storage errors.
    fn append(
        &self,
        group: &GroupId,
        entry: NewEntry,
        idle_expires_at: UnixSeconds,
    ) -> Result<SequenceNumber, DeliveryError>;

    /// Appends a commit, advances the head epoch, swaps the auth key (keeping the old one
    /// among the past keys), replaces the snapshot if
    /// one is given and applies the subscription changes, all at once and only if
    /// `commit.epoch` is the head epoch. Otherwise writes nothing and reports the head.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::CounterOverflow`]; storage errors.
    fn append_commit(
        &self,
        group: &GroupId,
        commit: NewCommit,
        idle_expires_at: UnixSeconds,
    ) -> Result<CommitWrite, DeliveryError>;

    /// Up to [`PAGE_ENTRIES`] entries with a sequence number above `after`, oldest first.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn entries_after(
        &self,
        group: &GroupId,
        after: SequenceNumber,
    ) -> Result<Vec<GroupLogEntry>, DeliveryError>;

    /// The position of the commit made in `epoch` (the one that ended it), if the log has one.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn commit_position(
        &self,
        group: &GroupId,
        epoch: Epoch,
    ) -> Result<Option<SequenceNumber>, DeliveryError>;

    /// The latest snapshot, if any was uploaded.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn state(&self, group: &GroupId) -> Result<Option<GroupState>, DeliveryError>;

    /// Adds a subscription, or replaces the one with the same id, and moves the idle expiry.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn put_subscription(
        &self,
        group: &GroupId,
        subscription: StoredSubscription,
        idle_expires_at: UnixSeconds,
    ) -> Result<(), DeliveryError>;

    /// Deletes a subscription; deleting one that does not exist is not an error.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn remove_subscription(
        &self,
        group: &GroupId,
        subscription: &SubscriptionId,
    ) -> Result<(), DeliveryError>;

    /// The subscriptions that have not expired at `now`, in id order.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn live_subscriptions(
        &self,
        group: &GroupId,
        now: UnixSeconds,
    ) -> Result<Vec<StoredSubscription>, DeliveryError>;

    /// Deletes the group with its log, snapshot and subscriptions.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    fn delete_group(&self, group: &GroupId) -> Result<(), DeliveryError>;

    /// Deletes every group whose idle expiry is at or before `now`, and every subscription
    /// whose expiry is. Returns the deleted groups.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn expire(&self, now: UnixSeconds) -> Result<Vec<GroupId>, DeliveryError>;
}
