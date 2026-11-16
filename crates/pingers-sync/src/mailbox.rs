// SPDX-License-Identifier: AGPL-3.0-only
//! Mailbox lifecycle and sealed push (design B.4.4, B.4.9): register one-time mailboxes into
//! a pool on earlier wakes, hand them out, read and delete them; seal push tokens for
//! mailboxes and group subscriptions. What the posts mean is the owner's business.

use pingers_crypto::{
    MailboxReadKey, PushRelayPublicKey, PushTokenToSeal, SubscriptionOwnerKey, mailbox_id,
    push_bind_for_mailbox, push_bind_for_subscription, seal_push_token,
};
use pingers_proto::{
    GroupId, MailboxId, MailboxMessages, MailboxSpec, MessageAccepted, Padded, PushBind,
    PushHandle, PushRelayKeyId, PushTokenPlaintext, RelayRef, SealedPush, SequenceNumber,
    Subscription, SubscriptionId, TimeToLive, Urgency,
};
use pingers_store::{Mailbox, MailboxPurpose, MailboxState, SealStamp, StoredSubscription};
use pingers_time::UnixSeconds;

use crate::registrations::stamp_of;
use crate::{
    ApiRequest, NotSentReason, Outcome, Sending, SyncEngine, SyncError, SyncRng, WakeBudget,
};

/// What a sealed push token is made from (design B.4.9).
#[derive(Clone, Debug)]
pub struct PushSeal {
    /// Which push relay can open it.
    pub relay: RelayRef,
    /// That relay's HPKE key (from its pinned key config).
    pub relay_key: PushRelayPublicKey,
    /// The device's push token, platform, class and `not_after`.
    pub token: PushTokenPlaintext,
    /// Alert or Quiet, for class Standard.
    pub urgency: Urgency,
}

/// A mailbox to register.
#[derive(Clone, Debug)]
pub struct NewMailbox {
    /// What it is for.
    pub purpose: MailboxPurpose,
    /// How long the server keeps it.
    pub lifetime: TimeToLive,
    /// The push that wakes this device on a post; `None` when wake-up pushes are off.
    pub push: Option<PushSeal>,
}

/// What became of a request whose answer the caller wants when it is sent.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum Delivered<T> {
    /// Sent and accepted.
    Done(T),
    /// Not sent; queued for a later flush.
    Queued(NotSentReason),
    /// Not sent; dropped.
    Dropped(NotSentReason),
}

impl<T> Delivered<T> {
    /// Applies `accept` to an answer; carries the not-sent cases over.
    pub(crate) fn from_outcome(
        outcome: Outcome,
        accept: impl FnOnce(crate::Response) -> Result<T, SyncError>,
    ) -> Result<Self, SyncError> {
        match outcome {
            Outcome::Answered(response) => accept(response.success()?).map(Self::Done),
            Outcome::Queued(reason) => Ok(Self::Queued(reason)),
            Outcome::Dropped(reason) => Ok(Self::Dropped(reason)),
        }
    }
}

fn random_handle(rng: &mut SyncRng) -> PushHandle {
    let mut bytes = [0_u8; PushHandle::LENGTH];
    rng.fill_bytes(&mut bytes);
    PushHandle::from_bytes(bytes)
}

/// Seals `seal` for `bind` with a fresh handle.
pub(crate) fn sealed_push(
    seal: &PushSeal,
    bind: PushBind,
    rng: &mut SyncRng,
) -> Result<SealedPush, SyncError> {
    let blob = seal_push_token(
        &seal.relay_key,
        &PushTokenToSeal {
            bind,
            plaintext: &seal.token,
        },
        rng,
    )?;
    Ok(SealedPush {
        relay: seal.relay.clone(),
        key_id: PushRelayKeyId::new(seal.relay_key.key_id()),
        blob,
        urgency: seal.urgency,
        handle: random_handle(rng),
    })
}

fn expiry(now: UnixSeconds, lifetime: TimeToLive) -> Result<UnixSeconds, SyncError> {
    now.checked_add(lifetime.duration())
        .ok_or(SyncError::OutOfRange("mailbox expiry"))
}

