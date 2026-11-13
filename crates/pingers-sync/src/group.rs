// SPDX-License-Identifier: AGPL-3.0-only
//! Group logs (design B.2, D): fetch the log and hand each entry to the group's owner in
//! order; post commits, exactly one per epoch, re-fetching and re-applying on conflict.
//!
//! The owner (pingers-parties for Party groups, pingers-meetup-session for Meetup groups)
//! holds the MLS state and decides what every entry means; this module only moves bytes.

use std::num::NonZeroU32;

use pingers_crypto::LogAuthKeyPair;
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, CreateGroup, Epoch, GroupId, GroupLogEntry,
    GroupMessages, GroupState, MessageAccepted, SequenceNumber, Subscription, SubscriptionId,
};

use crate::mailbox::Delivered;
use crate::{
    ApiRequest, GroupError, GroupSigner, GroupStart, Sending, SyncEngine, SyncError, WakeBudget,
};

/// How many times a commit is tried before the conflicts are reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommitAttempts(NonZeroU32);

impl CommitAttempts {
    /// Wraps a count.
    #[must_use]
    pub const fn new(attempts: NonZeroU32) -> Self {
        Self(attempts)
    }

    /// The count.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl Default for CommitAttempts {
    /// Three: each conflict means another member committed first, so after re-applying
    /// their commit a second try normally wins; three covers a burst.
    fn default() -> Self {
        Self(NonZeroU32::MIN.saturating_add(2))
    }
}

/// Whoever holds a group's MLS state and applies its log.
pub trait GroupLogOwner: Send {
    /// What applying can fail with.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The last log position already applied; `None` before the first.
    fn applied_through(&self) -> Option<SequenceNumber>;

    /// The current epoch's log-auth key, which signs the next request.
    fn log_auth_key(&self) -> &LogAuthKeyPair;

    /// Applies one entry, in log order. An entry the owner cannot use (an old epoch, a
    /// proposal it ignores) is still consumed: `applied_through` must move past it.
    ///
    /// # Errors
    ///
    /// The owner's own; the pull stops at this entry.
    fn apply(&mut self, entry: GroupLogEntry) -> Result<(), Self::Error>;

    /// The entry at `sequence` is one this device posted (its position came back from the
    /// post): it is not handed to [`GroupLogOwner::apply`], since an own commit was merged by
    /// `commit_accepted` and an own application message cannot be decrypted by its sender.
    /// `applied_through` must still move past it.
    ///
    /// # Errors
    ///
    /// The owner's own; the pull stops at this entry.
    fn skip_own(&mut self, sequence: SequenceNumber) -> Result<(), Self::Error>;
}

/// An owner that also makes commits.
pub trait CommitOwner: GroupLogOwner {
    /// The commit to post for the current epoch, made after everything fetched so far was
    /// applied; `None` if there is nothing left to commit (another member's commit already
    /// did it).
    ///
    /// # Errors
    ///
    /// The owner's own.
    fn prepare_commit(&mut self) -> Result<Option<CommitRequest>, Self::Error>;

    /// The server stored the prepared commit at `sequence`: merge it.
    ///
    /// # Errors
    ///
    /// The owner's own.
    fn commit_accepted(&mut self, sequence: SequenceNumber) -> Result<(), Self::Error>;

    /// Another commit took the epoch (`head_epoch` is the log's now): discard the prepared
    /// one. The engine then fetches and applies the winner before asking again.
    ///
    /// # Errors
    ///
    /// The owner's own.
    fn commit_rejected(&mut self, head_epoch: Epoch) -> Result<(), Self::Error>;
}

/// A group and the current epoch's log-auth key, which together address a Grp request.
#[derive(Clone, Copy)]
pub struct GroupCall<'call> {
    /// The group.
    pub group: &'call GroupId,
    /// The current epoch's log-auth key pair.
    pub key: &'call LogAuthKeyPair,
}

/// What a commit loop ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Committed {
    /// Stored at this log position.
    Accepted(SequenceNumber),
    /// The owner had nothing left to commit after catching up.
    NothingToCommit,
}

