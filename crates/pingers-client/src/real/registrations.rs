// SPDX-License-Identifier: AGPL-3.0-only
//! The wake's one push registration (pingers-sync's maintenance) wired to this device's
//! groups and seals, and how well wake-up pushes reach the phone (design B.4.9).

use pingers_meetup_session::meetup_log_auth_key;
use pingers_parties::{PartiesError, PartyLifecycle, ProfileChange, ProfilePublication};
use pingers_proto::{GroupId, Subscription, TimeToLive};
use pingers_store::{MailboxPurpose, Session, WakePushes as StoredPushes};
use pingers_sync::{
    GroupMembership, Maintained, Maintenance, PoolTarget, Pushes, SyncError, WakeBudget,
};
use pingers_wake::PriorityWindow;

use super::party_log;
use super::push::TOKEN_LIFETIME;
use super::records::{self, Key, corrupt};
use super::sync::{KNOCK_POOL, MAILBOX_LIFETIME, Net};
use crate::snapshot::{PushDelivery, PushDowngrade};
use crate::{ClientError, Unavailable};

impl Net {
    /// A sealed subscription for a group this device is creating (a Party, a Meetup), if wake-up
    /// pushes can be sealed now; `None` otherwise, and a later maintenance step adds one.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub(crate) async fn own_subscription(&self) -> Result<Option<Subscription>, ClientError> {
        let Some(engine) = self.engine.as_deref() else {
            return Ok(None);
        };
        match self.ensure_wake_push().await {
            Ok(()) => {}
            Err(
                error @ (ClientError::Store(_)
                | ClientError::Records(_)
                | ClientError::Sync(SyncError::Store(_))),
            ) => return Err(error),
            Err(_) => return Ok(None),
        }
        let lifetime = TimeToLive::new(TOKEN_LIFETIME).map_err(|_| corrupt())?;
        Ok(engine.party_subscription(lifetime)?)
    }

    /// Records this device's subscription in a group it created, so pushes naming it are
    /// matched to the group and maintenance keeps it sealed.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub(crate) async fn remember_own_subscription(
        &self,
        group: &GroupId,
        subscription: &Subscription,
    ) -> Result<(), ClientError> {
        let Some(engine) = self.engine.as_deref() else {
            return Ok(());
        };
        Ok(engine.remember_subscription(group, subscription).await?)
    }
}

/// The FCM priority window as stored, or an empty one.
pub(super) fn priority_window(session: &Session) -> Result<PriorityWindow, ClientError> {
    Ok(records::read(session, &Key::PushPriority, |reader| {
        Ok(PriorityWindow::from_parts(reader.u32()?, reader.byte()?))
    })?
    .unwrap_or_default())
}

impl Net {
    /// What this wake's registrations should carry: the current seals, none ("No wake-up
    /// pushes"), or unknown (the relay key or the token could not be had: no seal is touched).
    ///
    /// # Errors
    ///
    /// A storage failure.
    async fn pushes(&self) -> Result<Pushes, ClientError> {
        match self.ensure_wake_push().await {
            Ok(()) => Ok(Pushes::Current),
            Err(ClientError::Unavailable(Unavailable::WakePushesOff)) => {
                self.clear_wake_push();
                Ok(Pushes::Off)
            }
            Err(
                error @ (ClientError::Store(_)
                | ClientError::Records(_)
                | ClientError::Sync(SyncError::Store(_))),
            ) => Err(error),
            Err(_) => Ok(Pushes::Unknown),
        }
    }

    /// Every group this device is in, with the key its own requests about it are signed
    /// with: its Parties, and the Meetups it is in now. A Meetup whose key cannot be read (it
    /// is ending) is left out, and its subscription with it.
    ///
    /// # Errors
    ///
    /// A storage failure; [`ClientError::Parties`] for a Party whose key cannot be derived.
    async fn memberships(&self) -> Result<Vec<GroupMembership>, ClientError> {
        let (mut memberships, meetups) = self
            .backend
            .store(|session, clock, _rng| {
                let mut memberships = Vec::new();
                for party in party_log::parties(session)? {
                    let row = session
                        .parties()
                        .get(&party)?
                        .ok_or(PartiesError::UnknownParty)?;
                    let key = PartyLifecycle::new(session, clock).log_auth_key(&party)?;
                    memberships.push(GroupMembership {
                        group: row.group,
                        key,
                    });
                }
                let meetups: Vec<_> = session
                    .meetups()
                    .list()?
                    .into_iter()
                    .map(|meetup| meetup.group)
                    .collect();
                Ok((memberships, meetups))
            })
            .await?;
        for group in meetups {
            if let Ok(key) = meetup_log_auth_key(&self.backend.database, group).await {
                memberships.push(GroupMembership { group, key });
            }
        }
        Ok(memberships)
    }