impl SyncEngine {
    /// Registers one mailbox (`POST /v1/mailboxes`, anonymous, one per request) and adds it
    /// to the pool once the server has accepted it. Not queued: a pool refill that misses
    /// this wake is tried on the next.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] if the server refuses; as [`SyncEngine::send`] otherwise.
    pub async fn register_mailbox(
        &self,
        mailbox: &NewMailbox,
        budget: WakeBudget,
    ) -> Result<Delivered<MailboxId>, SyncError> {
        let (read_key, push) = self.with_rng(|rng| {
            let read_key = MailboxReadKey::generate(rng);
            let id = mailbox_id(&read_key);
            let push = mailbox
                .push
                .as_ref()
                .map(|seal| sealed_push(seal, push_bind_for_mailbox(&id), rng))
                .transpose()?;
            Ok::<_, SyncError>((read_key, push))
        })?;
        let id = mailbox_id(&read_key);
        let handle = push.as_ref().map(|push| push.handle);
        let seal = mailbox.push.as_ref().map(stamp_of).transpose()?;
        let request = ApiRequest::create_mailbox(&MailboxSpec {
            mailbox_id: id,
            time_to_live: mailbox.lifetime,
            push,
        })?;
        let outcome = self.send(request, Sending::or_drop(budget)).await?;
        match Delivered::from_outcome(outcome, |_| Ok(()))? {
            Delivered::Done(()) => {}
            Delivered::Queued(reason) => return Ok(Delivered::Queued(reason)),
            Delivered::Dropped(reason) => return Ok(Delivered::Dropped(reason)),
        }
        let stored = Mailbox {
            read_key,
            purpose: mailbox.purpose,
            state: MailboxState::Pooled,
            push: handle,
            seal,
            expires_at: expiry(self.now(), mailbox.lifetime)?,
        };
        self.blocking(move |session| Ok(session.mailboxes().insert(&stored)?))
            .await?;
        Ok(Delivered::Done(id))
    }

