// SPDX-License-Identifier: AGPL-3.0-only
//! The mailbox endpoints and their rules (design B.4.9, D).

use core::num::NonZeroUsize;
use std::sync::{Mutex, PoisonError};

use pingers_crypto::{MailboxReadKey, mailbox_id, push_bind_for_mailbox};
use pingers_presence::{check_mailbox_post, check_mailbox_spec};
use pingers_proto::{MailboxId, MailboxMessages, MailboxPush, MailboxSpec, Padded, SequenceNumber};
use pingers_time::Clock;
use rand_core::CryptoRng;

use crate::MailboxError;
use crate::limit::{Admission, PostLimit};
use crate::relay::RelayDirectory;
use crate::storage::{MailboxStorage, Removal, StoredMailbox};
use crate::wake::{ForgetPush, PushJob, push_delay};
use pingers_proto::PushRequest;

/// Limits the operator can tune.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailboxRules {
    /// The most messages one mailbox holds; further posts are refused until it is read and
    /// deleted, or expires. Bounds storage per mailbox (64 × 32 KiB = 2 MiB by default).
    pub max_messages: NonZeroUsize,
}

impl MailboxRules {
    /// 64 messages per mailbox: far more than the few knocks one lease attracts.
    pub const DEFAULT: Self = Self {
        max_messages: match NonZeroUsize::new(64) {
            Some(max) => max,
            None => NonZeroUsize::MIN,
        },
    };
}

/// What a [`Mailboxes`] is built from.
pub struct MailboxParts<S, C, D, L> {
    /// Where mailboxes are kept.
    pub storage: S,
    /// Wall time, for expiry.
    pub clock: C,
    /// The relays this server forwards to.
    pub directory: D,
    /// The per-mailbox post limit.
    pub limit: L,
    /// The operator's limits.
    pub rules: MailboxRules,
}

/// The outcome of a stored post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Posted {
    /// The message's position in the mailbox.
    pub sequence: SequenceNumber,
    /// The push to forward, if the mailbox has a sealed push whose relay is still configured.
    pub wake: Option<PushJob>,
}

/// Anonymous mailboxes: register, post, read, delete, expire.
///
/// Nothing here learns who owns or posts to a mailbox. The sealed push is forwarded as it was
/// registered and never opened.
pub struct Mailboxes<S, C, D, L> {
    storage: S,
    clock: C,
    directory: D,
    limit: L,
    rules: MailboxRules,
}

impl<S: MailboxStorage, C: Clock, D: RelayDirectory, L: PostLimit> Mailboxes<S, C, D, L> {
    /// Assembles the service.
    #[must_use]
    pub fn new(parts: MailboxParts<S, C, D, L>) -> Self {
        Self {
            storage: parts.storage,
            clock: parts.clock,
            directory: parts.directory,
            limit: parts.limit,
            rules: parts.rules,
        }
    }

    /// `POST /v1/mailboxes`: registers one mailbox. An expired mailbox with the same id that
    /// has not been swept yet is replaced.
    ///
    /// # Errors
    ///
    /// [`MailboxError::LifetimeTooLong`] for a lifetime over 30 days;
    /// [`MailboxError::UnknownRelay`] if the sealed push names a relay this server does not
    /// forward to; [`MailboxError::AlreadyExists`] if a live mailbox has the id;
    /// [`MailboxError::Storage`] if the storage fails.
    pub fn register(&mut self, spec: &MailboxSpec) -> Result<(), MailboxError> {
        check_mailbox_spec(spec).map_err(MailboxError::LifetimeTooLong)?;
        if let Some(push) = &spec.push
            && self.directory.resolve(&push.relay).is_none()
        {
            return Err(MailboxError::UnknownRelay);
        }
        let now = self.clock.now();
        if let Some(existing) = self.storage.mailbox(&spec.mailbox_id)? {
            if !existing.expired(now) {
                return Err(MailboxError::AlreadyExists);
            }
            self.storage.delete_mailbox(&spec.mailbox_id)?;
        }
        let expires_at = now
            .checked_add(spec.time_to_live.duration())
            .ok_or(MailboxError::Internal)?;
        self.storage.insert_mailbox(&StoredMailbox {
            id: spec.mailbox_id,
            push: spec.push.clone(),
            expires_at,
        })?;
        Ok(())
    }

    /// `POST /v1/mailboxes/{id}/msgs`: stores a post and returns the push to forward, if any
    /// (the wake rule). The caller runs the job with [`crate::forward`]; storing does not
    /// wait for the relay.
    ///
    /// # Errors
    ///
    /// [`MailboxError::NotAMailboxClass`] unless the post is 1, 4, 8 or 32 KiB;
    /// [`MailboxError::NotFound`] if no live mailbox has the id; [`MailboxError::Full`] if it
    /// holds [`MailboxRules::max_messages`]; [`MailboxError::RateLimited`] if the post limit
    /// refuses; [`MailboxError::Storage`] if the storage fails.
    pub fn post(
        &mut self,
        id: &MailboxId,
        body: &Padded,
        rng: &mut impl CryptoRng,
    ) -> Result<Posted, MailboxError> {
        check_mailbox_post(body.as_bytes()).map_err(MailboxError::NotAMailboxClass)?;
        let now = self.clock.now();
        let mailbox = self.live(id)?;
        if self.storage.message_count(id)? >= self.rules.max_messages.get() {
            return Err(MailboxError::Full);
        }
        if self.limit.admit(id, now) == Admission::Refuse {
            return Err(MailboxError::RateLimited);
        }
        let sequence = self.storage.append_message(id, body)?;
        let wake = mailbox.push.and_then(|push| {
            let target = self.directory.resolve(&push.relay)?;
            Some(PushJob {
                mailbox: *id,
                request: PushRequest {
                    key_id: push.key_id,
                    blob: push.blob,
                    bind: push_bind_for_mailbox(id),
                    urgency: push.urgency,
                    handle: push.handle,
                },
                target,
                delay: push_delay(push.urgency, rng),
            })
        });
        Ok(Posted { sequence, wake })
    }

