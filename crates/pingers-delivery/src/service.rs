// SPDX-License-Identifier: AGPL-3.0-only
//! The delivery service: the rules on top of a [`DeliveryStore`] and the live registry.

use core::time::Duration;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use pingers_crypto::{PushBind, push_bind_for_subscription};
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, CreateGroup, Delivery as DeliveryMode, Epoch,
    GroupAuthKey, GroupId, GroupMessages, GroupState, LiveFrame, MlsMessageBytes, SealedPush,
    SequenceNumber, Subscription, SubscriptionId, Urgency,
};
use pingers_time::{Clock, UnixSeconds};
use rand_core::CryptoRng;

use crate::live::{Admitted, ChannelId, FrameCheck, LiveRegistry, NewChannel, StoredAllowance};
use crate::store::{
    CommitWrite, DeliveryStore, GroupHead, NewCommit, NewEntry, NewGroup, PastAuthKey,
    StoredSubscription,
};
use crate::{DeliveryError, GroupLifetime, GroupStart};

/// The longest a subscription may live: Party subscriptions are renewed every 30 days
/// (design B.4.9).
pub const MAX_SUBSCRIPTION_LIFETIME: Duration = Duration::from_hours(30 * 24);

/// The most subscriptions a group holds. Each wake fans out to every one, so this bounds what
/// one commit or message can make the server send to the push relay. Far above a Party's
/// members.
pub const MAX_SUBSCRIPTIONS_PER_GROUP: usize = 1024;

/// The most live channels a group has open at once (a Meetup's members, with room for
/// reconnects that overlap the channel they replace).
pub const MAX_LIVE_CHANNELS_PER_GROUP: usize = 256;

/// The most stored frames (chat) one live channel may add to the log per 15-minute bucket.
/// The live channel needs no Anon credential, so this is what bounds the log growth one member
/// can cause through it; ephemeral frames are not stored and not counted.
pub const STORED_LIVE_FRAMES_PER_BUCKET: u32 = 240;

/// `Quiet` wakes are each delayed by a random 0 to 30 seconds to blur Party fan-out at the push
/// relay (design B.4.9); alerts are not delayed.
pub const MAX_QUIET_DELAY: Duration = Duration::from_secs(30);

/// The most wakes one group sends in a minute (posts and commits together). Each wake pushes
/// every subscription off the live channel, so this bounds the pushes one member can cause
/// the others; a message past it is still stored and read on the next wake, which a busy
/// conversation does not wait long for.
pub const WAKES_PER_GROUP_PER_MINUTE: u32 = 60;

/// Groups whose wake count is kept before stale minutes are dropped.
const TRACKED_WAKE_WINDOWS: usize = 65_536;

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

/// Who reads a group's log, by the log-auth key that signed the request (design B.2: the
/// server stores each epoch's key and requires a signature by it).
///
/// The rule: a key of epoch `e` reads the log up to and including the position of the commit
/// that ended `e`, never past it, whatever position it asks to read after. A current member
/// that missed commits reads with its old key, applies the commit that ended its epoch, and
/// reads on from there with the next epoch's key, one commit at a time; it misses nothing,
/// because each next key reads everything before its own boundary. A removed member holds the
/// key of the epoch it was removed in, so it reads up to the commit that removed it (and so
/// learns it was removed), and nothing written after, not even a late message a lagging
/// sender wrote in that old epoch: it cannot derive the next key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    /// Signed with the head epoch's key: reads everything.
    Current,
    /// Signed with the key of this earlier epoch.
    Behind(Epoch),
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
    /// Each group's current minute of wakes: when it started and how many it has sent.
    wake_windows: Mutex<HashMap<GroupId, (UnixSeconds, u32)>>,
}