/// What a pull applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct Pulled {
    /// Entries handed to the owner.
    pub applied: usize,
    /// This device's own entries, passed over (`skip_own`).
    pub skipped: usize,
    /// The log's epoch, from the last page.
    pub head_epoch: Option<Epoch>,
}

impl SyncEngine {
    /// The positions of this device's entries in `group` not yet pulled (pingers-store's
    /// `own_group_entry`, so they outlive a restart).
    async fn own_entries(&self, group: &GroupId) -> Result<Vec<SequenceNumber>, SyncError> {
        let group = *group;
        crate::engine::blocking(&self.database, move |session| {
            Ok(session.own_group_entries().positions(&group)?)
        })
        .await
    }

    /// Forgets own positions in `group` up to `through`: the pull has handed them over.
    async fn forget_own_entries(
        &self,
        group: &GroupId,
        through: Option<SequenceNumber>,
    ) -> Result<(), SyncError> {
        let Some(through) = through else {
            return Ok(());
        };
        let group = *group;
        crate::engine::blocking(&self.database, move |session| {
            Ok(session
                .own_group_entries()
                .forget_through(&group, through)?)
        })
        .await
    }

    fn signer<'key>(&self, key: &'key LogAuthKeyPair) -> GroupSigner<'key> {
        GroupSigner {
            key,
            now: self.now(),
        }
    }

    /// One page of a group's log after `after` (`None`: from the start).
    ///
    /// # Errors
    ///
    /// [`SyncError::NoRelay`]; [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn read_group_messages(
        &self,
        call: GroupCall<'_>,
        after: Option<SequenceNumber>,
        budget: WakeBudget,
    ) -> Result<GroupMessages, SyncError> {
        let request = ApiRequest::read_group_messages(call.group, after, &self.signer(call.key))?;
        self.exchange(request, budget).await?.success()?.decode()
    }

    /// Fetches everything after what `owner` has applied and hands it over in order, page by
    /// page, until a page comes back empty. Each page is signed with the owner's key as it
    /// stands, since an applied commit changes it.
    ///
    /// # Errors
    ///
    /// [`GroupError::Owner`] if applying fails (the entries before it stay applied);
    /// [`GroupError::Sync`] otherwise.
    pub async fn pull_group<O: GroupLogOwner>(
        &self,
        group: &GroupId,
        owner: &mut O,
        budget: WakeBudget,
    ) -> Result<Pulled, GroupError<O::Error>> {
        let mut pulled = Pulled {
            applied: 0,
            skipped: 0,
            head_epoch: None,
        };
        loop {
            let page = self
                .read_group_messages(
                    GroupCall {
                        group,
                        key: owner.log_auth_key(),
                    },
                    owner.applied_through(),
                    budget,
                )
                .await?;
            pulled.head_epoch = Some(page.head_epoch);
            if page.messages.is_empty() {
                return Ok(pulled);
            }
            let own = self.own_entries(group).await?;
            // The last position handed over: own positions up to it are forgotten after the
            // page. If the owner fails midway, the next page's forget covers them, since
            // positions only grow.
            let mut handed = None;
            for entry in page.messages {
                let sequence = entry.sequence;
                if own.contains(&sequence) {
                    owner.skip_own(sequence).map_err(GroupError::Owner)?;
                    pulled.skipped = pulled.skipped.saturating_add(1);
                } else {
                    owner.apply(entry).map_err(GroupError::Owner)?;
                    pulled.applied = pulled.applied.saturating_add(1);
                }
                handed = Some(sequence);
            }
            self.forget_own_entries(group, handed).await?;
        }
    }

    /// Posts a commit for the owner's current epoch, exactly one per epoch: catch up, ask the
    /// owner for its commit, post it; on `409` (another commit took the epoch) tell the
    /// owner, catch up again and retry, up to [`CommitAttempts`]. Commits are never queued:
    /// one prepared against a stale epoch would only conflict later.
    ///
    /// # Errors
    ///
    /// [`GroupError::ConflictsExhausted`]; [`GroupError::Owner`]; [`GroupError::Sync`].
    pub async fn commit<O: CommitOwner>(
        &self,
        group: &GroupId,
        owner: &mut O,
        budget: WakeBudget,
    ) -> Result<Committed, GroupError<O::Error>> {
        let attempts = self.settings().commit_attempts.get();
        for _attempt in 0..attempts {
            let _: Pulled = self.pull_group(group, owner, budget).await?;
            let Some(commit) = owner.prepare_commit().map_err(GroupError::Owner)? else {
                return Ok(Committed::NothingToCommit);
            };
            let request = ApiRequest::commit(group, &commit, &self.signer(owner.log_auth_key()))?;
            let response = self.exchange(request, budget).await?;
            // 200 carries `Accepted`, 409 carries `Conflict`; both bodies are `CommitResponse`.
            if response.status() != 409 {
                response.success_ref()?;
            }
            match response.decode::<CommitResponse>()? {
                CommitResponse::Accepted { sequence } => {
                    owner.commit_accepted(sequence).map_err(GroupError::Owner)?;
                    return Ok(Committed::Accepted(sequence));
                }
                CommitResponse::Conflict { head_epoch } => {
                    owner
                        .commit_rejected(head_epoch)
                        .map_err(GroupError::Owner)?;
                }
            }
        }
        Err(GroupError::ConflictsExhausted { attempts })
    }

    /// Creates a group (`POST /v1/groups`, `Group` credential). Not queued: the creator
    /// needs the group now (a Meetup answering a knock, a new Party).
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] (409 if the id exists); as [`SyncEngine::send`].
    pub async fn create_group(
        &self,
        request: &CreateGroup,
        start: GroupStart,
        budget: WakeBudget,
    ) -> Result<(), SyncError> {
        self.exchange(ApiRequest::create_group(request, start)?, budget)
            .await?
            .success()
            .map(drop)
    }

    /// Posts an application message (chat, meet point, roster profile) to the log.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn post_group_message(
        &self,
        call: GroupCall<'_>,
        message: &AppMessage,
        sending: Sending,
    ) -> Result<Delivered<Option<SequenceNumber>>, SyncError> {
        let request = ApiRequest::post_group_message(call.group, message, &self.signer(call.key))?;
        let outcome = self.send(request, sending).await?;
        Delivered::from_outcome(outcome, |response| {
            // An ephemeral message is forwarded, not stored, and has no position.
            if response.body().is_empty() {
                Ok(None)
            } else {
                Ok(Some(response.decode::<MessageAccepted>()?.sequence))
            }
        })
    }

    /// Renews or replaces this device's subscription in `group`, and records it.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn put_subscription(
        &self,
        call: GroupCall<'_>,
        subscription: &Subscription,
        budget: WakeBudget,
    ) -> Result<(), SyncError> {
        let request =
            ApiRequest::put_subscription(call.group, subscription, &self.signer(call.key))?;
        self.exchange(request, budget).await?.success()?;
        self.remember_subscription(call.group, subscription).await
    }

    /// Removes this device's subscription from `group` on the server and locally.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn delete_subscription(
        &self,
        call: GroupCall<'_>,
        subscription: &SubscriptionId,
        budget: WakeBudget,
    ) -> Result<(), SyncError> {
        let request =
            ApiRequest::delete_subscription(call.group, subscription, &self.signer(call.key))?;
        let response = self.exchange(request, budget).await?;
        if response.status() != 404 {
            response.success()?;
        }
        let group = *call.group;
        crate::engine::blocking(&self.database, move |session| {
            Ok(session.subscriptions().delete(&group)?)
        })
        .await
    }

    /// The group's latest sealed state snapshot (a joiner's first read).
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] (404 if none was uploaded); as [`SyncEngine::send`].
    pub async fn group_state(
        &self,
        call: GroupCall<'_>,
        budget: WakeBudget,
    ) -> Result<GroupState, SyncError> {
        let request = ApiRequest::group_state(call.group, &self.signer(call.key))?;
        self.exchange(request, budget).await?.success()?.decode()
    }

    /// Deletes a group (a Meetup that ended).
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn delete_group(
        &self,
        call: GroupCall<'_>,
        budget: WakeBudget,
    ) -> Result<(), SyncError> {
        let request = ApiRequest::delete_group(call.group, &self.signer(call.key))?;
        let response = self.exchange(request, budget).await?;
        if response.status() != 404 {
            response.success()?;
        }
        Ok(())
    }
}
