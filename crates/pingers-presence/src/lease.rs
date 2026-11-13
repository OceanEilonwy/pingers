// SPDX-License-Identifier: AGPL-3.0-only
//! Building leases: always four records, real or dummy, shuffled (design B.4.2, B.4.5).

use pingers_crypto::RevokeKey;
use pingers_proto::{Lease, MailboxId, MeetupHint, RecordRandomness};
use pingers_time::{Bucket, BucketCount, UnixSeconds};
use rand_core::CryptoRng;

use crate::stub::not_implemented;
use crate::{ActiveSpot, PresenceError, SpotRef};

/// A lease is live for its start bucket and the next three (design B.4.2, F.1).
pub const LEASE_BUCKETS: BucketCount = BucketCount::new(4);

/// A Spot the device is Inside, competing for one of the four slots.
pub struct SlotCandidate<'spot> {
    /// The Spot's keys.
    pub spot: &'spot ActiveSpot,
    /// The last time a peer was seen at this Spot (a match, knock or Meetup), if ever.
    pub last_peer_activity: Option<UnixSeconds>,
}

/// Picks at most four candidates: those with the most recent peer activity first; Spots never
/// active come last, in the order given (design B.4.2). The caller rotates the rest in on
/// later refreshes by moving them to the front of the list.
#[must_use]
pub fn choose_slots(mut candidates: Vec<SlotCandidate<'_>>) -> Vec<SlotCandidate<'_>> {
    // Stable sort: equal activity keeps the caller's order, which is how rotation works.
    candidates.sort_by_key(|candidate| core::cmp::Reverse(candidate.last_peer_activity));
    candidates.truncate(Lease::SLOTS);
    candidates
}

/// One real record to put in a lease.
pub struct RealSlot<'spot> {
    /// The Spot's keys.
    pub spot: &'spot ActiveSpot,
    /// The one-time knock mailbox the beacon names (from the mailbox pool).
    pub knock_mailbox: MailboxId,
    /// The Meetup this device is in at the Spot, if any (design B.5).
    pub meetup_hint: Option<MeetupHint>,
}

/// Everything a lease needs.
pub struct LeaseInputs<'spot> {
    /// The current bucket.
    pub start: Bucket,
    /// At most four real slots; the rest are dummies. Empty when suppressed or outside every
    /// Spot, which looks the same on the wire.
    pub real: Vec<RealSlot<'spot>>,
    /// A fresh revoke key; its commitment goes into the lease.
    pub revoke_key: &'spot RevokeKey,
}

/// What a slot holds, for the client's `lease` table (design C.2). Never leaves the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotContent {
    /// 96 random bytes.
    Dummy,
    /// A real record.
    Real {
        /// Which Spot.
        spot: SpotRef,
        /// The knock mailbox its beacon names.
        knock_mailbox: MailboxId,
    },
}

/// One slot of a built lease, as the client stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotRecord {
    /// The record's randomness `r` (also how own records are skipped when matching).
    pub randomness: RecordRandomness,
    /// Real or dummy.
    pub content: SlotContent,
}

/// A lease ready to upload, and what the client must remember about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltLease {
    /// The 424-byte lease, records already shuffled.
    pub lease: Lease,
    /// The slots, in the same order as `lease.records`.
    pub slots: [SlotRecord; Lease::SLOTS],
}

/// Builds a lease: one sealed record per real slot, random bytes for the rest, shuffled
/// with `rng`. Dummies are indistinguishable from real records without the Spot key.
///
/// # Errors
///
/// [`PresenceError::TooManySlots`] if more than four real slots are given;
/// [`PresenceError::Crypto`] if tagging or sealing fails.
pub fn build_lease(
    inputs: &LeaseInputs<'_>,
    rng: &mut impl CryptoRng,
) -> Result<BuiltLease, PresenceError> {
    if inputs.real.len() > Lease::SLOTS {
        return Err(PresenceError::TooManySlots);
    }
    not_implemented((inputs, rng))
}
