// SPDX-License-Identifier: AGPL-3.0-only
//! Which live channels are attached to which group (design B.4.9, "Meetup live channel").
//!
//! In memory only. A channel is an anonymous chunked OHTTP exchange; the registry holds the
//! caller's handle for it (for example the sending half of a queue), the subscription the
//! member named in `?sub=`, and nothing else: no IP address, no account.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use pingers_proto::{GroupId, SubscriptionId};

use crate::{DeliveryError, MAX_LIVE_CHANNELS_PER_GROUP};

/// One open live channel, unique for the life of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(u64);

#[derive(Debug)]
struct Channel<T> {
    subscription: Option<SubscriptionId>,
    sender: T,
    /// Stored frames counted in the bucket with this index.
    stored: StoredCount,
}

/// How many stored frames a channel sent in one bucket.
#[derive(Clone, Copy, Debug, Default)]
struct StoredCount {
    bucket: u64,
    frames: u32,
}

/// The stored-frame allowance a frame is charged to: `limit` per bucket.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StoredAllowance {
    /// The current bucket's index.
    pub(crate) bucket: u64,
    /// Frames allowed per bucket.
    pub(crate) limit: u32,
}

/// Whether a frame from a channel may go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// Yes (and charged, if it is stored).
    Yes,
    /// The channel is not attached to the group.
    UnknownChannel,
    /// The channel used its allowance this bucket.
    OverAllowance,
}

#[derive(Debug)]
struct Inner<T> {
    next: u64,
    groups: HashMap<GroupId, BTreeMap<ChannelId, Channel<T>>>,
}

/// The fan-out registry. Dropping a channel's entry drops its handle, which is how a group's
/// deletion closes its channels.
#[derive(Debug)]
pub(crate) struct LiveRegistry<T> {
    inner: Mutex<Inner<T>>,
}

impl<T: Clone> LiveRegistry<T> {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next: 0,
                groups: HashMap::new(),
            }),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, Inner<T>>, DeliveryError> {
        self.inner.lock().map_err(|_| DeliveryError::Poisoned)
    }

    /// Attaches a channel unless the group already has [`MAX_LIVE_CHANNELS_PER_GROUP`].
    pub(crate) fn attach(
        &self,
        group: GroupId,
        subscription: Option<SubscriptionId>,
        sender: T,
    ) -> Result<ChannelId, DeliveryError> {
        let mut inner = self.lock()?;
        if inner.groups.get(&group).map_or(0, BTreeMap::len) >= MAX_LIVE_CHANNELS_PER_GROUP {
            return Err(DeliveryError::TooManyChannels);
        }
        let id = ChannelId(inner.next);
        inner.next = inner
            .next
            .checked_add(1)
            .ok_or(DeliveryError::CounterOverflow)?;
        inner.groups.entry(group).or_default().insert(
            id,
            Channel {
                subscription,
                sender,
                stored: StoredCount::default(),
            },
        );
        Ok(id)
    }

    /// Whether a frame from `channel` may go on: the channel must be attached, and a stored
    /// frame (`allowance` given) must fit in the channel's allowance for the bucket, which it
    /// is then charged to.
    pub(crate) fn admit_frame(
        &self,
        group: &GroupId,
        channel: ChannelId,
        allowance: Option<StoredAllowance>,
    ) -> Result<Admitted, DeliveryError> {
        let mut inner = self.lock()?;
        let Some(found) = inner
            .groups
            .get_mut(group)
            .and_then(|channels| channels.get_mut(&channel))
        else {
            return Ok(Admitted::UnknownChannel);
        };
        let Some(allowance) = allowance else {
            return Ok(Admitted::Yes);
        };
        if found.stored.bucket != allowance.bucket {
            found.stored = StoredCount {
                bucket: allowance.bucket,
                frames: 0,
            };
        }
        if found.stored.frames >= allowance.limit {
            return Ok(Admitted::OverAllowance);
        }
        found.stored.frames = found.stored.frames.saturating_add(1);
        Ok(Admitted::Yes)
    }

    pub(crate) fn detach(&self, group: &GroupId, channel: ChannelId) -> Result<(), DeliveryError> {
        let mut inner = self.lock()?;
        let channels = inner
            .groups
            .get_mut(group)
            .ok_or(DeliveryError::UnknownChannel)?;
        channels
            .remove(&channel)
            .ok_or(DeliveryError::UnknownChannel)?;
        if channels.is_empty() {
            inner.groups.remove(group);
        }
        Ok(())
    }

    /// The handles of every channel of the group except `except`, in channel order.
    pub(crate) fn targets(
        &self,
        group: &GroupId,
        except: Option<ChannelId>,
    ) -> Result<Vec<T>, DeliveryError> {
        let inner = self.lock()?;
        Ok(inner
            .groups
            .get(group)
            .into_iter()
            .flatten()
            .filter(|(id, _)| Some(**id) != except)
            .map(|(_, channel)| channel.sender.clone())
            .collect())
    }

    /// The subscriptions whose owners are on a live channel, so not to be pushed.
    pub(crate) fn connected(
        &self,
        group: &GroupId,
    ) -> Result<HashSet<SubscriptionId>, DeliveryError> {
        let inner = self.lock()?;
        Ok(inner
            .groups
            .get(group)
            .into_iter()
            .flatten()
            .filter_map(|(_, channel)| channel.subscription)
            .collect())
    }

    pub(crate) fn count(&self, group: &GroupId) -> Result<usize, DeliveryError> {
        let inner = self.lock()?;
        Ok(inner.groups.get(group).map_or(0, BTreeMap::len))
    }

    /// Closes the group's channels attached to any of `subscriptions`.
    pub(crate) fn close_subscriptions(
        &self,
        group: &GroupId,
        subscriptions: &[SubscriptionId],
    ) -> Result<(), DeliveryError> {
        if subscriptions.is_empty() {
            return Ok(());
        }
        let mut inner = self.lock()?;
        if let Some(channels) = inner.groups.get_mut(group) {
            channels.retain(|_, channel| {
                channel
                    .subscription
                    .is_none_or(|subscription| !subscriptions.contains(&subscription))
            });
            if channels.is_empty() {
                inner.groups.remove(group);
            }
        }
        Ok(())
    }

    pub(crate) fn close_group(&self, group: &GroupId) -> Result<(), DeliveryError> {
        self.lock()?.groups.remove(group);
        Ok(())
    }
}
