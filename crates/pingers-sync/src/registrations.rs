// SPDX-License-Identifier: AGPL-3.0-only
//! Keeping this device's push registrations current (design B.4.9, E.2, F.3).
//!
//! Every mailbox and group subscription the device registered carries a sealed push. It goes
//! stale when the platform's push token changes, when the push relay rotates its key, when
//! "No wake-up pushes" is switched either way, or as its `not_after` comes near. Each wake
//! makes at most one push registration, so that the server cannot group a device's mailboxes
//! and subscriptions by when they were registered (design F.3, "kept separate"): it seals one
//! stale registration again and replaces it in place, or, when nothing is stale, registers one
//! mailbox for the pool. Each request is its own anonymous OHTTP request.
//!
//! In place means the same mailbox id and subscription id with a fresh handle and a fresh
//! seal. The server learns that a registration it already holds was renewed. The push relay,
//! if it logs, can link the old and new token of one device through the unchanged bind; a
//! fresh id would hide that but would break every invite link, board record and member
//! record that names the old one.

use std::collections::HashSet;
use std::time::Duration;

use pingers_crypto::{LogAuthKeyPair, push_seal_fingerprint};
use pingers_proto::{GroupId, MailboxId, MailboxPush, SubscriptionId, TimeToLive};
use pingers_store::{Mailbox, MailboxPurpose, SealStamp, StoredSubscription};
use pingers_time::UnixSeconds;

use crate::engine::blocking;
use crate::mailbox::sealed_push;
use crate::{
    ApiRequest, GroupCall, NotSentReason, PoolRefill, PoolTarget, PushSeal, SyncEngine, SyncError,
    WakeBudget, WakePush,
};

/// How long before a seal or a subscription runs out it is renewed: a few days of wakes, so a
/// device that is rarely woken still renews in time.
pub const RENEW_AHEAD: Duration = Duration::from_hours(3 * 24);

/// A group this device is in, and the key its own requests about the group are signed with.
pub struct GroupMembership {
    /// The group.
    pub group: GroupId,
    /// The current epoch's log-auth key.
    pub key: LogAuthKeyPair,
}

/// What one maintenance step works from.
pub struct Maintenance<'groups> {
    /// The knock mailbox pool to keep filled when no registration is stale.
    pub pool: PoolTarget,
    /// Every group this device is in (Parties and live Meetups). A stored subscription for a
    /// group not listed is forgotten; a listed group without one gets one.
    pub groups: &'groups [GroupMembership],
    /// The lifetime of a new or renewed subscription.
    pub subscription_lifetime: TimeToLive,
}

/// The one request a maintenance step made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Maintained {
    /// Nothing was stale and the pool was full.
    UpToDate,
    /// A subscription was added to a group that had none.
    SubscriptionAdded,
    /// A subscription was sealed again (new token or relay key) or renewed.
    SubscriptionResealed,
    /// A subscription was removed ("No wake-up pushes").
    SubscriptionRemoved,
    /// A mailbox's sealed push was replaced.
    MailboxResealed,
    /// A mailbox's sealed push was removed ("No wake-up pushes").
    MailboxPushRemoved,
    /// The pool was refilled (or found full) instead.
    Pool(PoolRefill),
    /// The request was not sent; the registration stays stale for a later wake.
    NotSent(NotSentReason),
}

/// What a registration sealed with `seal` records: the fingerprint of its relay key and token,
/// and its `not_after`. A wake push's two seals share both.
pub(crate) fn stamp_of(seal: &PushSeal) -> Result<SealStamp, SyncError> {
    Ok(SealStamp {
        fingerprint: push_seal_fingerprint(
            &seal.relay_key,
            seal.token.platform,
            &seal.token.token,
        )?,
        not_after: seal.token.not_after,
    })
}

/// The registration a maintenance step works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Task {
    AddSubscription(GroupId),
    ResealSubscription(GroupId),
    RemoveSubscription(GroupId),
    ResealMailbox(MailboxId),
    RemoveMailboxPush(MailboxId),
}

/// Why a registration is stale, most urgent first; within one reason, the earliest deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Urgency {
    /// No push at all where one belongs, or one where none may be.
    Missing,
    /// Sealed with an older token or relay key, or before seals were recorded.
    Stale(UnixSeconds),
    /// Sealed with the current token and key, but running out.
    Expiring(UnixSeconds),
}

