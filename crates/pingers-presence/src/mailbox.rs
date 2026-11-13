// SPDX-License-Identifier: AGPL-3.0-only
//! The client's mailbox lifecycle and unanswered knocks (design B.4.4, B.4.9).

use core::time::Duration;

use pingers_crypto::{MailboxReadKey, PushRelayPublicKey};
use pingers_proto::{MailboxSpec, PushTokenPlaintext, RelayRef, TimeToLive, Urgency};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;

use crate::PresenceError;
use crate::stub::not_implemented;

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
/// # Errors
///
/// [`PresenceError::Crypto`] if sealing the push token fails.
pub fn prepare_mailbox(
    request: &MailboxRequest<'_>,
    rng: &mut impl CryptoRng,
) -> Result<MailboxRegistration, PresenceError> {
    not_implemented((request, rng))
}

/// One lease: how long knock and reply mailboxes live, and how long a knock may go unanswered.
pub const LEASE_DURATION: Duration = Duration::from_hours(1);

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
