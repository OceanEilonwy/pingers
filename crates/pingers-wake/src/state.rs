// SPDX-License-Identifier: AGPL-3.0-only
//! What the pipeline remembers between wakes, and the snapshot pingers-client stores.

use pingers_geo::{
    ExclusionTrail, Fence, Fix, PresenceInput, PresenceSnapshot, PresenceState, PresenceTransition,
    SpotPresence, exit_buffer,
};
use pingers_presence::{LEASE_BUCKETS, LEASE_DURATION, OwnRecords, SpotRef};
use pingers_proto::{Epoch, Lease, MailboxId, PartyId, RecordRandomness};
use pingers_time::{Bucket, UnixSeconds};

use crate::ports::PartyInfo;

/// A real record in one of this device's live leases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealRecord {
    /// The Spot.
    pub spot: SpotRef,
    /// The Party epoch whose keys sealed it; a knock on it opens only under that epoch.
    pub epoch: Epoch,
    /// The knock mailbox its beacon names.
    pub knock_mailbox: MailboxId,
}

/// One slot of a live lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaseSlot {
    /// The record's randomness (own records are skipped when matching).
    pub randomness: RecordRandomness,
    /// The real record, or `None` for a dummy.
    pub real: Option<RealRecord>,
}

/// A lease the server may hold. A lease is remembered before it is uploaded, so a wake that
/// stops mid-upload still skips its records and answers knocks on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveLease {
    /// Its start bucket.
    pub start: Bucket,
    /// Its slots, in board order.
    pub slots: [LeaseSlot; Lease::SLOTS],
}

impl LiveLease {
    /// Live until bucket `start + 4` starts (design B.4.3).
    fn live_at(&self, now: Bucket) -> bool {
        now.saturating_buckets_since(self.start) < LEASE_BUCKETS
    }

    pub(crate) fn real_records(&self) -> impl Iterator<Item = (&LeaseSlot, RealRecord)> {
        self.slots
            .iter()
            .filter_map(|slot| slot.real.map(|real| (slot, real)))
    }
}

/// Whether a knock this device sent has been answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnockStatus {
    /// No Welcome yet.
    Waiting,
    /// A Welcome arrived.
    Answered,
}

/// A knock this device sent, kept for one lease: it is not sent again to the same mailbox,
/// and an unanswered one raises a notice when the lease is over (design B.4.4 step 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SentKnock {
    /// The Spot.
    pub spot: SpotRef,
    /// The epoch of the keys it matched under.
    pub epoch: Epoch,
    /// The mailbox it was posted to.
    pub knock_mailbox: MailboxId,
    /// The record it answers (for the tie-break, design B.4.4 step 6).
    pub matched: RecordRandomness,
    /// The start bucket of that record: the `b` of the Meetup's `PSK_meet(b)` (design B.5).
    pub matched_start: Bucket,
    /// Where the Welcome will come.
    pub reply_mailbox: MailboxId,
    /// When it was sent.
    pub sent_at: UnixSeconds,
    /// Whether a Welcome arrived.
    pub status: KnockStatus,
}

/// A knock this device decided on, named by the knocker's reply mailox, so the same post is not
/// decided again on the next read of the mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecidedKnock {
    /// The knocker's reply mailbox.
    pub reply_mailbox: MailboxId,
    /// When it was decided.
    pub at: UnixSeconds,
}

/// The newest epoch the wake has seen for a Party, to notice a Commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownEpoch {
    /// The Party.
    pub party: PartyId,
    /// The epoch.
    pub epoch: Epoch,
}

/// The last time a peer was seen at a Spot (a match or a knock), for slot choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerActivity {
    /// The Spot.
    pub spot: SpotRef,
    /// When.
    pub at: UnixSeconds,
}

/// A Spot's stored presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpotSnapshot {
    /// The Spot.
    pub spot: SpotRef,
    /// Its state machine.
    pub presence: PresenceSnapshot,
}

/// Everything pingers-client stores between processes. It holds no coordinates: the last fix
/// and the geofence anchor are not kept, so a restarted process takes a fresh fix.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WakeSnapshot {
    /// Per-Spot presence.
    pub spots: Vec<SpotSnapshot>,
    /// The exclusion trail.
    pub last_in_zone: Option<UnixSeconds>,
    /// Leases the server may hold.
    pub leases: Vec<LiveLease>,
    /// Knocks sent within the last lease.
    pub sent_knocks: Vec<SentKnock>,
    /// Knocks decided within the last lease.
    pub decided_knocks: Vec<DecidedKnock>,
    /// The last successful upload.
    pub last_upload: Option<UnixSeconds>,
    /// The newest epoch seen per Party.
    pub epochs: Vec<KnownEpoch>,
    /// Peer activity per Spot.
    pub peer_activity: Vec<PeerActivity>,
}