/// The stored registrations a step chooses from.
pub(crate) struct Registrations<'rows> {
    pub(crate) mailboxes: &'rows [Mailbox],
    pub(crate) subscriptions: &'rows [StoredSubscription],
    pub(crate) groups: &'rows HashSet<GroupId>,
}

/// The most urgent stale registration, if any. Subscriptions of groups not in `groups` are
/// left out: the caller forgets them first.
pub(crate) fn next_task(
    rows: &Registrations<'_>,
    desired: Option<SealStamp>,
    now: UnixSeconds,
    subscription_lifetime: TimeToLive,
) -> Option<Task> {
    let Some(desired) = desired else {
        // "No wake-up pushes": no registration may carry a token.
        return rows
            .subscriptions
            .iter()
            .find(|subscription| rows.groups.contains(&subscription.group))
            .map(|subscription| Task::RemoveSubscription(subscription.group))
            .or_else(|| {
                rows.mailboxes
                    .iter()
                    .find(|mailbox| mailbox.push.is_some())
                    .map(|mailbox| Task::RemoveMailboxPush(mailbox.id()))
            });
    };
    let soon = now.checked_add(RENEW_AHEAD).unwrap_or(now);
    let renewed_subscription = now
        .checked_add(subscription_lifetime.duration())
        .unwrap_or(now)
        .min(desired.not_after);
    let subscribed: HashSet<GroupId> = rows
        .subscriptions
        .iter()
        .map(|subscription| subscription.group)
        .collect();
    let mut groups: Vec<&GroupId> = rows.groups.iter().collect();
    groups.sort_unstable_by_key(|group| group.as_bytes());
    let missing = groups
        .into_iter()
        .find(|group| !subscribed.contains(group))
        .map(|group| (Urgency::Missing, Task::AddSubscription(*group)));
    let subscriptions = rows
        .subscriptions
        .iter()
        .filter(|subscription| rows.groups.contains(&subscription.group))
        .filter_map(|subscription| {
            let deadline = subscription.seal.map_or(subscription.expires_at, |seal| {
                seal.not_after.min(subscription.expires_at)
            });
            let due = Due {
                deadline,
                renewed: renewed_subscription,
                soon,
            };
            urgency(subscription.seal, desired, due)
                .map(|urgency| (urgency, Task::ResealSubscription(subscription.group)))
        });
    let mailboxes = rows.mailboxes.iter().filter_map(|mailbox| {
        let urgency = match (mailbox.push, mailbox.seal) {
            (None, _) => Some(Urgency::Missing),
            // A seal that outlives its mailbox never needs renewing.
            (Some(_), Some(seal)) if seal.not_after >= mailbox.expires_at => {
                let due = Due {
                    deadline: UnixSeconds::new(u64::MAX),
                    renewed: desired.not_after,
                    soon,
                };
                urgency(Some(seal), desired, due)
            }
            (Some(_), seal) => {
                let due = Due {
                    deadline: seal.map_or(now, |seal| seal.not_after),
                    renewed: desired.not_after,
                    soon,
                };
                urgency(seal, desired, due)
            }
        };
        urgency.map(|urgency| (urgency, Task::ResealMailbox(mailbox.id())))
    });
    missing
        .into_iter()
        .chain(subscriptions)
        .chain(mailboxes)
        .min_by_key(|(urgency, _)| *urgency)
        .map(|(_, task)| task)
}

/// When a registration runs out, when it would run out if renewed now, and the end of the
/// renewal window.
#[derive(Clone, Copy)]
struct Due {
    deadline: UnixSeconds,
    renewed: UnixSeconds,
    soon: UnixSeconds,
}

/// Whether a registration sealed with `seal` needs sealing again, and how urgently. One that
/// is running out is renewed only if renewing gains time (a relay whose longest token is
/// shorter than the window would otherwise be renewed on every wake).
fn urgency(seal: Option<SealStamp>, desired: SealStamp, due: Due) -> Option<Urgency> {
    match seal {
        None => Some(Urgency::Stale(UnixSeconds::EPOCH)),
        Some(seal) if seal.fingerprint != desired.fingerprint => {
            Some(Urgency::Stale(seal.not_after))
        }
        Some(_) if due.deadline <= due.soon && due.renewed > due.deadline => {
            Some(Urgency::Expiring(due.deadline))
        }
        Some(_) => None,
    }
}

/// What one step chose from, for carrying out its task.
struct Step<'step, 'rows> {
    maintenance: &'step Maintenance<'step>,
    rows: &'step Registrations<'rows>,
    desired: Option<&'step (WakePush, SealStamp)>,
}

