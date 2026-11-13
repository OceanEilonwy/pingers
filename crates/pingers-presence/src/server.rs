// SPDX-License-Identifier: AGPL-3.0-only
//! The server half: lease validation, segment expiry and hashing, mailbox posting rules
//! (design B.4.3, B.4.9, D). The server matches nothing; these rules are about shape and time
//! only.

use pingers_crypto::segment_hash;
use pingers_proto::{Lease, MailboxSpec, Record, SegmentHash, SizeClass, TimeToLive};
use pingers_time::{Bucket, BucketCount, UnixSeconds};

use crate::{LEASE_BUCKETS, MAX_MAILBOX_LIFETIME, PresenceError, mailbox_expires_at};

/// Accepts a lease whose start bucket is the current bucket, or the previous one (a client
/// whose clock is behind within the allowed skew, design F.1), and whose four records are
/// distinct. A repeated record could only be a copy, which readers would otherwise see twice.
///
/// # Errors
///
/// [`PresenceError::LeaseNotCurrent`] for another start bucket;
/// [`PresenceError::DuplicateRecord`] if two records share their randomness.
pub fn check_lease(lease: &Lease, now: Bucket) -> Result<(), PresenceError> {
    if lease.start != now && now.previous() != Some(lease.start) {
        return Err(PresenceError::LeaseNotCurrent);
    }
    let mut seen = Vec::with_capacity(Lease::SLOTS);
    for record in &lease.records {
        if seen.contains(&record.randomness) {
            return Err(PresenceError::DuplicateRecord);
        }
        seen.push(record.randomness);
    }
    Ok(())
}

/// Whether the segment starting at `start` has expired: it is dropped when bucket
/// `start + 4` starts (design B.4.3).
#[must_use]
pub fn segment_expired(start: Bucket, now: Bucket) -> bool {
    crate::lease::segment_expired(start, now)
}

/// The start buckets of the live board at `now`: the four newest segments, oldest first.
/// Fewer near the epoch.
#[must_use]
pub fn live_segments(now: Bucket) -> Vec<Bucket> {
    let mut starts: Vec<Bucket> = (0..LEASE_BUCKETS.get())
        .filter_map(|back| now.checked_sub(BucketCount::new(back)))
        .collect();
    starts.reverse();
    starts
}

/// Whether the segment starting at `start` can still grow at `now`. [`check_lease`] accepts
/// the previous bucket, so a segment takes appends during its own bucket and the next one,
/// and is closed from bucket `start + 2`.
#[must_use]
pub fn segment_closed(start: Bucket, now: Bucket) -> bool {
    now.saturating_buckets_since(start) >= BucketCount::new(2)
}