/// Whether an `Inside` Spot has been confirmed by this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Confirmation {
    /// Confirmed by a fix or OS event this process saw, or not `Inside` at all.
    Live,
    /// Restored as `Inside` and not yet confirmed. The stored state cannot say whether the
    /// device walked away while the process was gone, so the Spot gets no keys and no real
    /// record until a fix shows it within the exit buffer.
    Restored,
}

/// One Spot's state machine.
pub(crate) struct TrackedSpot {
    pub(crate) spot: SpotRef,
    pub(crate) presence: SpotPresence,
    pub(crate) confirmation: Confirmation,
}

impl TrackedSpot {
    /// `Inside` and confirmed by this process: the only Spots that get keys.
    pub(crate) fn inside(&self) -> bool {
        self.presence.state() == PresenceState::Inside && self.confirmation == Confirmation::Live
    }

    /// Feeds one input. Stale inputs are dropped, as pingers-geo asks.
    fn handle(&mut self, input: PresenceInput) -> Option<PresenceTransition> {
        match self.presence.handle(input) {
            Ok(pingers_geo::PresenceChange::Changed(transition)) => Some(transition),
            // The only error is `StaleInput`, and a stale input is dropped.
            Ok(pingers_geo::PresenceChange::Unchanged) | Err(_) => None,
        }
    }

    /// A fix within the exit buffer confirms a restored Spot.
    fn confirm_with(&mut self, fix: &Fix) {
        let fence = self.presence.fence();
        let buffer = exit_buffer(fence.characteristic_radius());
        if fence.distance_outside(fix.coordinate) <= buffer {
            self.confirmation = Confirmation::Live;
        }
    }
}

/// A Spot that changed state in this wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpotTransition {
    /// The Spot.
    pub spot: SpotRef,
    /// The change.
    pub transition: PresenceTransition,
}

/// The OS geofences last registered, to tell when to choose again.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GeofenceAnchor {
    pub(crate) horizon: Option<pingers_geo::Circle>,
    pub(crate) spots: Vec<(SpotRef, Fence)>,
}

/// The pipeline's memory. Build it with [`WakeState::new`] or [`WakeState::restore`], run wakes
/// with [`WakeState::run`], and store [`WakeState::snapshot`] after each one.
pub struct WakeState {
    pub(crate) spots: Vec<TrackedSpot>,
    restored: Vec<SpotSnapshot>,
    pub(crate) trail: ExclusionTrail,
    pub(crate) last_fix: Option<Fix>,
    pub(crate) leases: Vec<LiveLease>,
    pub(crate) sent_knocks: Vec<SentKnock>,
    pub(crate) decided_knocks: Vec<DecidedKnock>,
    pub(crate) last_upload: Option<UnixSeconds>,
    pub(crate) epochs: Vec<KnownEpoch>,
    pub(crate) peer_activity: Vec<PeerActivity>,
    pub(crate) geofences: Option<GeofenceAnchor>,
}

impl Default for WakeState {
    fn default() -> Self {
        Self::new()
    }
}

