// SPDX-License-Identifier: AGPL-3.0-only
//! Building leases: always four records, real or dummy, shuffled (design B.4.2, B.4.5).

use pingers_crypto::{
    BeaconHeader, RevokeKey, derive_seal_key, record_tag, revoke_commitment, seal_board_beacon,
};
use pingers_proto::{BoardBeacon, Lease, MailboxId, MeetupHint, Record, RecordRandomness};
use pingers_time::{Bucket, BucketCount, UnixSeconds};
use rand_core::CryptoRng;

use crate::random::shuffle;
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
/// Randomness is drawn in a fixed order, so `vectors/presence/lease.json` can list it: for
/// each slot in order, 16 bytes of record randomness if it is real or 96 bytes if it is a
/// dummy; then the Fisher–Yates draws of the shuffle (see the `random` module).
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
    let mut slots = Vec::with_capacity(Lease::SLOTS);
    for index in 0..Lease::SLOTS {
        let slot = match inputs.real.get(index) {
            Some(real) => real_record(inputs.start, real, rng)?,
            None => dummy_record(rng),
        };
        slots.push(slot);
    }
    shuffle(&mut slots, rng);
    let records: [Record; Lease::SLOTS] = slots
        .iter()
        .map(|slot| slot.record)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_length| PresenceError::Internal)?;
    let slots: [SlotRecord; Lease::SLOTS] = slots
        .iter()
        .map(|slot| slot.stored)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_length| PresenceError::Internal)?;
    Ok(BuiltLease {
        lease: Lease {
            start: inputs.start,
            records,
            revoke_commitment: revoke_commitment(inputs.revoke_key),
        },
        slots,
    })
}

/// A record and what the client stores about it, kept together through the shuffle.
struct BuiltSlot {
    record: Record,
    stored: SlotRecord,
}

/// `r`, `tag(b0, r)` and the sealed beacon (design B.4.1, B.4.2).
fn real_record(
    start: Bucket,
    real: &RealSlot<'_>,
    rng: &mut impl CryptoRng,
) -> Result<BuiltSlot, PresenceError> {
    let mut randomness = [0; RecordRandomness::LENGTH];
    rng.fill_bytes(&mut randomness);
    let randomness = RecordRandomness::from_bytes(randomness);
    let tag = record_tag(real.spot.tag_key(), start, &randomness)?;
    let seal_key = derive_seal_key(real.spot.spot_key(), start)?;
    let sealed = seal_board_beacon(
        &seal_key,
        &BeaconHeader {
            start,
            randomness: &randomness,
            tag: &tag,
        },
        &BoardBeacon {
            knock_mailbox: real.knock_mailbox,
            meetup_hint: real.meetup_hint,
        },
    )?;
    Ok(BuiltSlot {
        record: Record {
            randomness,
            tag,
            sealed,
        },
        stored: SlotRecord {
            randomness,
            content: SlotContent::Real {
                spot: real.spot.spot(),
                knock_mailbox: real.knock_mailbox,
            },
        },
    })
}

/// 96 random bytes (design B.4.5).
fn dummy_record(rng: &mut impl CryptoRng) -> BuiltSlot {
    let mut bytes = [0; Record::SIZE];
    rng.fill_bytes(&mut bytes);
    let record = Record::from_bytes(&bytes);
    BuiltSlot {
        record,
        stored: SlotRecord {
            randomness: record.randomness,
            content: SlotContent::Dummy,
        },
    }
}

/// Whether a segment that started at `start` has expired at `now`: it is dropped when bucket
/// `start + 4` starts (design B.4.3). Readers skip expired segments, which is what stops a
/// server replaying an old segment as if it were live.
pub(crate) fn segment_expired(start: Bucket, now: Bucket) -> bool {
    now.saturating_buckets_since(start) >= LEASE_BUCKETS
}