impl Step<'_, '_> {
    fn member(&self, group: GroupId) -> Result<&GroupMembership, SyncError> {
        self.maintenance
            .groups
            .iter()
            .find(|membership| membership.group == group)
            .ok_or(SyncError::Internal)
    }

    fn subscription(&self, group: GroupId) -> Result<SubscriptionId, SyncError> {
        self.rows
            .subscriptions
            .iter()
            .find(|subscription| subscription.group == group)
            .map(|subscription| subscription.id)
            .ok_or(SyncError::Internal)
    }

    fn purpose(&self, mailbox: MailboxId) -> Result<MailboxPurpose, SyncError> {
        self.rows
            .mailboxes
            .iter()
            .find(|row| row.id() == mailbox)
            .map(|row| row.purpose)
            .ok_or(SyncError::Internal)
    }
}

/// A subscription to put: its group, the id the server holds (`None` for a new one), the seal
/// and what it records, and its lifetime.
struct SubscriptionRenewal<'renewal> {
    member: &'renewal GroupMembership,
    id: Option<SubscriptionId>,
    seal: &'renewal PushSeal,
    stamp: SealStamp,
    lifetime: TimeToLive,
}

impl SyncEngine {
    /// The seal registrations should carry now, from the wake push the client set.
    fn desired(&self) -> Result<Option<(WakePush, SealStamp)>, SyncError> {
        let push = self
            .wake_push
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        push.map(|push| stamp_of(&push.alert).map(|desired| (push, desired)))
            .transpose()
    }

    /// One maintenance step: forgets subscriptions of groups this device has left (no
    /// request), then makes at most one request. An empty pool is refilled first (a lease
    /// needs a knock mailbox now); otherwise the most urgent stale registration is sealed
    /// again or cleared, and with none stale the pool is topped up. Not queued: what was not
    /// sent stays stale for the next wake.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] if the server refuses; [`SyncError::Store`]; as
    /// [`SyncEngine::send`].
    pub async fn maintain_push_registrations(
        &self,
        maintenance: &Maintenance<'_>,
        budget: WakeBudget,
    ) -> Result<Maintained, SyncError> {
        let now = self.now();
        let groups: HashSet<GroupId> = maintenance
            .groups
            .iter()
            .map(|membership| membership.group)
            .collect();
        let (mailboxes, subscriptions) = self.registrations(groups.clone(), now).await?;
        if self.pooled_mailboxes(maintenance.pool.purpose).await? == 0 {
            return self.refill(&maintenance.pool, budget).await;
        }
        let desired = self.desired()?;
        let rows = Registrations {
            mailboxes: &mailboxes,
            subscriptions: &subscriptions,
            groups: &groups,
        };
        let Some(task) = next_task(
            &rows,
            desired.as_ref().map(|(_, desired)| *desired),
            now,
            maintenance.subscription_lifetime,
        ) else {
            return self.refill(&maintenance.pool, budget).await;
        };
        let step = Step {
            maintenance,
            rows: &rows,
            desired: desired.as_ref(),
        };
        match self.perform(task, &step, budget).await {
            Err(SyncError::NoRelay { .. }) => Ok(Maintained::NotSent(NotSentReason::NoRelay)),
            Err(SyncError::BudgetExhausted { .. }) => {
                Ok(Maintained::NotSent(NotSentReason::BudgetExhausted))
            }
            other => other,
        }
    }

    /// The live mailboxes and the subscriptions of `groups`; subscriptions of other groups
    /// are forgotten here.
    async fn registrations(
        &self,
        groups: HashSet<GroupId>,
        now: UnixSeconds,
    ) -> Result<(Vec<Mailbox>, Vec<StoredSubscription>), SyncError> {
        blocking(&self.database, move |session| {
            let mut subscriptions = session.subscriptions().all()?;
            for gone in subscriptions
                .iter()
                .filter(|subscription| !groups.contains(&subscription.group))
            {
                session.subscriptions().delete(&gone.group)?;
            }
            subscriptions.retain(|subscription| groups.contains(&subscription.group));
            Ok((session.mailboxes().live(now)?, subscriptions))
        })
        .await
    }

