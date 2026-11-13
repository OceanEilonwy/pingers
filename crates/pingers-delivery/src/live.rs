// SPDX-License-Identifier: AGPL-3.0-only
//! Which live channels are attached to which group (design B.4.9, "Meetup live channel").
//!
//! In memory only. A channel is an anonymous chunked OHTTP exchange; the registry holds the
//! caller's handle for it (for example the sending half of a queue), the subscription the
//! member named in `?sub=`, and nothing else: no IP address, no account.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use pingers_proto::{GroupId, SubscriptionId};

use crate::DeliveryError;

/// One open live channel, unique for the life of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(u64);

impl ChannelId {
    /// The number, for metrics and logs that need to tell channels apart.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
struct Channel<T> {
    subscription: Option<SubscriptionId>,
    sender: T,
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

    pub(crate) fn attach(
        &self,
        group: GroupId,
        subscription: Option<SubscriptionId>,
        sender: T,
    ) -> Result<ChannelId, DeliveryError> {
        let mut inner = self.lock()?;
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
            },
        );
        Ok(id)
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

    /// Whether `channel` is attached to `group`.
    pub(crate) fn contains(
        &self,
        group: &GroupId,
        channel: ChannelId,
    ) -> Result<bool, DeliveryError> {
        let inner = self.lock()?;
        Ok(inner
            .groups
            .get(group)
            .is_some_and(|channels| channels.contains_key(&channel)))
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

    pub(crate) fn close_group(&self, group: &GroupId) -> Result<(), DeliveryError> {
        self.lock()?.groups.remove(group);
        Ok(())
    }
}