    /// `GET /v1/mailboxes/{id}/msgs?after=seq`: the messages after `after`, for the holder of
    /// the read key.
    ///
    /// # Errors
    ///
    /// [`MailboxError::WrongReadKey`] if the key does not derive `id`;
    /// [`MailboxError::NotFound`] if no live mailbox has the id;
    /// [`MailboxError::Storage`] if the storage fails.
    pub fn read(
        &self,
        id: &MailboxId,
        read_key: &MailboxReadKey,
        after: Option<SequenceNumber>,
    ) -> Result<MailboxMessages, MailboxError> {
        check_read_key(id, read_key)?;
        self.live(id)?;
        Ok(MailboxMessages {
            messages: self.storage.messages_after(id, after)?,
        })
    }

    /// `DELETE /v1/mailboxes/{id}/msgs?through=seq`: the holder of the read key deletes the
    /// messages it has read, up to and including `through`, freeing room in the mailbox
    /// ([`MailboxRules::max_messages`]) so that posts it has handled cannot fill it.
    ///
    /// # Errors
    ///
    /// [`MailboxError::WrongReadKey`] if the key does not derive `id`;
    /// [`MailboxError::NotFound`] if no live mailbox has the id;
    /// [`MailboxError::Storage`] if the storage fails.
    pub fn delete_read(
        &mut self,
        id: &MailboxId,
        read_key: &MailboxReadKey,
        through: SequenceNumber,
    ) -> Result<(), MailboxError> {
        check_read_key(id, read_key)?;
        self.live(id)?;
        Ok(self.storage.delete_messages_through(id, through)?)
    }

    /// `DELETE /v1/mailboxes/{id}`: deletes the mailbox and its messages now.
    ///
    /// # Errors
    ///
    /// [`MailboxError::WrongReadKey`] if the key does not derive `id`;
    /// [`MailboxError::NotFound`] if no live mailbox has the id;
    /// [`MailboxError::Storage`] if the storage fails.
    pub fn delete(
        &mut self,
        id: &MailboxId,
        read_key: &MailboxReadKey,
    ) -> Result<(), MailboxError> {
        check_read_key(id, read_key)?;
        self.live(id)?;
        match self.storage.delete_mailbox(id)? {
            Removal::Removed => Ok(()),
            Removal::Absent => Err(MailboxError::NotFound),
        }
    }

    /// `PUT /v1/mailboxes/{id}/push`: replaces the mailbox's sealed push (`None` removes it),
    /// for the holder of the read key. The lifetime and the messages stay as they are.
    ///
    /// # Errors
    ///
    /// [`MailboxError::WrongReadKey`] if the key does not derive `id`;
    /// [`MailboxError::NotFound`] if no live mailbox has the id;
    /// [`MailboxError::UnknownRelay`] if the push names a relay this server does not forward
    /// to; [`MailboxError::Storage`] if the storage fails.
    pub fn replace_push(
        &mut self,
        id: &MailboxId,
        read_key: &MailboxReadKey,
        update: &MailboxPush,
    ) -> Result<(), MailboxError> {
        check_read_key(id, read_key)?;
        self.live(id)?;
        if let Some(push) = &update.push
            && self.directory.resolve(&push.relay).is_none()
        {
            return Err(MailboxError::UnknownRelay);
        }
        self.storage.set_push(id, update.push.as_ref())?;
        Ok(())
    }

    /// Deletes every expired mailbox and its messages. The server calls this on a timer;
    /// requests already treat expired mailboxes as absent.
    ///
    /// # Errors
    ///
    /// [`MailboxError::Storage`] if the storage fails.
    pub fn expire(&mut self) -> Result<(), MailboxError> {
        self.storage.delete_expired(self.clock.now())?;
        Ok(())
    }

    /// Deletes a mailbox's sealed push after its relay answered 410.
    ///
    /// # Errors
    ///
    /// [`MailboxError::Storage`] if the storage fails.
    pub fn push_gone(&mut self, id: &MailboxId) -> Result<(), MailboxError> {
        self.storage.set_push(id, None)?;
        Ok(())
    }

    /// The storage, for inspection.
    #[must_use]
    pub const fn storage(&self) -> &S {
        &self.storage
    }

    fn live(&self, id: &MailboxId) -> Result<StoredMailbox, MailboxError> {
        self.storage
            .mailbox(id)?
            .filter(|mailbox| !mailbox.expired(self.clock.now()))
            .ok_or(MailboxError::NotFound)
    }
}

/// The read key is the capability: it must derive the mailbox id.
fn check_read_key(id: &MailboxId, read_key: &MailboxReadKey) -> Result<(), MailboxError> {
    // The id is public to every poster, so comparing it in variable time leaks nothing.
    if mailbox_id(read_key) == *id {
        Ok(())
    } else {
        Err(MailboxError::WrongReadKey)
    }
}

impl<S: MailboxStorage, C: Clock, D: RelayDirectory, L: PostLimit> ForgetPush
    for Mutex<Mailboxes<S, C, D, L>>
{
    fn forget_push(&self, mailbox: &MailboxId) -> Result<(), MailboxError> {
        self.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_gone(mailbox)
    }
}

#[cfg(test)]
mod tests;