impl WakeState {
    /// A fresh install: no Spots tracked yet, nothing uploaded.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            spots: Vec::new(),
            restored: Vec::new(),
            trail: ExclusionTrail::new(),
            last_fix: None,
            leases: Vec::new(),
            sent_knocks: Vec::new(),
            decided_knocks: Vec::new(),
            last_upload: None,
            epochs: Vec::new(),
            peer_activity: Vec::new(),
            geofences: None,
        }
    }

    /// Restores a stored snapshot after a restart. Spots restored as `Inside` get no keys
    /// until a fix confirms them (see `CONTRACT.md`, "Restart").
    #[must_use]
    pub fn restore(snapshot: WakeSnapshot) -> Self {
        Self {
            restored: snapshot.spots,
            trail: ExclusionTrail::restore(snapshot.last_in_zone),
            leases: snapshot.leases,
            sent_knocks: snapshot.sent_knocks,
            decided_knocks: snapshot.decided_knocks,
            last_upload: snapshot.last_upload,
            epochs: snapshot.epochs,
            peer_activity: snapshot.peer_activity,
            ..Self::new()
        }
    }

    /// What to store. Spots restored but not yet seen in a wake are kept as they were.
    #[must_use]
    pub fn snapshot(&self) -> WakeSnapshot {
        let mut spots: Vec<SpotSnapshot> = self
            .spots
            .iter()
            .map(|tracked| SpotSnapshot {
                spot: tracked.spot,
                presence: tracked.presence.snapshot(),
            })
            .collect();
        spots.extend(self.restored.iter().copied());
        WakeSnapshot {
            spots,
            last_in_zone: self.trail.last_in_zone(),
            leases: self.leases.clone(),
            sent_knocks: self.sent_knocks.clone(),
            decided_knocks: self.decided_knocks.clone(),
            last_upload: self.last_upload,
            epochs: self.epochs.clone(),
            peer_activity: self.peer_activity.clone(),
        }
    }

    /// Follows the Parties' Spots: new Spots start `Outside` (or from their restored
    /// snapshot), edited fences restart from the stored state, deleted Spots are dropped.
    pub(crate) fn reconcile(&mut self, parties: &[PartyInfo], now: UnixSeconds) {
        let mut next = Vec::new();
        for party in parties {
            for info in &party.spots {
                let spot = SpotRef {
                    party: party.party,
                    spot: info.spot,
                };
                next.push(self.track(spot, &info.fence, now));
            }
        }
        self.spots = next;
    }

    fn track(&mut self, spot: SpotRef, fence: &Fence, now: UnixSeconds) -> TrackedSpot {
        if let Some(index) = self.spots.iter().position(|tracked| tracked.spot == spot) {
            let tracked = self.spots.swap_remove(index);
            if tracked.presence.fence() == fence {
                return tracked;
            }
            return TrackedSpot {
                spot,
                presence: SpotPresence::restore(fence.clone(), tracked.presence.snapshot()),
                confirmation: Confirmation::Restored,
            };
        }
        if let Some(index) = self.restored.iter().position(|stored| stored.spot == spot) {
            let stored = self.restored.swap_remove(index);
            return TrackedSpot {
                spot,
                presence: SpotPresence::restore(fence.clone(), stored.presence),
                confirmation: Confirmation::Restored,
            };
        }
        TrackedSpot {
            spot,
            presence: SpotPresence::new(fence.clone(), now),
            confirmation: Confirmation::Live,
        }
    }

    /// Forgets leases, knocks and decisions older than a lease.
    pub(crate) fn prune(&mut self, now: UnixSeconds) {
        let bucket = now.bucket();
        self.leases.retain(|lease| lease.live_at(bucket));
        // A knock is decided after its record's lease started, so one lease after the decision
        // that lease has expired and its mailbox is no longer read: where exactly the cut-off
        // falls (`<` or `<=`) cannot be observed.
        self.decided_knocks
            .retain(|decided| now.saturating_duration_since(decided.at) < LEASE_DURATION);
    }

    /// Feeds one input to every Spot, or to one Spot, and collects the changes.
    pub(crate) fn feed(
        &mut self,
        target: Option<SpotRef>,
        input: PresenceInput,
    ) -> Vec<SpotTransition> {
        let mut changes = Vec::new();
        for tracked in &mut self.spots {
            if target.is_some_and(|spot| spot != tracked.spot) {
                continue;
            }
            if let PresenceInput::Fix(fix) = input {
                tracked.confirm_with(&fix);
            }
            if let Some(transition) = tracked.handle(input) {
                // Every transition enters or leaves Inside on evidence this process saw, and
                // confirmation only matters while Inside.
                tracked.confirmation = Confirmation::Live;
                changes.push(SpotTransition {
                    spot: tracked.spot,
                    transition,
                });
            }
        }
        changes
    }

    /// The tracked Spot, if any.
    pub(crate) fn spot(&self, spot: SpotRef) -> Option<&TrackedSpot> {
        self.spots.iter().find(|tracked| tracked.spot == spot)
    }

    /// Every record of every live lease.
    pub(crate) fn own_records(&self) -> OwnRecords {
        OwnRecords::new(
            self.leases
                .iter()
                .flat_map(|lease| lease.slots.iter().map(|slot| slot.randomness))
                .collect(),
        )
    }

    /// The newest lease's real record for `spot`, for the tie-break field of a knock.
    pub(crate) fn own_record_for(&self, spot: SpotRef) -> Option<RecordRandomness> {
        self.leases.last().and_then(|lease| {
            lease
                .real_records()
                .find(|(_slot, real)| real.spot == spot)
                .map(|(slot, _real)| slot.randomness)
        })
    }

    /// The last peer activity at `spot`.
    pub(crate) fn peer_activity_at(&self, spot: SpotRef) -> Option<UnixSeconds> {
        self.peer_activity
            .iter()
            .find(|activity| activity.spot == spot)
            .map(|activity| activity.at)
    }

    /// Records peer activity at `spot`.
    pub(crate) fn saw_peer(&mut self, spot: SpotRef, at: UnixSeconds) {
        match self
            .peer_activity
            .iter_mut()
            .find(|activity| activity.spot == spot)
        {
            Some(activity) => activity.at = activity.at.max(at),
            None => self.peer_activity.push(PeerActivity { spot, at }),
        }
    }

    /// Whether the geofences were registered for exactly these Spots.
    pub(crate) fn geofences_cover(&self, spots: &[(SpotRef, Fence)]) -> bool {
        self.geofences
            .as_ref()
            .is_some_and(|anchor| anchor.spots == spots)
    }
}
