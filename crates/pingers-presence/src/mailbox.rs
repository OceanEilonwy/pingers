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
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_crypto::{PushRelayPrivateKey, open_push_token};
    use pingers_proto::{PushClass, PushPlatform, PushToken, RelayId};
    use pingers_testkit::seeded_rng;

    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn knock_is_unanswered_after_one_lease() {
        let sent = UnixSeconds::new(10_000);
        assert!(!knock_unanswered(sent, UnixSeconds::new(13_599)));
        assert!(knock_unanswered(sent, UnixSeconds::new(13_600)));
    }

    fn one_hour() -> Result<TimeToLive, TestError> {
        Ok(TimeToLive::new(LEASE_DURATION)?)
    }

    #[test]
    fn mailbox_without_wake_has_no_push_and_an_id_from_its_read_key() -> Result<(), TestError> {
        let registration = prepare_mailbox(
            &MailboxRequest {
                purpose: MailboxPurpose::Knock,
                time_to_live: one_hour()?,
                push: PushSealing::NoWake,
            },
            &mut seeded_rng(70),
        )?;
        assert_eq!(
            registration.spec.mailbox_id,
            mailbox_id(&registration.read_key)
        );
        assert_eq!(registration.spec.time_to_live, one_hour()?);
        assert_eq!(registration.spec.push, None);
        Ok(())
    }

    #[test]
    fn sealed_push_opens_only_for_this_mailbox() -> Result<(), TestError> {
        let mut rng = seeded_rng(71);
        let relay_secret = PushRelayPrivateKey::generate(&mut rng);
        let relay_key = relay_secret.public_key(1)?;
        let token = PushTokenPlaintext {
            platform: PushPlatform::Fcm,
            class: PushClass::Location,
            token: PushToken::from_bytes(vec![0x70; 140]),
            not_after: UnixSeconds::new(2_000_000_000),
        };
        let request = MailboxRequest {
            purpose: MailboxPurpose::Knock,
            time_to_live: one_hour()?,
            push: PushSealing::Seal {
                relay: RelayRef::Listed(RelayId::new(3)),
                relay_key: &relay_key,
                token: &token,
                urgency: Urgency::Quiet,
            },
        };
        let first = prepare_mailbox(&request, &mut rng)?;
        let second = prepare_mailbox(&request, &mut rng)?;
        let (Some(push), Some(other)) = (&first.spec.push, &second.spec.push) else {
            return Err(TestError::Malformed("no sealed push"));
        };
        assert_eq!(push.relay, RelayRef::Listed(RelayId::new(3)));
        assert_eq!(push.urgency, Urgency::Quiet);
        let bind = push_bind_for_mailbox(&first.spec.mailbox_id);
        assert_eq!(open_push_token(&relay_secret, &bind, &push.blob)?, token);
        // Moved to another mailbox, the blob does not open.
        let other_bind = push_bind_for_mailbox(&second.spec.mailbox_id);
        assert!(open_push_token(&relay_secret, &other_bind, &push.blob).is_err());
        // Fresh per mailbox: nothing repeats between two registrations of the same token.
        assert_ne!(push.blob, other.blob);
        assert_ne!(push.handle, other.handle);
        assert_ne!(first.spec.mailbox_id, second.spec.mailbox_id);
        Ok(())
    }

    #[test]
    fn mailbox_lifetime_over_thirty_days_is_refused() -> Result<(), TestError> {
        let request = |days: u64| -> Result<MailboxRequest<'static>, TestError> {
            Ok(MailboxRequest {
                purpose: MailboxPurpose::Invite,
                time_to_live: TimeToLive::new(Duration::from_hours(days.saturating_mul(24)))?,
                push: PushSealing::NoWake,
            })
        };
        let mut rng = seeded_rng(72);
        assert!(prepare_mailbox(&request(30)?, &mut rng).is_ok());
        assert!(matches!(
            prepare_mailbox(&request(31)?, &mut rng),
            Err(PresenceError::MailboxRule)
        ));
        Ok(())
    }

    #[test]
    fn pooled_mailbox_must_outlive_the_lease() -> Result<(), TestError> {
        // A lease starting in bucket 1000 lives until bucket 1004 starts (second 903_600).
        let lease = Bucket::from_index(1_000)?;
        let ttl = one_hour()?;
        assert!(mailbox_covers_lease(UnixSeconds::new(900_000), ttl, lease));
        assert!(mailbox_covers_lease(
            UnixSeconds::new(899_000),
            TimeToLive::new(Duration::from_secs(4_600))?,
            lease
        ));
        assert!(!mailbox_covers_lease(
            UnixSeconds::new(900_001),
            TimeToLive::new(Duration::from_secs(3_598))?,
            lease
        ));
        assert!(!mailbox_covers_lease(UnixSeconds::new(899_000), ttl, lease));
        Ok(())
    }

    #[test]
    fn expiry_past_the_end_of_time_covers_every_lease() -> Result<(), TestError> {
        let forever = UnixSeconds::new(u64::MAX);
        assert_eq!(mailbox_expires_at(forever, one_hour()?), None);
        assert!(mailbox_covers_lease(
            forever,
            one_hour()?,
            Bucket::from_index(1)?
        ));
        Ok(())
    }

    #[test]
    fn a_lease_that_never_ends_is_never_covered() -> Result<(), TestError> {
        let last = Bucket::from_index(u64::MAX / 900)?;
        assert!(!mailbox_covers_lease(
            UnixSeconds::new(0),
            one_hour()?,
            last
        ));
        Ok(())
    }
}