    /// The one push registration a wake makes: seals the platform's token again if it, the
    /// relay key or the seal's lifetime calls for it, then lets pingers-sync renew the most
    /// urgent stale registration or refill the knock pool.
    ///
    /// # Errors
    ///
    /// [`Unavailable::Sync`] without an instance; [`ClientError::Sync`]; a storage failure.
    pub(crate) async fn maintain_push_registrations(&self) -> Result<Maintained, ClientError> {
        let engine = self.engine()?;
        let pushes = self.pushes().await?;
        let groups = self.memberships().await?;
        let maintenance = Maintenance {
            pool: PoolTarget {
                purpose: MailboxPurpose::Knock,
                size: KNOCK_POOL,
                lifetime: TimeToLive::new(MAILBOX_LIFETIME).map_err(|_| corrupt())?,
            },
            groups: &groups,
            subscription_lifetime: TimeToLive::new(TOKEN_LIFETIME).map_err(|_| corrupt())?,
            pushes,
        };
        let result = engine
            .maintain_push_registrations(&maintenance, WakeBudget::UNLIMITED)
            .await;
        self.observe(&result);
        let maintained = result?;
        if matches!(
            maintained,
            Maintained::SubscriptionAdded | Maintained::SubscriptionRemoved | Maintained::Forgotten
        ) {
            self.announce_subscriptions().await?;
        }
        Ok(maintained)
    }

    /// Tells each Party whose subscription this device added or removed since the members were
    /// last told (a profile operation naming it), so whoever removes this device later drops
    /// the right one from the server (design B.2). A resealed subscription keeps its id and
    /// needs no word.
    ///
    /// # Errors
    ///
    /// A storage failure; [`ClientError::Parties`].
    async fn announce_subscriptions(&self) -> Result<(), ClientError> {
        let outbound = self
            .backend
            .store(|session, clock, rng| {
                let Some(identity) = session.identity().load()? else {
                    return Ok(Vec::new());
                };
                let own_key = identity.signing_key.public_key();
                let lifecycle = PartyLifecycle::new(session, clock);
                let mut outbound = Vec::new();
                for party in party_log::parties(session)? {
                    let row = session
                        .parties()
                        .get(&party)?
                        .ok_or(PartiesError::UnknownParty)?;
                    let held = session
                        .subscriptions()
                        .get(&row.group)?
                        .map(|subscription| subscription.id);
                    if session.member_subscriptions().get(&party, &own_key)? == held {
                        continue;
                    }
                    match lifecycle.update_profile(
                        &party,
                        ProfileChange::Subscription(held),
                        rng,
                    )? {
                        ProfilePublication::Send(message) => outbound.push(*message),
                        // Sent with the waiting profile once the commit settles.
                        ProfilePublication::Waiting => {}
                    }
                }
                Ok(outbound)
            })
            .await?;
        if outbound.is_empty() {
            return Ok(());
        }
        self.push(outbound);
        self.flush().await
    }

    /// One maintenance step right after the token or the setting changed, so the first stale
    /// registration is renewed at once. Only a storage failure is an error: anything else is
    /// retried by the next wake's step.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub(crate) async fn maintain_now(&self) -> Result<(), ClientError> {
        match self.maintain_push_registrations().await {
            Err(
                error @ (ClientError::Store(_)
                | ClientError::Records(_)
                | ClientError::Sync(SyncError::Store(_))),
            ) => Err(error),
            Ok(_) | Err(_) => Ok(()),
        }
    }

    /// Whether wake-up pushes reach this phone, most fundamental reason first: the setting,
    /// the build and server, the platform's token, the relay key, then FCM's priority.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub(crate) async fn push_delivery(&self) -> Result<PushDelivery, ClientError> {
        let (setting, window) = self
            .backend
            .store(|session, _clock, _rng| {
                Ok((session.settings().wake_pushes()?, priority_window(session)?))
            })
            .await?;
        let downgrade = if setting == StoredPushes::Disabled {
            Some(PushDowngrade::WakePushesOff)
        } else if !self.has_push_relay() {
            Some(PushDowngrade::NoPushRelay)
        } else if !self.has_push_token() {
            Some(PushDowngrade::NoPushToken)
        } else if let Some(problem) = self.push_problem() {
            Some(problem)
        } else if window.is_downgraded() {
            Some(PushDowngrade::PriorityLowered)
        } else {
            None
        };
        Ok(downgrade.map_or(PushDelivery::Full, PushDelivery::Downgraded))
    }
}