/// The hash a board page carries for a fully closed segment (design B.4.3, transparency
/// hook), or `None` while it can still grow. `records` are the segment's records in board
/// order; revoked records stay in, so every reader hashes the same bytes.
#[must_use]
pub fn closed_segment_hash(start: Bucket, now: Bucket, records: &[Record]) -> Option<SegmentHash> {
    segment_closed(start, now).then(|| segment_hash(records))
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

/// Checks that a mailbox registered at `registered_at` still exists at `now`; posts to and
/// reads from an expired mailbox are refused, and its messages dropped.
///
/// # Errors
///
/// [`PresenceError::MailboxRule`] once its lifetime has passed.
pub fn check_mailbox_live(
    registered_at: UnixSeconds,
    time_to_live: TimeToLive,
    now: UnixSeconds,
) -> Result<(), PresenceError> {
    match mailbox_expires_at(registered_at, time_to_live) {
        Some(expires) if now >= expires => Err(PresenceError::MailboxRule),
        Some(_) | None => Ok(()),
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

    use core::time::Duration;

    use pingers_proto::{RecordRandomness, RecordTag, RevokeCommitment, SealedBeacon};

    use super::*;
    use crate::test_support::TestError;

    fn record(fill: u8) -> Record {
        Record {
            randomness: RecordRandomness::from_bytes([fill; 16]),
            tag: RecordTag::from_bytes([fill; 16]),
            sealed: SealedBeacon::from_bytes([fill; 64]),
        }
    }

    fn lease(start: Bucket, fills: [u8; 4]) -> Lease {
        Lease {
            start,
            records: fills.map(record),
            revoke_commitment: RevokeCommitment::from_bytes([9; 32]),
        }
    }

    #[test]
    fn lease_for_this_or_the_previous_bucket_is_accepted() -> Result<(), TestError> {
        let now = Bucket::from_index(50)?;
        assert!(check_lease(&lease(now, [1, 2, 3, 4]), now).is_ok());
        assert!(check_lease(&lease(Bucket::from_index(49)?, [1, 2, 3, 4]), now).is_ok());
        Ok(())
    }

    #[test]
    fn lease_from_two_buckets_ago_or_the_future_is_refused() -> Result<(), TestError> {
        let now = Bucket::from_index(50)?;
        for start in [48, 51] {
            assert!(matches!(
                check_lease(&lease(Bucket::from_index(start)?, [1, 2, 3, 4]), now),
                Err(PresenceError::LeaseNotCurrent)
            ));
        }
        Ok(())
    }

    #[test]
    fn lease_repeating_a_record_is_refused() -> Result<(), TestError> {
        let now = Bucket::from_index(50)?;
        assert!(matches!(
            check_lease(&lease(now, [1, 2, 3, 1]), now),
            Err(PresenceError::DuplicateRecord)
        ));
        Ok(())
    }

    #[test]
    fn segment_expires_when_its_fifth_bucket_starts() -> Result<(), TestError> {
        let start = Bucket::from_index(100)?;
        assert!(!segment_expired(start, Bucket::from_index(103)?));
        assert!(segment_expired(start, Bucket::from_index(104)?));
        Ok(())
    }

    #[test]
    fn live_board_is_the_four_newest_segments() -> Result<(), TestError> {
        let starts: Vec<u64> = live_segments(Bucket::from_index(10)?)
            .iter()
            .map(|b| b.index())
            .collect();
        assert_eq!(starts, vec![7, 8, 9, 10]);
        Ok(())
    }

    #[test]
    fn segment_closes_after_the_bucket_late_leases_may_use() -> Result<(), TestError> {
        let start = Bucket::from_index(100)?;
        let records = [record(1), record(2)];
        assert_eq!(
            closed_segment_hash(start, Bucket::from_index(101)?, &records),
            None
        );
        assert_eq!(
            closed_segment_hash(start, Bucket::from_index(102)?, &records),
            Some(segment_hash(&records))
        );
        Ok(())
    }

    #[test]
    fn mailbox_lifetime_is_capped_at_thirty_days() -> Result<(), TestError> {
        let spec = |days: u64| -> Result<MailboxSpec, TestError> {
            Ok(MailboxSpec {
                mailbox_id: pingers_proto::MailboxId::from_bytes([1; 16]),
                time_to_live: TimeToLive::new(Duration::from_hours(days * 24))?,
                push: None,
            })
        };
        assert!(check_mailbox_spec(&spec(30)?).is_ok());
        assert!(check_mailbox_spec(&spec(31)?).is_err());
        Ok(())
    }

    #[test]
    fn expired_mailbox_refuses_posts() -> Result<(), TestError> {
        let registered = UnixSeconds::new(1_000);
        let one_hour = TimeToLive::new(Duration::from_hours(1))?;
        assert!(check_mailbox_live(registered, one_hour, UnixSeconds::new(4_599)).is_ok());
        assert!(check_mailbox_live(registered, one_hour, UnixSeconds::new(4_600)).is_err());
        Ok(())
    }

    #[test]
    fn mailbox_posts_must_be_a_mailbox_class() {
        assert!(check_mailbox_post(&[0; 1_024]).is_ok());
        assert!(check_mailbox_post(&[0; 4_096]).is_ok());
        assert!(check_mailbox_post(&[0; 256]).is_err());
        assert!(check_mailbox_post(&[0; 1_025]).is_err());
    }
}