impl<S: DeliveryStore, T: Clone> Delivery<S, T> {
    /// A service over `store`, reading time from `clock`.
    pub fn new(store: S, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            clock,
            live: LiveRegistry::new(),
            wake_windows: Mutex::new(HashMap::new()),
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

    /// Refuses subscriptions that would replace an unexpired one bound to another owner key:
    /// a subscription keeps the owner it was first stored with (design D).
    fn check_owners(
        &self,
        group: &GroupId,
        added: &[StoredSubscription],
    ) -> Result<(), DeliveryError> {
        let now = self.clock.now();
        let held = self.store.live_subscriptions(group, now)?;
        let taken = added.iter().any(|new| {
            held.iter().any(|old| {
                old.subscription.id == new.subscription.id
                    && old.subscription.owner != new.subscription.owner
            })
        });
        if taken {
            return Err(DeliveryError::SubscriptionOwned);
        }
        Ok(())
    }

    /// Refuses a change that would leave `group` with more than
    /// [`MAX_SUBSCRIPTIONS_PER_GROUP`] unexpired subscriptions. Concurrent changes are
    /// checked one by one, so a burst can pass the limit by at most the burst's size.
    fn check_room(
        &self,
        group: &GroupId,
        added: &[StoredSubscription],
        removed: &[SubscriptionId],
    ) -> Result<(), DeliveryError> {
        let now = self.clock.now();
        let mut ids: HashSet<SubscriptionId> = self
            .store
            .live_subscriptions(group, now)?
            .into_iter()
            .map(|stored| stored.subscription.id)
            .collect();
        for id in removed {
            ids.remove(id);
        }
        ids.extend(added.iter().map(|stored| stored.subscription.id));
        if ids.len() > MAX_SUBSCRIPTIONS_PER_GROUP {
            return Err(DeliveryError::TooManySubscriptions);
        }
        Ok(())
    }

    /// `POST /v1/groups`: a new group at `start.epoch`, of lifetime class `start.lifetime`.
    /// Wakes nobody: the creator delivers Welcomes through mailboxes.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::GroupExists`]; [`DeliveryError::SubscriptionTooLong`];
    /// [`DeliveryError::TooManySubscriptions`]; storage errors.
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
        let distinct: HashSet<SubscriptionId> = subscriptions
            .iter()
            .map(|stored| stored.subscription.id)
            .collect();
        if distinct.len() > MAX_SUBSCRIPTIONS_PER_GROUP {
            return Err(DeliveryError::TooManySubscriptions);
        }
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
    /// ones are not; the live channels attached to a removed subscription are closed with it
    /// (design G.6: the removed member gets nothing further, and can send nothing more), and
    /// the commit is not forwarded to them.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::SubscriptionTooLong`];
    /// [`DeliveryError::TooManySubscriptions`]; [`DeliveryError::CounterOverflow`]; storage
    /// errors.
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
        let removed = request.subscriptions_removed;
        if !subscriptions_added.is_empty() {
            self.check_owners(group, &subscriptions_added)?;
            self.check_room(group, &subscriptions_added, &removed)?;
        }
        let write = self.store.append_commit(
            group,
            NewCommit {
                epoch: request.epoch,
                next_epoch,
                commit: request.commit,
                next_auth_key: request.next_auth_key,
                state: request.state,
                subscriptions_added,
                subscriptions_removed: removed.clone(),
            },
            Self::idle_expiry(now, lifetime)?,
        )?;
        match write {
            CommitWrite::Appended { sequence } => {
                self.live.close_subscriptions(group, &removed)?;
                // Members on a channel of the ended epoch get the commit there, not pushed.
                let wakes = self.wakes(
                    group,
                    WakeRule {
                        connected_in: request.epoch,
                        urgency: request.wake,
                    },
                    rng,
                )?;
                // Every channel attached in the epoch this commit ended gets it as its last
                // frame and is closed: its members attach again with the next key, which a
                // member removed without naming its subscription does not have.
                let targets = self.live.end_epoch(group, next_epoch)?;
                Ok(CommitOutcome::Accepted {
                    sequence,
                    wakes,
                    targets,
                })
            }
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
        let delivery = message.delivery;
        let head = self.store.head(group)?;
        let sequence = self.record(
            group,
            &head,
            Incoming {
                epoch: message.epoch,
                body: message.body,
                delivery,
            },
        )?;
        let wakes = match (delivery, message.wake) {
            (DeliveryMode::Stored, Some(urgency)) => self.wakes(
                group,
                WakeRule {
                    connected_in: head.head_epoch,
                    urgency,
                },
                rng,
            )?,
            (DeliveryMode::Stored, None) | (DeliveryMode::Ephemeral, _) => Vec::new(),
        };
        Ok(Posted {
            sequence,
            wakes,
            targets: self.live.targets(group, None, head.head_epoch)?,
        })
    }

    /// Stores a stored message or touches the group for an ephemeral one, `head` being the
    /// group's head as read before.
    fn record(
        &self,
        group: &GroupId,
        head: &GroupHead,
        incoming: Incoming,
    ) -> Result<Option<SequenceNumber>, DeliveryError> {
        let now = self.clock.now();
        let Incoming {
            epoch,
            body,
            delivery,
        } = incoming;
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

    /// The wake rule (design D): every unexpired subscription not attached to a live channel of
    /// epoch `rule.connected_in`, with the request's urgency; `Quiet` ones get a random delay.
    /// None once the group has sent [`WAKES_PER_GROUP_PER_MINUTE`] this minute.
    fn wakes(
        &self,
        group: &GroupId,
        rule: WakeRule,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<Wake>, DeliveryError> {
        let WakeRule {
            connected_in,
            urgency,
        } = rule;
        let now = self.clock.now();
        if !self.take_wake(group, now) {
            return Ok(Vec::new());
        }
        let connected = self.live.connected(group, connected_in)?;
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

    /// Counts one wake against `group`'s minute; `false` once [`WAKES_PER_GROUP_PER_MINUTE`]
    /// are used up.
    fn take_wake(&self, group: &GroupId, now: UnixSeconds) -> bool {
        let minute = Duration::from_mins(1);
        let mut windows = self
            .wake_windows
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if windows.len() >= TRACKED_WAKE_WINDOWS {
            windows.retain(|_, (start, _)| now.saturating_duration_since(*start) < minute);
        }
        let window = windows.entry(*group).or_insert((now, 0));
        if now.saturating_duration_since(window.0) >= minute {
            *window = (now, 0);
        }
        if window.1 >= WAKES_PER_GROUP_PER_MINUTE {
            return false;
        }
        window.1 = window.1.saturating_add(1);
        true
    }

    /// `GET /v1/groups/{id}/msgs?after=seq`: up to [`crate::PAGE_ENTRIES`] entries after `after`.
    /// A read by a current member counts as activity.
    ///
    /// A reader [`Reader::Behind`] gets only entries up to and including the commit that ended
    /// its key's epoch, and the head epoch as at most one past its own: it learns that its
    /// epoch has ended, not what followed or how much. Its read is not activity, so a removed
    /// member polling cannot keep a group alive.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn messages(
        &self,
        group: &GroupId,
        after: SequenceNumber,
        reader: Reader,
    ) -> Result<GroupMessages, DeliveryError> {
        let now = self.clock.now();
        let messages = self.store.entries_after(group, after)?;
        let head = self.store.head(group)?;
        match reader {
            Reader::Current => {
                self.store
                    .touch(group, Self::idle_expiry(now, head.lifetime)?)?;
                Ok(GroupMessages {
                    messages,
                    head_epoch: head.head_epoch,
                })
            }
            Reader::Behind(epoch) => {
                // A past key exists only because a commit ended its epoch; were that commit
                // ever missing, the reader gets nothing rather than everything.
                let boundary = self.store.commit_position(group, epoch)?;
                Ok(GroupMessages {
                    messages: messages
                        .into_iter()
                        .take_while(|entry| boundary.is_some_and(|last| entry.sequence <= last))
                        .collect(),
                    head_epoch: epoch.checked_next().unwrap_or(epoch).min(head.head_epoch),
                })
            }
        }
    }

    /// `GET /v1/groups/{id}/state`: the latest snapshot. A read by a current member counts as
    /// activity.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::LaterEpoch`] for a reader
    /// [`Reader::Behind`] the snapshot's epoch (it could not open it, and a removed member must
    /// not see what the group became); storage errors.
    pub fn state(
        &self,
        group: &GroupId,
        reader: Reader,
    ) -> Result<Option<GroupState>, DeliveryError> {
        let now = self.clock.now();
        let state = self.store.state(group)?;
        match reader {
            Reader::Current => {
                let lifetime = self.store.head(group)?.lifetime;
                self.store.touch(group, Self::idle_expiry(now, lifetime)?)?;
                Ok(state)
            }
            Reader::Behind(epoch) => match state {
                Some(found) if found.epoch > epoch => Err(DeliveryError::LaterEpoch),
                other @ (Some(_) | None) => Ok(other),
            },
        }
    }

    /// The group's unexpired subscription `id`, if it has one: its owner key must sign any
    /// request that changes it or speaks for it.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn subscription(
        &self,
        group: &GroupId,
        id: &SubscriptionId,
    ) -> Result<Option<Subscription>, DeliveryError> {
        Ok(self
            .store
            .live_subscriptions(group, self.clock.now())?
            .into_iter()
            .map(|stored| stored.subscription)
            .find(|subscription| subscription.id == *id))
    }

    /// The key that `Grp` requests to this group must be signed with now.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn auth_key(&self, group: &GroupId) -> Result<GroupAuthKey, DeliveryError> {
        Ok(self.store.head(group)?.auth_key)
    }

    /// The key that `Grp` requests to this group must be signed with now, with the epoch it
    /// belongs to, read together: what a live channel records when it attaches
    /// ([`Delivery::open_live`]).
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn current_auth(&self, group: &GroupId) -> Result<PastAuthKey, DeliveryError> {
        let head = self.store.head(group)?;
        Ok(PastAuthKey {
            epoch: head.head_epoch,
            key: head.auth_key,
        })
    }

    /// The keys of the group's earlier epochs with their epochs, newest first. A request
    /// signed with one is from a member that has not applied the later commits, or from one
    /// they removed: it reads as [`Reader::Behind`] that epoch, and its commit is answered
    /// with the conflict.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn past_auth_keys(&self, group: &GroupId) -> Result<Vec<PastAuthKey>, DeliveryError> {
        self.store.past_auth_keys(group)
    }

