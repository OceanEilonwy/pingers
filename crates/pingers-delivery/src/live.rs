// SPDX-License-Identifier: AGPL-3.0-only
//! Which live channels are attached to which group (design B.4.9, "Meetup live channel").
//!
//! In memory only. A channel is an anonymous chunked OHTTP exchange; the registry holds the
//! caller's handle for it (for example the sending half of a queue), the subscription the
//! member named in `?sub=`, the epoch whose log-auth key let it attach, and nothing else: no IP
//! address, no account.
//!
//! A channel lives for one epoch. The commit that ends its epoch is the last frame it gets,
//! and it is closed after it (the member attaches again with the next epoch's key, which a
//! removed member does not have). A channel found behind the head epoch is refused and dropped
//! wherever it is looked up, so one that attached while a commit was landing cannot outlive it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use pingers_proto::{Epoch, GroupId, SubscriptionId};

use crate::{DeliveryError, MAX_LIVE_CHANNELS_PER_GROUP};

/// One open live channel, unique for the life of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(u64);

#[derive(Debug)]
struct Channel<T> {
    subscription: Option<SubscriptionId>,
    /// The epoch whose log-auth key authorised the attach.
    epoch: Epoch,
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

/// A channel to attach.
#[derive(Debug)]
pub(crate) struct NewChannel<T> {
    /// The epoch whose log-auth key signed the attach.
    pub(crate) epoch: Epoch,
    /// The subscription named in `?sub=`, not to be pushed while attached.
    pub(crate) subscription: Option<SubscriptionId>,
    /// The caller's handle.
    pub(crate) sender: T,
}

/// What a frame from a channel is checked against.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameCheck {
    /// The group's head epoch: a channel attached before it is dropped.
    pub(crate) head: Epoch,
    /// The allowance a stored frame is charged to; `None` for an ephemeral one.
    pub(crate) allowance: Option<StoredAllowance>,
}

/// Whether a frame from a channel may go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// Yes (and charged, if it is stored).
    Yes,
    /// The channel is not attached to the group.
    UnknownChannel,
    /// The channel attached in an epoch a commit has ended; it has now been dropped.
    EpochEnded,
    /// The channel used its allowance this bucket.
    OverAllowance,
}

#[derive(Debug)]
struct Inner<T> {
    next: u64,
    groups: HashMap<GroupId, BTreeMap<ChannelId, Channel<T>>>,
}

