// SPDX-License-Identifier: AGPL-3.0-only
//! The client's mailbox lifecycle and unanswered knocks (design B.4.4, B.4.9).

use core::time::Duration;

use pingers_crypto::{
    MailboxReadKey, PushRelayPublicKey, PushTokenToSeal, mailbox_id, push_bind_for_mailbox,
    seal_push_token,
};
use pingers_proto::{
    MailboxSpec, PushHandle, PushTokenPlaintext, RelayRef, SealedPush, TimeToLive, Urgency,
};
use pingers_time::{Bucket, UnixSeconds};
use rand_core::CryptoRng;

use crate::PresenceError;

/// What a mailbox is for (design C.2 `mailbox.purpose`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MailboxPurpose {
    /// Named by one real board record; lives one lease.
    Knock,
    /// Named in one knock for the Welcome; lives one lease.
    Reply,
    /// Named in an invite link; lives until the invite expires.
    Invite,
}

/// Whether, and how, a mailbox can wake its owner.
pub enum PushSealing<'push> {
    /// No push token ("No wake-up pushes", or the relay key is not trusted).
    NoWake,
    /// Seal the device's token to this relay.
    Seal {
        /// Which relay, as the server knows it.
        relay: RelayRef,
        /// The relay's pinned key.
        relay_key: &'push PushRelayPublicKey,
        /// The token, class and expiry to seal.
        token: &'push PushTokenPlaintext,
        /// Priority for class Standard.
        urgency: Urgency,
    },
}

/// A mailbox ready to register: the spec to send, and the read key to keep.
pub struct MailboxRegistration {
    /// Stays on the device (pingers-store `mailbox` table).
    pub read_key: MailboxReadKey,
    /// The body of `POST /v1/mailboxes`.
    pub spec: MailboxSpec,
}

/// What kind of mailbox to prepare.
pub struct MailboxRequest<'push> {
    /// What it is for.
    pub purpose: MailboxPurpose,
    /// How long the server keeps it: one lease for knock and reply mailboxes.
    pub time_to_live: TimeToLive,
    /// Whether and how it can wake this device.
    pub push: PushSealing<'push>,
}

/// Generates a read key, derives the mailbox id, and seals the push token fresh for this
/// mailbox with a random handle. Register at most one per wake, never in the wake that uses
/// it (design B.4.4, F.3); that scheduling is pingers-sync's job.
///
/// Randomness is drawn in this order: the 32-byte read key, then (when sealing) the 8-byte
/// push handle and the HPKE randomness of `pingers_crypto::seal_push_token`.
///
/// # Errors
///
/// [`PresenceError::MailboxRule`] if the lifetime is over [`MAX_MAILBOX_LIFETIME`];
/// [`PresenceError::Crypto`] if sealing the push token fails.
pub fn prepare_mailbox(
    request: &MailboxRequest<'_>,
    rng: &mut impl CryptoRng,
) -> Result<MailboxRegistration, PresenceError> {
    if request.time_to_live.duration() > MAX_MAILBOX_LIFETIME {
        return Err(PresenceError::MailboxRule);
    }
    let read_key = MailboxReadKey::generate(rng);
    let mailbox = mailbox_id(&read_key);
    let push = match &request.push {
        PushSealing::NoWake => None,
        PushSealing::Seal {
            relay,
            relay_key,
            token,
            urgency,
        } => {
            let mut handle = [0; PushHandle::LENGTH];
            rng.fill_bytes(&mut handle);
            let blob = seal_push_token(
                relay_key,
                &PushTokenToSeal {
                    bind: push_bind_for_mailbox(&mailbox),
                    plaintext: token,
                },
                rng,
            )?;
            Some(SealedPush {
                relay: relay.clone(),
                blob,
                urgency: *urgency,
                handle: PushHandle::from_bytes(handle),
            })
        }
    };
    Ok(MailboxRegistration {
        read_key,
        spec: MailboxSpec {
            mailbox_id: mailbox,
            time_to_live: request.time_to_live,
            push,
        },
    })
}

/// One lease: how long knock and reply mailboxes live, and how long a knock may go unanswered.
pub const LEASE_DURATION: Duration = Duration::from_hours(1);

/// The longest a mailbox may live: 30 days (design B.4.9).
pub const MAX_MAILBOX_LIFETIME: Duration = Duration::from_hours(30 * 24);

/// When a mailbox registered at `registered_at` expires, or `None` if that is past the end of
/// time. The server drops it then, and the device forgets its read key.
#[must_use]
pub fn mailbox_expires_at(
    registered_at: UnixSeconds,
    time_to_live: TimeToLive,
) -> Option<UnixSeconds> {
    registered_at.checked_add(time_to_live.duration())
}

/// Whether a pooled knock mailbox lives at least as long as a lease starting in `lease_start`
/// (until bucket `lease_start + 4` starts), so a knock posted in the record's last minute still
/// finds it. The pool hands out only mailboxes for which this holds.
#[must_use]
pub fn mailbox_covers_lease(
    registered_at: UnixSeconds,
    time_to_live: TimeToLive,
    lease_start: Bucket,
) -> bool {
    let needed_until = lease_start
        .checked_add(crate::LEASE_BUCKETS)
        .map(Bucket::start);
    match (
        mailbox_expires_at(registered_at, time_to_live),
        needed_until,
    ) {
        (Some(expires), Some(needed)) => expires >= needed,
        // A lease that never ends cannot be covered; a mailbox that never expires covers any.
        (_, None) => false,
        (None, Some(_)) => true,
    }
}

/// Whether a knock sent at `sent_at` and not answered by `now` should raise
/// `Notice::UnansweredKnock` (design B.4.4 step 7: nothing answered within one lease).
#[must_use]
pub fn knock_unanswered(sent_at: UnixSeconds, now: UnixSeconds) -> bool {
    now.saturating_duration_since(sent_at) >= LEASE_DURATION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knock_is_unanswered_after_one_lease() {
        let sent = UnixSeconds::new(10_000);
        assert!(!knock_unanswered(sent, UnixSeconds::new(13_599)));
        assert!(knock_unanswered(sent, UnixSeconds::new(13_600)));
    }
}
