// SPDX-License-Identifier: AGPL-3.0-only
//! The mailbox pool and the pushes that wake this device (design B.4.4, B.4.9, E.2): one-time
//! mailboxes are registered on earlier wakes, at most one per wake, so that registering one
//! never coincides with using it; a knock names one at once, and carries a sealed Meetup
//! subscription so the poster can add the knocker to the Meetup with its push.

use pingers_proto::{MailboxId, Subscription, TimeToLive};
use pingers_store::MailboxPurpose;
use pingers_time::{Bucket, BucketCount, UnixSeconds};

use crate::mailbox::Delivered;
use crate::{NewMailbox, NotSentReason, PushSeal, SyncEngine, SyncError, WakeBudget};

/// Buckets a lease is live for (design F.1, `L = 4`).
pub const LEASE_BUCKETS: BucketCount = BucketCount::new(4);

/// The sealed pushes this device registers, set by the client from the platform's push
/// tokens. Without it ("No wake-up pushes") mailboxes and subscriptions carry no push.
#[derive(Clone, Debug)]
pub struct WakePush {
    /// For knock mailboxes: the location-push class where the platform has one (design
    /// B.4.9: Location, or Standard/Alert).
    pub knock: PushSeal,
    /// For reply and invite mailboxes and Meetup subscriptions: Standard/Alert.
    pub alert: PushSeal,
}

/// How many mailboxes of one purpose to keep pooled, and how long each lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolTarget {
    /// The purpose.
    pub purpose: MailboxPurpose,
    /// Pooled, unexpired mailboxes to keep.
    pub size: u32,
    /// The lifetime of each new one.
    pub lifetime: TimeToLive,
}

/// What a pool refill did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum PoolRefill {
    /// The pool was already full.
    Full {
        /// Pooled mailboxes.
        pooled: u32,
    },
    /// One mailbox was registered.
    Registered {
        /// Pooled mailboxes now.
        pooled: u32,
    },
    /// The registration was not sent; try on a later wake.
    NotSent(NotSentReason),
}

/// When a lease starting in `start` stops being live.
fn lease_end(start: Bucket) -> Result<UnixSeconds, SyncError> {
    start
        .checked_add(LEASE_BUCKETS)
        .map(Bucket::start)
        .ok_or(SyncError::OutOfRange("lease end"))
}

impl SyncEngine {
    /// Sets (or, with `None`, clears) the pushes new mailboxes and subscriptions are sealed
    /// with.
    pub fn set_wake_push(&self, push: Option<WakePush>) {
        *self
            .wake_push
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = push;
    }

    fn seal_for(&self, purpose: MailboxPurpose) -> Option<PushSeal> {
        let push = self
            .wake_push
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        push.as_ref().map(|push| match purpose {
            MailboxPurpose::Knock => push.knock.clone(),
            MailboxPurpose::Reply | MailboxPurpose::Invite => push.alert.clone(),
        })
    }

    /// Registers at most one mailbox for `target`'s pool if it holds fewer than
    /// `target.size` (design E.2: at most one registration per wake), sealed with the wake
    /// push for its purpose.
    ///
    /// # Errors
    ///
    /// As [`SyncEngine::register_mailbox`].
    pub async fn refill_mailbox_pool(
        &self,
        target: &PoolTarget,
        budget: WakeBudget,
    ) -> Result<PoolRefill, SyncError> {
        let pooled = self.pooled_mailboxes(target.purpose).await?;
        if pooled >= target.size {
            return Ok(PoolRefill::Full { pooled });
        }
        let mailbox = NewMailbox {
            purpose: target.purpose,
            lifetime: target.lifetime,
            push: self.seal_for(target.purpose),
        };
        match self.register_mailbox(&mailbox, budget).await? {
            Delivered::Done(_) => Ok(PoolRefill::Registered {
                pooled: pooled.saturating_add(1),
            }),
            Delivered::Queued(reason) | Delivered::Dropped(reason) => {
                Ok(PoolRefill::NotSent(reason))
            }
        }
    }

    /// Takes a pooled mailbox of `purpose` that lives at least until a lease starting in
    /// `lease_start` ends, marking it in use. Synchronous (one SQLite statement), as
    /// pingers-wake's `ServerPort::take_mailbox` is.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`]; [`SyncError::OutOfRange`].
    pub fn take_mailbox_for_lease(
        &self,
        purpose: MailboxPurpose,
        lease_start: Bucket,
    ) -> Result<Option<MailboxId>, SyncError> {
        self.take_mailbox_until(purpose, lease_end(lease_start)?)
    }

    /// Takes a pooled mailbox of `purpose` that lives at least until `until` (an invite's
    /// expiry, a join's reply), marking it in use. Synchronous, like
    /// [`SyncEngine::take_mailbox_for_lease`].
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub fn take_mailbox_until(
        &self,
        purpose: MailboxPurpose,
        until: UnixSeconds,
    ) -> Result<Option<MailboxId>, SyncError> {
        let session = self.database.session()?;
        Ok(session
            .server(&self.server())?
            .mailboxes()
            .take_pooled(purpose, until)?
            .map(|mailbox| mailbox.id()))
    }

    /// A sealed subscription for the Meetup a knock may lead to, sealed with the alert push;
    /// `None` when wake-up pushes are off. The knocker keeps it and records it with
    /// [`SyncEngine::remember_subscription`] once it is in the Meetup.
    ///
    /// # Errors
    ///
    /// [`SyncError::Crypto`] if sealing fails.
    pub fn knock_subscription(
        &self,
        lifetime: TimeToLive,
    ) -> Result<Option<Subscription>, SyncError> {
        self.seal_for(MailboxPurpose::Reply)
            .map(|seal| self.new_subscription(&seal, lifetime))
            .transpose()
    }

    /// A sealed subscription for a Party this device is joining (the join request's
    /// `party_subscription`), sealed with the alert push; `None` when wake-up pushes are off.
    /// The joiner records it with [`SyncEngine::remember_subscription`] once it is in the
    /// group.
    ///
    /// # Errors
    ///
    /// [`SyncError::Crypto`] if sealing fails.
    pub fn party_subscription(
        &self,
        lifetime: TimeToLive,
    ) -> Result<Option<Subscription>, SyncError> {
        self.knock_subscription(lifetime)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn a_lease_ends_four_buckets_after_it_starts() -> Result<(), SyncError> {
        let start = Bucket::containing(UnixSeconds::new(9_000));
        assert_eq!(lease_end(start)?, UnixSeconds::new(9_000 + 4 * 900));
        Ok(())
    }
}
