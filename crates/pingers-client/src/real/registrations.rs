// SPDX-License-Identifier: AGPL-3.0-only
//! Each server's one push registration of a wake (pingers-sync's maintenance) wired to this
//! device's groups on that server and its seals, and how well wake-up pushes reach the phone
//! (design B.4.9).

use pingers_meetup_session::meetup_log_auth_key;
use pingers_parties::{PartyLifecycle, ProfileChange, ProfilePublication};
use pingers_proto::{GroupId, ServerId, Subscription, TimeToLive};
use pingers_store::{Session, WakePushes as StoredPushes};
use pingers_sync::{
    GroupMembership, Maintained, Maintenance, PoolTarget, Pushes, SyncEngine, SyncError, WakeBudget,
};
use pingers_wake::PriorityWindow;

use super::push::TOKEN_LIFETIME;
use super::records::{self, Key, corrupt};
use super::servers::Link;
use super::sync::{MAILBOX_LIFETIME, Net, POOL_ORDER, PoolStep};
use crate::snapshot::{PushDelivery, PushDowngrade};
use crate::{ClientError, Unavailable};

impl Net {
    /// A sealed subscription for a group this device is creating on `server` (a Party, a
    /// Meetup), if wake-up pushes can be sealed for that server now; `None` otherwise, and a
    /// later maintenance step adds one.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub(crate) async fn own_subscription(
        &self,
        server: &ServerId,
    ) -> Result<Option<Subscription>, ClientError> {
        let Ok(link) = self.link(server).await else {
            return Ok(None);
        };
        match self.ensure_wake_push(server).await {
            Ok(()) => {}
            Err(
                error @ (ClientError::Store(_)
                | ClientError::Records(_)
                | ClientError::Sync(SyncError::Store(_))),
            ) => return Err(error),
            Err(_) => return Ok(None),
        }
        let lifetime = TimeToLive::new(TOKEN_LIFETIME).map_err(|_never| corrupt())?;
        Ok(link.engine.party_subscription(lifetime)?)
    }

    /// Records this device's subscription in a group it created on `server`, so pushes naming
    /// it are matched to the group and maintenance keeps it sealed.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub(crate) async fn remember_own_subscription(
        &self,
        server: &ServerId,
        group: &GroupId,
        subscription: &Subscription,
    ) -> Result<(), ClientError> {
        let Ok(link) = self.link(server).await else {
            return Ok(());
        };
        Ok(link
            .engine
            .remember_subscription(group, subscription)
            .await?)
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
    /// What this wake's registrations on `link`'s server should carry: the current seals, none
    /// ("No wake-up pushes"), or unknown (the relay key or the token could not be had: no seal
    /// is touched).
    ///
    /// # Errors
    ///
    /// A storage failure.
    async fn pushes(&self, link: &Link) -> Result<Pushes, ClientError> {
        match self.ensure_wake_push(&link.server).await {
            Ok(()) => Ok(Pushes::Current),
            Err(ClientError::Unavailable(Unavailable::WakePushesOff)) => {
                link.clear_wake_push();
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

    /// Every group this device is in on `server`, with the key its own requests about it are
    /// signed with: its Parties there, and the Meetups of those Parties it is in now. A Meetup
    /// whose key cannot be read (it is ending) is left out, and its subscription with it.
    ///
    /// # Errors
    ///
    /// A storage failure; [`ClientError::Parties`] for a Party whose key cannot be derived.
    async fn memberships(&self, server: &ServerId) -> Result<Vec<GroupMembership>, ClientError> {
        let server = server.clone();
        let (mut memberships, meetups) = self
            .backend
            .store(move |session, clock, _rng| {
                let mut memberships = Vec::new();
                let mut here = Vec::new();
                for row in session.parties().list()? {
                    if row.server != server {
                        continue;
                    }
                    let key = PartyLifecycle::new(session, clock).log_auth_key(&row.id)?;
                    memberships.push(GroupMembership {
                        group: row.group,
                        key,
                    });
                    here.push(row.id);
                }
                let meetups: Vec<_> = session
                    .meetups()
                    .list()?
                    .into_iter()
                    .filter(|meetup| here.contains(&meetup.party))
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

    /// The one push registration a wake makes on `server`: seals the platform's token again if
    /// it, the relay key or the seal's lifetime calls for it, then lets that server's engine
    /// renew its most urgent stale registration or refill its knock pool.
    ///
    /// # Errors
    ///
    /// [`Unavailable::UnknownServer`]; [`ClientError::Sync`]; a storage failure.
    pub(crate) async fn maintain_push_registrations(
        &self,
        server: &ServerId,
    ) -> Result<Maintained, ClientError> {
        let link = self.link(server).await?;
        let pushes = self.pushes(&link).await?;
        let groups = self.memberships(server).await?;
        let maintenance = Maintenance {
            pool: Self::pool_short_of_target(&link.engine).await?,
            groups: &groups,
            subscription_lifetime: TimeToLive::new(TOKEN_LIFETIME).map_err(|_never| corrupt())?,
            pushes,
        };
        let result = link
            .engine
            .maintain_push_registrations(&maintenance, WakeBudget::UNLIMITED)
            .await;
        link.observe(&result);
        let maintained = result?;
        if matches!(
            maintained,
            Maintained::SubscriptionAdded | Maintained::SubscriptionRemoved | Maintained::Forgotten
        ) {
            self.announce_subscriptions(&link.server).await?;
        }
        Ok(maintained)
    }

    /// Tells each Party on `server` whose subscription this device added or removed since the
    /// members were last told (a profile operation naming it), so whoever removes this device
    /// later drops the right one from that server (design B.2). A resealed subscription keeps
    /// its id and needs no word.
    ///
    /// # Errors
    ///
    /// A storage failure; [`ClientError::Parties`].
    async fn announce_subscriptions(&self, server: &ServerId) -> Result<(), ClientError> {
        let on = server.clone();
        let outbound = self
            .backend
            .store(move |session, clock, rng| {
                let Some(identity) = session.identity().load()? else {
                    return Ok(Vec::new());
                };
                let own_key = identity.signing_key.public_key();
                let lifecycle = PartyLifecycle::new(session, clock);
                let scoped = session.server(&on)?;
                let mut outbound = Vec::new();
                for row in session.parties().list()? {
                    if row.server != on {
                        continue;
                    }
                    let held = scoped
                        .subscriptions()
                        .get(&row.group)?
                        .map(|subscription| subscription.id);
                    if session.member_subscriptions().get(&row.id, &own_key)? == held {
                        continue;
                    }
                    match lifecycle.update_profile(
                        &row.id,
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
        self.push(server, outbound);
        self.flush().await
    }

    /// The pool this wake tops up on `engine`'s server: the first in [`POOL_ORDER`] short of its size (a first knock
    /// mailbox, a first reply mailbox, then both to full), or the last when all are full.
    ///
    /// # Errors
    ///
    /// A storage failure.
    async fn pool_short_of_target(engine: &SyncEngine) -> Result<PoolTarget, ClientError> {
        let lifetime = TimeToLive::new(MAILBOX_LIFETIME).map_err(|_| corrupt())?;
        let mut chosen = None;
        for PoolStep { purpose, size } in POOL_ORDER {
            chosen = Some(PoolTarget {
                purpose,
                size,
                lifetime,
            });
            if engine.pooled_mailboxes(purpose).await? < size {
                break;
            }
        }
        chosen.ok_or_else(corrupt)
    }

    /// One maintenance step on every server right after the token or the setting changed, so
    /// the first stale registration on each is renewed at once. Only a storage failure is an
    /// error: anything else is retried by the next wake's step.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub(crate) async fn maintain_now(&self) -> Result<(), ClientError> {
        for link in self.servers.all().await? {
            // Only a storage failure stops the round; another server is still tried.
            if let Err(
                error @ (ClientError::Store(_)
                | ClientError::Records(_)
                | ClientError::Sync(SyncError::Store(_))),
            ) = self.maintain_push_registrations(&link.server).await
            {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Whether wake-up pushes reach this phone, most fundamental reason first: the setting,
    /// the build and servers, the platform's token, the relay keys, then FCM's priority. With
    /// several servers, a server whose pushes could not be sealed downgrades delivery (the
    /// built-in server's reason first), as its Parties then wake nobody.
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
        let mut links = self.servers.all().await?;
        let built_in = self.servers.built_in().cloned();
        links.sort_by_key(|link| Some(&link.server) != built_in.as_ref());
        let downgrade = if setting == StoredPushes::Disabled {
            Some(PushDowngrade::WakePushesOff)
        } else if !links.iter().any(|link| link.has_push_relay()) {
            Some(PushDowngrade::NoPushRelay)
        } else if !links.iter().any(|link| link.has_push_token()) {
            Some(PushDowngrade::NoPushToken)
        } else if let Some(problem) = links.iter().find_map(|link| link.push_problem()) {
            Some(problem)
        } else if window.is_downgraded() {
            Some(PushDowngrade::PriorityLowered)
        } else {
            None
        };
        Ok(downgrade.map_or(PushDelivery::Full, PushDelivery::Downgraded))
    }
}
