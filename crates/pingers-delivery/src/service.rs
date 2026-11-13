// SPDX-License-Identifier: AGPL-3.0-only
//! The delivery service: the rules on top of a [`DeliveryStore`] and the live registry.

use core::time::Duration;
use std::sync::Arc;

use pingers_crypto::{PushBind, push_bind_for_subscription};
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, CreateGroup, Delivery as DeliveryMode, Epoch,
    GroupAuthKey, GroupId, GroupMessages, GroupState, LiveFrame, MlsMessageBytes, SealedPush,
    SequenceNumber, Subscription, SubscriptionId, Urgency,
};
use pingers_time::{Clock, UnixSeconds};
use rand_core::CryptoRng;

use crate::live::{ChannelId, LiveRegistry};
use crate::store::{CommitWrite, DeliveryStore, NewCommit, NewEntry, NewGroup, StoredSubscription};
use crate::{DeliveryError, GroupLifetime, GroupStart};

/// The longest a subscription may live: Party subscriptions are renewed every 30 days
/// (design B.4.9).
pub const MAX_SUBSCRIPTION_LIFETIME: Duration = Duration::from_hours(30 * 24);

/// `Quiet` wakes are each delayed by a random 0 to 30 seconds to blur Party fan-out at the push
/// relay (design B.4.9); alerts are not delayed.
pub const MAX_QUIET_DELAY: Duration = Duration::from_secs(30);

/// One sealed push to hand to the push relay (design D, "Wake rule"). The server sends
/// `{ blob, bind, urgency, handle }` after `delay`, and calls
/// [`Delivery::remove_subscription`] if the relay answers `410 Gone`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wake {
    /// Which subscription this is, to delete it on `410 Gone`.
    pub subscription: SubscriptionId,
    /// The relay and the sealed token, as the member registered them.
    pub push: SealedPush,
    /// `SHA-256("pingers/v1/push-bind" || sub_id)`, which the relay checks the blob against.
    pub bind: PushBind,
    /// The urgency the request asked for (not the one stored with the subscription).
    pub urgency: Urgency,
    /// How long to wait before forwarding.
    pub delay: Duration,
}

/// The answer to a commit.
#[derive(Debug)]
#[must_use]
pub enum CommitOutcome<T> {
    /// The commit is the epoch's one commit.
    Accepted {
        /// Its log position.
        sequence: SequenceNumber,
        /// Pushes for members not on a live channel.
        wakes: Vec<Wake>,
        /// Live channels to forward the commit to.
        targets: Vec<T>,
    },
    /// Another commit won the epoch (HTTP 409).
    Conflict {
        /// The log's head epoch.
        head_epoch: Epoch,
    },
}

impl<T> CommitOutcome<T> {
    /// The wire answer.
    #[must_use]
    pub fn response(&self) -> CommitResponse {
        match self {
            Self::Accepted { sequence, .. } => CommitResponse::Accepted {
                sequence: *sequence,
            },
            Self::Conflict { head_epoch } => CommitResponse::Conflict {
                head_epoch: *head_epoch,
            },
        }
    }
}

/// The answer to `POST /v1/groups/{id}/msgs`.
#[derive(Debug)]
#[must_use]
pub struct Posted<T> {
    /// Its log position; `None` for an ephemeral message, which is not stored.
    pub sequence: Option<SequenceNumber>,
    /// Pushes for members not on a live channel; always empty for an ephemeral message.
    pub wakes: Vec<Wake>,
    /// Live channels to forward the message to.
    pub targets: Vec<T>,
}

/// What to do with a frame that arrived on a live channel.
#[derive(Debug)]
#[must_use]
pub struct Forwarded<T> {
    /// Its log position if the frame is stored (chat); `None` if ephemeral (locations).
    pub sequence: Option<SequenceNumber>,
    /// The group's other live channels.
    pub targets: Vec<T>,
}