    /// Makes the one request `task` needs.
    async fn perform(
        &self,
        task: Task,
        step: &Step<'_, '_>,
        budget: WakeBudget,
    ) -> Result<Maintained, SyncError> {
        match (task, step.desired) {
            (Task::AddSubscription(group), Some((push, desired))) => {
                let renewal = SubscriptionRenewal {
                    member: step.member(group)?,
                    id: None,
                    seal: &push.alert,
                    stamp: *desired,
                    lifetime: step.maintenance.subscription_lifetime,
                };
                self.put_sealed_subscription(&renewal, budget).await?;
                Ok(Maintained::SubscriptionAdded)
            }
            (Task::ResealSubscription(group), Some((push, desired))) => {
                let renewal = SubscriptionRenewal {
                    member: step.member(group)?,
                    id: Some(step.subscription(group)?),
                    seal: &push.alert,
                    stamp: *desired,
                    lifetime: step.maintenance.subscription_lifetime,
                };
                self.put_sealed_subscription(&renewal, budget).await?;
                Ok(Maintained::SubscriptionResealed)
            }
            (Task::RemoveSubscription(group), _) => {
                let member = step.member(group)?;
                let call = GroupCall {
                    group: &member.group,
                    key: &member.key,
                };
                self.delete_subscription(call, &step.subscription(group)?, budget)
                    .await?;
                Ok(Maintained::SubscriptionRemoved)
            }
            (Task::ResealMailbox(mailbox), Some((push, desired))) => {
                let seal = match step.purpose(mailbox)? {
                    MailboxPurpose::Knock => &push.knock,
                    MailboxPurpose::Reply | MailboxPurpose::Invite => &push.alert,
                };
                self.replace_mailbox_push(&mailbox, Some((seal, *desired)), budget)
                    .await?;
                Ok(Maintained::MailboxResealed)
            }
            (Task::RemoveMailboxPush(mailbox), _) => {
                self.replace_mailbox_push(&mailbox, None, budget).await?;
                Ok(Maintained::MailboxPushRemoved)
            }
            // A task that needs a push is only chosen with one.
            (
                Task::AddSubscription(_) | Task::ResealSubscription(_) | Task::ResealMailbox(_),
                None,
            ) => Err(SyncError::Internal),
        }
    }

    async fn refill(&self, pool: &PoolTarget, budget: WakeBudget) -> Result<Maintained, SyncError> {
        let refilled = self.refill_mailbox_pool(pool, budget).await?;
        Ok(match refilled {
            PoolRefill::Full { .. } => Maintained::UpToDate,
            other @ (PoolRefill::Registered { .. } | PoolRefill::NotSent(_)) => {
                Maintained::Pool(other)
            }
        })
    }

    /// `PUT /v1/groups/{id}/subs/{sub_id}` with a fresh seal and handle, under the stored id
    /// (or a new one), and records it with its seal.
    async fn put_sealed_subscription(
        &self,
        renewal: &SubscriptionRenewal<'_>,
        budget: WakeBudget,
    ) -> Result<(), SyncError> {
        let subscription = self.seal_subscription(renewal.id, renewal.seal, renewal.lifetime)?;
        self.note_issued(subscription.push.handle, renewal.stamp);
        let call = GroupCall {
            group: &renewal.member.group,
            key: &renewal.member.key,
        };
        self.put_subscription(call, &subscription, budget).await
    }

    /// `PUT /v1/mailboxes/{id}/push`, then the stored handle and seal.
    async fn replace_mailbox_push(
        &self,
        mailbox: &MailboxId,
        seal: Option<(&PushSeal, SealStamp)>,
        budget: WakeBudget,
    ) -> Result<(), SyncError> {
        let push = seal
            .map(|(seal, _)| {
                self.with_rng(|rng| {
                    sealed_push(seal, pingers_crypto::push_bind_for_mailbox(mailbox), rng)
                })
            })
            .transpose()?;
        let handle = push.as_ref().map(|push| push.handle);
        let stamp = seal.map(|(_, desired)| desired);
        let update = MailboxPush { push };
        let id = *mailbox;
        let request = blocking(&self.database, move |session| {
            let stored = session
                .mailboxes()
                .get(&id)?
                .ok_or(SyncError::UnknownMailbox)?;
            ApiRequest::replace_mailbox_push(&id, &stored.read_key, &update)
        })
        .await?;
        self.exchange(request, budget).await?.success()?;
        blocking(&self.database, move |session| {
            Ok(session.mailboxes().set_push(&id, handle, stamp)?)
        })
        .await
    }
}

#[cfg(test)]
mod tests;
