// SPDX-License-Identifier: AGPL-3.0-only
//! An in-memory [`DeliveryStore`], for tests and for servers that keep nothing on disk.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};

use pingers_proto::{
    ContentType, Epoch, GroupAuthKey, GroupId, GroupLogEntry, GroupState, SequenceNumber,
    SubscriptionId,
};
use pingers_time::UnixSeconds;

use crate::store::{
    CommitWrite, DeliveryStore, GroupHead, NewCommit, NewEntry, NewGroup, PAGE_ENTRIES,
    StoredSubscription,
};
use crate::{DeliveryError, GroupLifetime};

/// Keeps every group in a map behind one lock.
#[derive(Debug, Default)]
pub struct MemoryDeliveryStore {
    groups: Mutex<HashMap<GroupId, Group>>,
}

#[derive(Debug)]
struct Group {
    head_epoch: Epoch,
    auth_key: GroupAuthKey,
    lifetime: GroupLifetime,
    idle_expires_at: UnixSeconds,
    log: Vec<GroupLogEntry>,
    state: Option<GroupState>,
    subscriptions: BTreeMap<SubscriptionId, StoredSubscription>,
}

impl Group {
    fn push(
        &mut self,
        epoch: Epoch,
        content_type: ContentType,
        body: pingers_proto::MlsMessageBytes,
    ) -> Result<SequenceNumber, DeliveryError> {
        let sequence = match self.log.last() {
            Some(last) => last
                .sequence
                .checked_next()
                .ok_or(DeliveryError::CounterOverflow)?,
            None => SequenceNumber::new(1),
        };
        self.log.push(GroupLogEntry {
            sequence,
            epoch,
            content_type,
            body,
        });
        Ok(sequence)
    }
}

impl MemoryDeliveryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<MutexGuard<'_, HashMap<GroupId, Group>>, DeliveryError> {
        self.groups.lock().map_err(|_| DeliveryError::Poisoned)
    }

    fn with_group<R>(
        &self,
        group: &GroupId,
        action: impl FnOnce(&mut Group) -> Result<R, DeliveryError>,
    ) -> Result<R, DeliveryError> {
        let mut groups = self.lock()?;
        let found = groups.get_mut(group).ok_or(DeliveryError::UnknownGroup)?;
        action(found)
    }
}

impl DeliveryStore for MemoryDeliveryStore {
    fn create_group(&self, group: NewGroup) -> Result<(), DeliveryError> {
        let mut groups = self.lock()?;
        if groups.contains_key(&group.id) {
            return Err(DeliveryError::GroupExists);
        }
        let epoch = group.epoch;
        groups.insert(
            group.id,
            Group {
                head_epoch: epoch,
                auth_key: group.auth_key,
                lifetime: group.lifetime,
                idle_expires_at: group.idle_expires_at,
                log: Vec::new(),
                state: group.state.map(|state| GroupState { epoch, state }),
                subscriptions: group
                    .subscriptions
                    .into_iter()
                    .map(|stored| (stored.subscription.id, stored))
                    .collect(),
            },
        );
        Ok(())
    }

    fn head(&self, group: &GroupId) -> Result<GroupHead, DeliveryError> {
        self.with_group(group, |found| {
            Ok(GroupHead {
                head_epoch: found.head_epoch,
                auth_key: found.auth_key,
                lifetime: found.lifetime,
            })
        })
    }

    fn touch(&self, group: &GroupId, idle_expires_at: UnixSeconds) -> Result<(), DeliveryError> {
        self.with_group(group, |found| {
            found.idle_expires_at = idle_expires_at;
            Ok(())
        })
    }

    fn append(
        &self,
        group: &GroupId,
        entry: NewEntry,
        idle_expires_at: UnixSeconds,
    ) -> Result<SequenceNumber, DeliveryError> {
        self.with_group(group, |found| {
            let sequence = found.push(entry.epoch, ContentType::Application, entry.body)?;
            found.idle_expires_at = idle_expires_at;
            Ok(sequence)
        })
    }

    fn append_commit(
        &self,
        group: &GroupId,
        commit: NewCommit,
        idle_expires_at: UnixSeconds,
    ) -> Result<CommitWrite, DeliveryError> {
        self.with_group(group, |found| {
            if found.head_epoch != commit.epoch {
                return Ok(CommitWrite::EpochTaken {
                    head_epoch: found.head_epoch,
                });
            }
            let sequence = found.push(commit.epoch, ContentType::Commit, commit.commit)?;
            found.head_epoch = commit.next_epoch;
            found.auth_key = commit.next_auth_key;
            found.idle_expires_at = idle_expires_at;
            if let Some(state) = commit.state {
                found.state = Some(GroupState {
                    epoch: commit.next_epoch,
                    state,
                });
            }
            for removed in &commit.subscriptions_removed {
                found.subscriptions.remove(removed);
            }
            for added in commit.subscriptions_added {
                found.subscriptions.insert(added.subscription.id, added);
            }
            Ok(CommitWrite::Appended { sequence })
        })
    }

    fn entries_after(
        &self,
        group: &GroupId,
        after: SequenceNumber,
    ) -> Result<Vec<GroupLogEntry>, DeliveryError> {
        self.with_group(group, |found| {
            Ok(found
                .log
                .iter()
                .filter(|entry| entry.sequence > after)
                .take(PAGE_ENTRIES)
                .cloned()
                .collect())
        })
    }

    fn state(&self, group: &GroupId) -> Result<Option<GroupState>, DeliveryError> {
        self.with_group(group, |found| Ok(found.state.clone()))
    }

    fn put_subscription(
        &self,
        group: &GroupId,
        subscription: StoredSubscription,
        idle_expires_at: UnixSeconds,
    ) -> Result<(), DeliveryError> {
        self.with_group(group, |found| {
            found
                .subscriptions
                .insert(subscription.subscription.id, subscription);
            found.idle_expires_at = idle_expires_at;
            Ok(())
        })
    }

    fn remove_subscription(
        &self,
        group: &GroupId,
        subscription: &SubscriptionId,
    ) -> Result<(), DeliveryError> {
        self.with_group(group, |found| {
            found.subscriptions.remove(subscription);
            Ok(())
        })
    }

    fn live_subscriptions(
        &self,
        group: &GroupId,
        now: UnixSeconds,
    ) -> Result<Vec<StoredSubscription>, DeliveryError> {
        self.with_group(group, |found| {
            Ok(found
                .subscriptions
                .values()
                .filter(|stored| stored.expires_at > now)
                .cloned()
                .collect())
        })
    }

    fn delete_group(&self, group: &GroupId) -> Result<(), DeliveryError> {
        self.lock()?
            .remove(group)
            .map(drop)
            .ok_or(DeliveryError::UnknownGroup)
    }

    fn expire(&self, now: UnixSeconds) -> Result<Vec<GroupId>, DeliveryError> {
        let mut groups = self.lock()?;
        let mut expired: Vec<GroupId> = groups
            .iter()
            .filter(|(_, group)| group.idle_expires_at <= now)
            .map(|(id, _)| *id)
            .collect();
        expired.sort_unstable();
        for id in &expired {
            groups.remove(id);
        }
        for group in groups.values_mut() {
            group
                .subscriptions
                .retain(|_, stored| stored.expires_at > now);
        }
        Ok(expired)
    }
}