impl<T> Inner<T> {
    /// Forgets the group's entry once it has no channels.
    fn tidy(&mut self, group: &GroupId) {
        if self.groups.get(group).is_some_and(BTreeMap::is_empty) {
            self.groups.remove(group);
        }
    }
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
        channel: NewChannel<T>,
    ) -> Result<ChannelId, DeliveryError> {
        let NewChannel {
            epoch,
            subscription,
            sender,
        } = channel;
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
                epoch,
                sender,
                stored: StoredCount::default(),
            },
        );
        Ok(id)
    }

    /// Whether a frame from `channel` may go on: the channel must be attached in the head epoch
    /// `head` (one behind it is dropped), and a stored frame (`allowance` given) must fit in
    /// the channel's allowance for the bucket, which it is then charged to.
    pub(crate) fn admit_frame(
        &self,
        group: &GroupId,
        channel: ChannelId,
        check: FrameCheck,
    ) -> Result<Admitted, DeliveryError> {
        let FrameCheck { head, allowance } = check;
        let mut inner = self.lock()?;
        let Some(channels) = inner.groups.get_mut(group) else {
            return Ok(Admitted::UnknownChannel);
        };
        let Some(found) = channels.get_mut(&channel) else {
            return Ok(Admitted::UnknownChannel);
        };
        if found.epoch < head {
            channels.remove(&channel);
            inner.tidy(group);
            return Ok(Admitted::EpochEnded);
        }
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
        inner.tidy(group);
        Ok(())
    }

    /// The handles of every channel of the group attached in epoch `head` or later, except
    /// `except`, in channel order. Channels behind `head` are dropped: their epoch has ended.
    pub(crate) fn targets(
        &self,
        group: &GroupId,
        except: Option<ChannelId>,
        head: Epoch,
    ) -> Result<Vec<T>, DeliveryError> {
        let mut inner = self.lock()?;
        let Some(channels) = inner.groups.get_mut(group) else {
            return Ok(Vec::new());
        };
        channels.retain(|_, channel| channel.epoch >= head);
        let targets = channels
            .iter()
            .filter(|(id, _)| Some(**id) != except)
            .map(|(_, channel)| channel.sender.clone())
            .collect();
        inner.tidy(group);
        Ok(targets)
    }

    /// For an accepted commit that started epoch `next`: the handles of all the group's
    /// channels, in channel order, after detaching every one attached before `next`. The commit
    /// is forwarded to each; for the detached ones it is the last frame (dropping the returned
    /// handle afterwards ends the exchange).
    pub(crate) fn end_epoch(&self, group: &GroupId, next: Epoch) -> Result<Vec<T>, DeliveryError> {
        let mut inner = self.lock()?;
        let Some(channels) = inner.groups.get_mut(group) else {
            return Ok(Vec::new());
        };
        let targets = channels
            .values()
            .map(|channel| channel.sender.clone())
            .collect();
        channels.retain(|_, channel| channel.epoch >= next);
        inner.tidy(group);
        Ok(targets)
    }

    /// The subscriptions whose owners are on a live channel attached in epoch `head` or later,
    /// so not to be pushed. A channel behind `head` suppresses no push.
    pub(crate) fn connected(
        &self,
        group: &GroupId,
        head: Epoch,
    ) -> Result<HashSet<SubscriptionId>, DeliveryError> {
        let inner = self.lock()?;
        Ok(inner
            .groups
            .get(group)
            .into_iter()
            .flatten()
            .filter(|(_, channel)| channel.epoch >= head)
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
            inner.tidy(group);
        }
        Ok(())
    }

    pub(crate) fn close_group(&self, group: &GroupId) -> Result<(), DeliveryError> {
        self.lock()?.groups.remove(group);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    /// A channel that attached under an epoch's key while the commit ending that epoch was
    /// landing (so after the commit closed the epoch's channels) gets nothing, sends nothing
    /// and suppresses no push.
    #[test]
    fn a_channel_attached_behind_the_head_is_dropped() -> Result<(), DeliveryError> {
        let registry = LiveRegistry::new();
        let group = GroupId::from_bytes([1; 32]);
        let subscription = SubscriptionId::from_bytes([2; 16]);
        let late = registry.attach(
            group,
            NewChannel {
                epoch: Epoch::new(0),
                subscription: Some(subscription),
                sender: 1_u8,
            },
        )?;
        let current = registry.attach(
            group,
            NewChannel {
                epoch: Epoch::new(1),
                subscription: None,
                sender: 2_u8,
            },
        )?;
        assert!(registry.connected(&group, Epoch::new(1))?.is_empty());
        assert_eq!(
            registry.admit_frame(
                &group,
                late,
                FrameCheck {
                    head: Epoch::new(1),
                    allowance: None
                }
            )?,
            Admitted::EpochEnded
        );
        assert_eq!(
            registry.admit_frame(
                &group,
                late,
                FrameCheck {
                    head: Epoch::new(1),
                    allowance: None
                }
            )?,
            Admitted::UnknownChannel
        );
        let _again = registry.attach(
            group,
            NewChannel {
                epoch: Epoch::new(0),
                subscription: None,
                sender: 3_u8,
            },
        )?;
        assert_eq!(registry.targets(&group, None, Epoch::new(1))?, vec![2]);
        assert_eq!(registry.count(&group)?, 1);
        assert_eq!(
            registry.admit_frame(
                &group,
                current,
                FrameCheck {
                    head: Epoch::new(1),
                    allowance: None
                }
            )?,
            Admitted::Yes
        );
        // The commit that ends epoch 1 reaches the channel once more and closes it.
        assert_eq!(registry.end_epoch(&group, Epoch::new(2))?, vec![2]);
        assert_eq!(registry.count(&group)?, 0);
        Ok(())
    }
}