    /// The head epoch as `reader` may see it: the epoch the next commit must be made in, or
    /// for a reader [`Reader::Behind`], at most one past its own (as in
    /// [`Delivery::messages`]).
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; storage errors.
    pub fn head_epoch(&self, group: &GroupId, reader: Reader) -> Result<Epoch, DeliveryError> {
        let head = self.store.head(group)?.head_epoch;
        Ok(match reader {
            Reader::Current => head,
            Reader::Behind(epoch) => epoch.checked_next().unwrap_or(epoch).min(head),
        })
    }

    /// `PUT /v1/groups/{id}/subs/{sub_id}`: a member renews or replaces its own subscription.
    /// The caller has checked the owner's signature against [`Delivery::subscription`].
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::SubscriptionTooLong`];
    /// [`DeliveryError::SubscriptionOwned`] if it would change the owner key of an unexpired
    /// subscription; [`DeliveryError::TooManySubscriptions`]; storage errors.
    pub fn put_subscription(
        &self,
        group: &GroupId,
        subscription: Subscription,
    ) -> Result<(), DeliveryError> {
        let now = self.clock.now();
        let stored = Self::stored(now, subscription)?;
        let lifetime = self.store.head(group)?.lifetime;
        self.check_owners(group, core::slice::from_ref(&stored))?;
        self.check_room(group, core::slice::from_ref(&stored), &[])?;
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

    /// Whether `wake` may still be sent now: its group exists and still holds an unexpired
    /// subscription with the wake's id and the same sealed push. A wake can wait out its delay
    /// (and any push to the same subscription still in flight) after it was made, and the
    /// subscription may since have been deleted, expired, replaced or answered `410 Gone`; the
    /// server asks this right before sending, so none of those is pushed again.
    ///
    /// # Errors
    ///
    /// Storage errors. A deleted group is not an error: its wakes are simply no longer due.
    pub fn still_subscribed(&self, group: &GroupId, wake: &Wake) -> Result<bool, DeliveryError> {
        match self.store.live_subscriptions(group, self.clock.now()) {
            Ok(live) => Ok(live.iter().any(|stored| {
                stored.subscription.id == wake.subscription && stored.subscription.push == wake.push
            })),
            Err(DeliveryError::UnknownGroup) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// `DELETE /v1/groups/{id}`: deletes a Meetup group's log, snapshot and subscriptions, and
    /// closes its live channels. A Party group cannot be deleted: every member holds its log-auth
    /// key, so a delete would let any one member erase the Party for all; it expires after
    /// [`GroupLifetime::Party`]'s idle time instead.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::PartyGroupKept`]; storage errors.
    pub fn delete_group(&self, group: &GroupId) -> Result<(), DeliveryError> {
        if self.store.head(group)?.lifetime == GroupLifetime::Party {
            return Err(DeliveryError::PartyGroupKept);
        }
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

    /// `POST /v1/groups/{id}/live?sub=<sub_id>`: attaches a live channel whose request was
    /// signed with the log-auth key of `attach.epoch` ([`Delivery::current_auth`]). While
    /// attached, `attach.subscription` is not pushed. The channel lasts until the commit that
    /// ends its epoch, which is its last frame.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::EpochEnded`] if a commit has ended
    /// the epoch since the signature was checked; [`DeliveryError::TooManyChannels`] if the group
    /// has [`MAX_LIVE_CHANNELS_PER_GROUP`] open; storage errors.
    pub fn open_live(
        &self,
        group: &GroupId,
        attach: LiveAttach,
        sender: T,
    ) -> Result<ChannelId, DeliveryError> {
        let LiveAttach {
            epoch,
            subscription,
        } = attach;
        let now = self.clock.now();
        let head = self.store.head(group)?;
        if epoch < head.head_epoch {
            return Err(DeliveryError::EpochEnded);
        }
        self.store
            .touch(group, Self::idle_expiry(now, head.lifetime)?)?;
        self.live.attach(
            *group,
            NewChannel {
                epoch,
                subscription,
                sender,
            },
        )
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
    /// [`DeliveryError::UnknownChannel`] if `from` is not attached to the group, or attached in
    /// an epoch a commit has ended (it is then detached: the frame is neither stored nor
    /// forwarded, and does not count as activity);
    /// [`DeliveryError::LiveRateLimited`] for a stored frame over the channel's
    /// [`STORED_LIVE_FRAMES_PER_BUCKET`] (it is neither stored nor forwarded);
    /// [`DeliveryError::UnknownGroup`]; [`DeliveryError::FutureEpoch`]; storage errors.
    pub fn live_frame(
        &self,
        group: &GroupId,
        from: ChannelId,
        frame: LiveFrame,
    ) -> Result<Forwarded<T>, DeliveryError> {
        let now = self.clock.now();
        let allowance = match frame.delivery {
            DeliveryMode::Stored => Some(StoredAllowance {
                bucket: now.bucket().index(),
                limit: STORED_LIVE_FRAMES_PER_BUCKET,
            }),
            DeliveryMode::Ephemeral => None,
        };
        let head = self.store.head(group)?;
        match self.live.admit_frame(
            group,
            from,
            FrameCheck {
                head: head.head_epoch,
                allowance,
            },
        )? {
            Admitted::Yes => {}
            Admitted::UnknownChannel | Admitted::EpochEnded => {
                return Err(DeliveryError::UnknownChannel);
            }
            Admitted::OverAllowance => return Err(DeliveryError::LiveRateLimited),
        }
        let sequence = self.record(
            group,
            &head,
            Incoming {
                epoch: frame.epoch,
                body: frame.body,
                delivery: frame.delivery,
            },
        )?;
        Ok(Forwarded {
            sequence,
            targets: self.live.targets(group, Some(from), head.head_epoch)?,
        })
    }
}

/// What a live channel attaches under ([`Delivery::open_live`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveAttach {
    /// The epoch whose log-auth key signed the request: the channel lasts until the commit
    /// that ends it.
    pub epoch: Epoch,
    /// The subscription named in `?sub=`, not pushed while the channel is attached.
    pub subscription: Option<SubscriptionId>,
}

/// Whom a commit or message wakes: subscriptions not on a live channel attached in
/// `connected_in` or later, with `urgency`.
#[derive(Clone, Copy)]
struct WakeRule {
    connected_in: Epoch,
    urgency: Urgency,
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