/// The MLS delivery service for Party and Meetup groups alike (design B.2, B.5, D).
///
/// `T` is the caller's handle for a live channel; it is cloned once per fan-out target.
/// Group-auth signatures (`Grp`) are checked by the caller against [`Delivery::auth_key`]
/// before calling the other methods.
pub struct Delivery<S, T> {
    store: S,
    clock: Arc<dyn Clock>,
    live: LiveRegistry<T>,
}

impl<S: DeliveryStore, T: Clone> Delivery<S, T> {
    /// A service over `store`, reading time from `clock`.
    pub fn new(store: S, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            clock,
            live: LiveRegistry::new(),
        }
    }

    /// The store, for maintenance and tests.
    pub const fn store(&self) -> &S {
        &self.store
    }

    fn idle_expiry(
        now: UnixSeconds,
        lifetime: GroupLifetime,
    ) -> Result<UnixSeconds, DeliveryError> {
        now.checked_add(lifetime.idle_lifetime())
            .ok_or(DeliveryError::TimeOutOfRange)
    }

    fn stored(
        now: UnixSeconds,
        subscription: Subscription,
    ) -> Result<StoredSubscription, DeliveryError> {
        let lifetime = subscription.time_to_live.duration();
        if lifetime > MAX_SUBSCRIPTION_LIFETIME {
            return Err(DeliveryError::SubscriptionTooLong);
        }
        let expires_at = now
            .checked_add(lifetime)
            .ok_or(DeliveryError::TimeOutOfRange)?;
        Ok(StoredSubscription {
            subscription,
            expires_at,
        })
    }

    /// `POST /v1/groups`: a new group at `start.epoch`, of lifetime class `start.lifetime`.
    /// Wakes nobody: the creator delivers Welcomes through mailboxes.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::GroupExists`]; [`DeliveryError::SubscriptionTooLong`]; storage errors.
    pub fn create_group(
        &self,
        request: CreateGroup,
        start: GroupStart,
    ) -> Result<(), DeliveryError> {
        let GroupStart { lifetime, epoch } = start;
        let now = self.clock.now();
        let subscriptions = request
            .subscriptions
            .into_iter()
            .map(|subscription| Self::stored(now, subscription))
            .collect::<Result<Vec<_>, _>>()?;
        self.store.create_group(NewGroup {
            id: request.group_id,
            auth_key: request.auth_key,
            epoch,
            state: request.state,
            subscriptions,
            lifetime,
            idle_expires_at: Self::idle_expiry(now, lifetime)?,
        })
    }

    /// `POST /v1/groups/{id}/commit`: accepts exactly one commit per epoch. The winner's
    /// subscription changes apply before the wake, so added members are woken and removed
    /// ones are not.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::SubscriptionTooLong`];
    /// [`DeliveryError::CounterOverflow`]; storage errors.
    pub fn commit(
        &self,
        group: &GroupId,
        request: CommitRequest,
        rng: &mut impl CryptoRng,
    ) -> Result<CommitOutcome<T>, DeliveryError> {
        let now = self.clock.now();
        let next_epoch = request
            .epoch
            .checked_next()
            .ok_or(DeliveryError::CounterOverflow)?;
        let subscriptions_added = request
            .subscriptions_added
            .into_iter()
            .map(|subscription| Self::stored(now, subscription))
            .collect::<Result<Vec<_>, _>>()?;
        // The class never changes, so reading it before the atomic append is race-free.
        let lifetime = self.store.head(group)?.lifetime;
        let write = self.store.append_commit(
            group,
            NewCommit {
                epoch: request.epoch,
                next_epoch,
                commit: request.commit,
                next_auth_key: request.next_auth_key,
                state: request.state,
                subscriptions_added,
                subscriptions_removed: request.subscriptions_removed,
            },
            Self::idle_expiry(now, lifetime)?,
        )?;
        match write {
            CommitWrite::Appended { sequence } => Ok(CommitOutcome::Accepted {
                sequence,
                wakes: self.wakes(group, request.wake, rng)?,
                targets: self.live.targets(group, None)?,
            }),
            CommitWrite::EpochTaken { head_epoch } => Ok(CommitOutcome::Conflict { head_epoch }),
        }
    }

    /// `POST /v1/groups/{id}/msgs`. A stored message is appended and, if `wake` is set, pushed
    /// to members off the live channel; an ephemeral one is only forwarded live and never
    /// pushed.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::FutureEpoch`];
    /// [`DeliveryError::CounterOverflow`]; storage errors.
    pub fn post(
        &self,
        group: &GroupId,
        message: AppMessage,
        rng: &mut impl CryptoRng,
    ) -> Result<Posted<T>, DeliveryError> {
        let now = self.clock.now();
        let delivery = message.delivery;
        let sequence = self.record(
            group,
            Incoming {
                epoch: message.epoch,
                body: message.body,
                delivery,
            },
            now,
        )?;
        let wakes = match (delivery, message.wake) {
            (DeliveryMode::Stored, Some(urgency)) => self.wakes(group, urgency, rng)?,
            (DeliveryMode::Stored, None) | (DeliveryMode::Ephemeral, _) => Vec::new(),
        };
        Ok(Posted {
            sequence,
            wakes,
            targets: self.live.targets(group, None)?,
        })
    }

    /// Stores a stored message or touches the group for an ephemeral one.
    fn record(
        &self,
        group: &GroupId,
        incoming: Incoming,
        now: UnixSeconds,
    ) -> Result<Option<SequenceNumber>, DeliveryError> {
        let Incoming {
            epoch,
            body,
            delivery,
        } = incoming;
        let head = self.store.head(group)?;
        if epoch > head.head_epoch {
            return Err(DeliveryError::FutureEpoch {
                epoch,
                head_epoch: head.head_epoch,
            });
        }
        let idle_expires_at = Self::idle_expiry(now, head.lifetime)?;
        match delivery {
            DeliveryMode::Stored => self
                .store
                .append(group, NewEntry { epoch, body }, idle_expires_at)
                .map(Some),
            DeliveryMode::Ephemeral => self.store.touch(group, idle_expires_at).map(|()| None),
        }
    }

    /// The wake rule (design D): every unexpired subscription not attached to a live channel,
    /// with the request's urgency; `Quiet` ones get a random delay.
    fn wakes(
        &self,
        group: &GroupId,
        urgency: Urgency,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<Wake>, DeliveryError> {
        let now = self.clock.now();
        let connected = self.live.connected(group)?;
        Ok(self
            .store
            .live_subscriptions(group, now)?
            .into_iter()
            .filter(|stored| !connected.contains(&stored.subscription.id))
            .map(|stored| Wake {
                subscription: stored.subscription.id,
                bind: push_bind_for_subscription(&stored.subscription.id),
                push: stored.subscription.push,
                urgency,
                delay: match urgency {
                    Urgency::Alert => Duration::ZERO,
                    Urgency::Quiet => quiet_delay(rng),
                },
            })
            .collect())
    }

    /// `GET /v1/groups/{id}/msgs?after=seq`: up to [`crate::PAGE_ENTRIES`] entries after `after`.
    /// A read by a member counts as activity.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn messages(
        &self,
        group: &GroupId,
        after: SequenceNumber,
    ) -> Result<GroupMessages, DeliveryError> {
        let now = self.clock.now();
        let messages = self.store.entries_after(group, after)?;
        let head = self.store.head(group)?;
        self.store
            .touch(group, Self::idle_expiry(now, head.lifetime)?)?;
        Ok(GroupMessages {
            messages,
            head_epoch: head.head_epoch,
        })
    }

    /// `GET /v1/groups/{id}/state`: the latest snapshot. A read counts as activity.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn state(&self, group: &GroupId) -> Result<Option<GroupState>, DeliveryError> {
        let now = self.clock.now();
        let state = self.store.state(group)?;
        let lifetime = self.store.head(group)?.lifetime;
        self.store.touch(group, Self::idle_expiry(now, lifetime)?)?;
        Ok(state)
    }

    /// The key that `Grp` requests to this group must be signed with now.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn auth_key(&self, group: &GroupId) -> Result<GroupAuthKey, DeliveryError> {
        Ok(self.store.head(group)?.auth_key)
    }

    /// `PUT /v1/groups/{id}/subs/{sub_id}`: a member renews or replaces its own subscription.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::SubscriptionTooLong`]; storage errors.
    pub fn put_subscription(
        &self,
        group: &GroupId,
        subscription: Subscription,
    ) -> Result<(), DeliveryError> {
        let now = self.clock.now();
        let stored = Self::stored(now, subscription)?;
        let lifetime = self.store.head(group)?.lifetime;
        self.store
            .put_subscription(group, stored, Self::idle_expiry(now, lifetime)?)
    }

    /// `DELETE /v1/groups/{id}/subs/{sub_id}`, and what the server does when the push relay
    /// answers `410 Gone` for a [`Wake`]. Idempotent.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn remove_subscription(
        &self,
        group: &GroupId,
        subscription: &SubscriptionId,
    ) -> Result<(), DeliveryError> {
        self.store.remove_subscription(group, subscription)
    }

    /// `DELETE /v1/groups/{id}`: deletes the log, snapshot and subscriptions, and closes the
    /// group's live channels.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn delete_group(&self, group: &GroupId) -> Result<(), DeliveryError> {
        self.store.delete_group(group)?;
        self.live.close_group(group)
    }

    /// Deletes groups idle for their class's [`GroupLifetime::idle_lifetime`] and expired
    /// subscriptions, and closes the deleted groups' live channels. Run it periodically.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn expire_idle(&self) -> Result<Vec<GroupId>, DeliveryError> {
        let expired = self.store.expire(self.clock.now())?;
        for group in &expired {
            self.live.close_group(group)?;
        }
        Ok(expired)
    }

    /// `POST /v1/groups/{id}/live?sub=<sub_id>`: attaches a live channel. While attached,
    /// `subscription` is not pushed.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn open_live(
        &self,
        group: &GroupId,
        subscription: Option<SubscriptionId>,
        sender: T,
    ) -> Result<ChannelId, DeliveryError> {
        let now = self.clock.now();
        let lifetime = self.store.head(group)?.lifetime;
        self.store.touch(group, Self::idle_expiry(now, lifetime)?)?;
        self.live.attach(*group, subscription, sender)
    }

    /// Detaches a live channel when its exchange ends.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownChannel`] if it is not attached (already closed by a delete or
    /// expiry).
    pub fn close_live(&self, group: &GroupId, channel: ChannelId) -> Result<(), DeliveryError> {
        self.live.detach(group, channel)
    }

    /// How many live channels the group has (the server sees this anyway; design B.4.9).
    ///
    /// # Errors
    ///
    /// [`DeliveryError::Poisoned`].
    pub fn live_channels(&self, group: &GroupId) -> Result<usize, DeliveryError> {
        self.live.count(group)
    }

    /// A frame from `from`: stored if it is, and forwarded to the group's other channels.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownChannel`] if `from` is not attached to the group;
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::FutureEpoch`]; storage errors.
    pub fn live_frame(
        &self,
        group: &GroupId,
        from: ChannelId,
        frame: LiveFrame,
    ) -> Result<Forwarded<T>, DeliveryError> {
        if !self.live.contains(group, from)? {
            return Err(DeliveryError::UnknownChannel);
        }
        let now = self.clock.now();
        let sequence = self.record(
            group,
            Incoming {
                epoch: frame.epoch,
                body: frame.body,
                delivery: frame.delivery,
            },
            now,
        )?;
        Ok(Forwarded {
            sequence,
            targets: self.live.targets(group, Some(from))?,
        })
    }
}

/// A message arriving by `POST` or on a live channel.
struct Incoming {
    epoch: Epoch,
    body: MlsMessageBytes,
    delivery: DeliveryMode,
}

/// A uniformly random whole number of seconds in `0..=30`.
fn quiet_delay(rng: &mut impl CryptoRng) -> Duration {
    let choices = MAX_QUIET_DELAY.as_secs().saturating_add(1);
    // The modulo bias over 2^64 draws is below 2^-59: irrelevant for blurring timing.
    Duration::from_secs(rng.next_u64().checked_rem(choices).unwrap_or(0))
}