    /// How many unexpired pooled mailboxes of `purpose` are held.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn pooled_mailboxes(&self, purpose: MailboxPurpose) -> Result<u32, SyncError> {
        let now = self.now();
        self.blocking(move |session| Ok(session.mailboxes().pooled_count(purpose, now)?))
            .await
    }

    /// Hands out a pooled mailbox of `purpose` (marked in use), to be named in a record,
    /// knock or invite.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn take_mailbox(
        &self,
        purpose: MailboxPurpose,
    ) -> Result<Option<MailboxId>, SyncError> {
        let now = self.now();
        self.blocking(move |session| {
            Ok(session
                .mailboxes()
                .take_pooled(purpose, now)?
                .map(|mailbox| mailbox.id()))
        })
        .await
    }

    /// The mailbox request built with the stored read key.
    async fn with_read_key(
        &self,
        mailbox: &MailboxId,
        build: impl FnOnce(&MailboxId, &MailboxReadKey) -> ApiRequest + Send + 'static,
    ) -> Result<ApiRequest, SyncError> {
        let id = *mailbox;
        self.blocking(move |session| {
            let stored = session
                .mailboxes()
                .get(&id)?
                .ok_or(SyncError::UnknownMailbox)?;
            Ok(build(&id, &stored.read_key))
        })
        .await
    }

    /// Reads one of this device's mailboxes after `after` (`None`: from the start). Reads are
    /// never queued: the next wake reads again.
    ///
    /// # Errors
    ///
    /// [`SyncError::UnknownMailbox`]; [`SyncError::NoRelay`]; [`SyncError::Status`] (404
    /// once the server has expired it); as [`SyncEngine::send`].
    pub async fn read_mailbox(
        &self,
        mailbox: &MailboxId,
        after: Option<SequenceNumber>,
        budget: WakeBudget,
    ) -> Result<MailboxMessages, SyncError> {
        let request = self
            .with_read_key(mailbox, move |id, key| {
                ApiRequest::read_mailbox(id, key, after)
            })
            .await?;
        self.exchange(request, budget).await?.success()?.decode()
    }

    /// Posts an already padded body to a mailbox (a knock, a Welcome, a join request), with a
    /// `Mailbox` credential.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] if the server refuses; as [`SyncEngine::send`].
    pub async fn post_to_mailbox(
        &self,
        mailbox: &MailboxId,
        body: &Padded,
        sending: Sending,
    ) -> Result<Delivered<SequenceNumber>, SyncError> {
        let outcome = self
            .send(ApiRequest::post_to_mailbox(mailbox, body), sending)
            .await?;
        Delivered::from_outcome(outcome, |response| {
            Ok(response.decode::<MessageAccepted>()?.sequence)
        })
    }

    /// Deletes one of this device's mailboxes on the server and then locally. A mailbox the
    /// server has already forgotten (404) is deleted locally too.
    ///
    /// # Errors
    ///
    /// [`SyncError::UnknownMailbox`]; [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn delete_mailbox(
        &self,
        mailbox: &MailboxId,
        budget: WakeBudget,
    ) -> Result<(), SyncError> {
        let request = self
            .with_read_key(mailbox, ApiRequest::delete_mailbox)
            .await?;
        let response = self.exchange(request, budget).await?;
        if response.status() != 404 {
            response.success()?;
        }
        let id = *mailbox;
        self.blocking(move |session| Ok(session.mailboxes().delete(&id)?))
            .await
    }

    /// Forgets mailboxes and subscriptions whose lifetime has passed (the server has dropped
    /// them already). Returns how many rows went.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn forget_expired(&self) -> Result<usize, SyncError> {
        let now = self.now();
        self.blocking(move |session| {
            let mailboxes = session.mailboxes().delete_expired(now)?;
            let subscriptions = session.subscriptions().delete_expired(now)?;
            Ok(mailboxes.saturating_add(subscriptions))
        })
        .await
    }

    /// A new sealed-push subscription for a group (random id and handle, sealed fresh), for
    /// the owner to put in `CreateGroup`, a commit's `subs_add`, a join request or a knock.
    ///
    /// # Errors
    ///
    /// [`SyncError::Crypto`] if sealing fails.
    pub fn new_subscription(
        &self,
        seal: &PushSeal,
        lifetime: TimeToLive,
    ) -> Result<Subscription, SyncError> {
        let subscription = self.seal_subscription(None, seal, lifetime)?;
        self.note_issued(subscription.push.handle, stamp_of(seal)?);
        Ok(subscription)
    }

    /// The owner key of this device's subscription `id`, derived from the account key (design D,
    /// "Subscription owner"): the subscription carries its public half, and replacing or
    /// deleting it, or attaching the live channel under it, is signed with it.
    ///
    /// # Errors
    ///
    /// [`SyncError::NoCredential`] before this device has an account; [`SyncError::Store`];
    /// [`SyncError::Crypto`].
    pub fn subscription_owner(
        &self,
        id: &SubscriptionId,
    ) -> Result<SubscriptionOwnerKey, SyncError> {
        let session = self.database.session()?;
        let identity = session
            .server(&self.server())?
            .identity()
            .load()?
            .ok_or(SyncError::NoCredential)?;
        Ok(SubscriptionOwnerKey::derive(&identity.signing_key, id)?)
    }

    /// A subscription under `id` (or a random new one), sealed fresh with a fresh handle.
    pub(crate) fn seal_subscription(
        &self,
        id: Option<SubscriptionId>,
        seal: &PushSeal,
        lifetime: TimeToLive,
    ) -> Result<Subscription, SyncError> {
        let (id, push) = self.with_rng(|rng| {
            let id = id.unwrap_or_else(|| {
                let mut bytes = [0_u8; SubscriptionId::LENGTH];
                rng.fill_bytes(&mut bytes);
                SubscriptionId::from_bytes(bytes)
            });
            let push = sealed_push(seal, push_bind_for_subscription(&id), rng)?;
            Ok::<_, SyncError>((id, push))
        })?;
        Ok(Subscription {
            id,
            push,
            time_to_live: lifetime,
            owner: self.subscription_owner(&id)?.public_key(),
        })
    }

    /// Remembers what a subscription handed out in this process was sealed with, until it is
    /// recorded ([`SyncEngine::remember_subscription`]).
    pub(crate) fn note_issued(&self, handle: PushHandle, seal: SealStamp) {
        self.issued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle, seal);
    }

    /// Records this device's subscription in `group` once the server holds it (after the
    /// group was created or the commit adding it was accepted), so a push carrying its
    /// handle can be matched to the group. Its seal is recorded if it was sealed in this
    /// process; otherwise it counts as stale and is sealed again by a later maintenance step.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`]; [`SyncError::OutOfRange`].
    pub async fn remember_subscription(
        &self,
        group: &GroupId,
        subscription: &Subscription,
    ) -> Result<(), SyncError> {
        let seal = self
            .issued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&subscription.push.handle);
        let stored = StoredSubscription {
            group: *group,
            id: subscription.id,
            handle: subscription.push.handle,
            expires_at: expiry(self.now(), subscription.time_to_live)?,
            seal,
        };
        self.blocking(move |session| Ok(session.subscriptions().put(&stored)?))
            .await
    }
}
