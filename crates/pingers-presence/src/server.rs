// SPDX-License-Identifier: AGPL-3.0-only
//! The server half: lease validation, segment expiry, mailbox posting rules (design B.4.3,
//! B.4.9, D). The server matches nothing; these rules are about shape and time only.

use core::time::Duration;

use pingers_proto::{Lease, MailboxSpec, SizeClass};
use pingers_time::Bucket;

use crate::{LEASE_BUCKETS, PresenceError};

/// The longest a mailbox may live: 30 days (design B.4.9).
pub const MAX_MAILBOX_LIFETIME: Duration = Duration::from_hours(30 * 24);

/// Accepts a lease whose start bucket is the current bucket, or the previous one (a client
/// whose clock is behind within the allowed skew, design F.1).
///
/// # Errors
///
/// [`PresenceError::LeaseNotCurrent`] otherwise.
pub fn check_lease(lease: &Lease, now: Bucket) -> Result<(), PresenceError> {
    if lease.start == now || now.previous() == Some(lease.start) {
        Ok(())
    } else {
        Err(PresenceError::LeaseNotCurrent)
    }
}

/// Whether the segment starting at `start` has expired: it is dropped when bucket
/// `start + 4` starts (design B.4.3).
#[must_use]
pub fn segment_expired(start: Bucket, now: Bucket) -> bool {
    now.saturating_buckets_since(start) >= LEASE_BUCKETS
}

/// The start buckets of the live board at `now`: the four newest segments, oldest first.
/// Fewer near the epoch.
#[must_use]
pub fn live_segments(now: Bucket) -> Vec<Bucket> {
    let mut starts: Vec<Bucket> = (0..LEASE_BUCKETS.get())
        .filter_map(|back| now.checked_sub(pingers_time::BucketCount::new(back)))
        .collect();
    starts.reverse();
    starts
}

/// Checks a mailbox registration's lifetime.
///
/// # Errors
///
/// [`PresenceError::MailboxRule`] if it asks for more than [`MAX_MAILBOX_LIFETIME`].
pub fn check_mailbox_spec(spec: &MailboxSpec) -> Result<(), PresenceError> {
    if spec.time_to_live.duration() <= MAX_MAILBOX_LIFETIME {
        Ok(())
    } else {
        Err(PresenceError::MailboxRule)
    }
}

/// Checks a mailbox post's size: exactly 1, 4, 8 or 32 KiB (design D).
///
/// # Errors
///
/// [`PresenceError::MailboxRule`] for any other length.
pub fn check_mailbox_post(body: &[u8]) -> Result<(), PresenceError> {
    if SizeClass::MAILBOX_POST
        .iter()
        .any(|class| class.size() == body.len())
    {
        Ok(())
    } else {
        Err(PresenceError::MailboxRule)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_time::TimeError;

    use super::*;

    #[test]
    fn segment_expires_when_its_fifth_bucket_starts() -> Result<(), TimeError> {
        let start = Bucket::from_index(100)?;
        assert!(!segment_expired(start, Bucket::from_index(103)?));
        assert!(segment_expired(start, Bucket::from_index(104)?));
        Ok(())
    }

    #[test]
    fn live_board_is_the_four_newest_segments() -> Result<(), TimeError> {
        let starts: Vec<u64> = live_segments(Bucket::from_index(10)?)
            .iter()
            .map(|b| b.index())
            .collect();
        assert_eq!(starts, vec![7, 8, 9, 10]);
        Ok(())
    }

    #[test]
    fn mailbox_posts_must_be_a_mailbox_class() {
        assert!(check_mailbox_post(&[0; 1_024]).is_ok());
        assert!(check_mailbox_post(&[0; 256]).is_err());
    }
}
