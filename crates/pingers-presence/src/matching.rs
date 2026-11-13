// SPDX-License-Identifier: AGPL-3.0-only
//! Trial matching the board against Spots the device is Inside (design B.4.3).

use pingers_proto::{BoardBeacon, BoardPage, Epoch, Record, RecordRandomness};
use pingers_time::Bucket;

use crate::stub::not_implemented;
use crate::{ActiveSpot, PresenceError, SpotRef};

/// Knock at most this many matched records per arrival (design B.4.4, F.3).
pub const MAX_KNOCKS_PER_ARRIVAL: usize = 3;

/// The randomness of this device's own live records, skipped when matching.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnRecords {
    randomness: Vec<RecordRandomness>,
}

impl OwnRecords {
    /// The set from the client's live leases.
    #[must_use]
    pub const fn new(randomness: Vec<RecordRandomness>) -> Self {
        Self { randomness }
    }

    /// Whether `randomness` is one of ours.
    #[must_use]
    pub fn contains(&self, randomness: &RecordRandomness) -> bool {
        self.randomness.contains(randomness)
    }
}

/// A record whose tag and seal verified under one of the active Spots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardMatch {
    /// The Spot it matched.
    pub spot: SpotRef,
    /// The Party epoch whose keys matched (current, or previous during the grace period).
    pub epoch: Epoch,
    /// The record's segment start bucket.
    pub start: Bucket,
    /// The record.
    pub record: Record,
    /// Its opened beacon: a knock mailbox and maybe a Meetup hint. It does not name the poster.
    pub beacon: BoardBeacon,
}

/// Trial-matches board pages against the active Spots.
///
/// Build one only from Spots that are `Inside` and not suppressed. A board read made for
/// cover, or while no Spot is Inside, is discarded unmatched: give it an empty matcher or do
/// not call it at all (design B.4.3).
pub struct TrialMatcher<'spots> {
    spots: &'spots [ActiveSpot],
}

impl<'spots> TrialMatcher<'spots> {
    /// A matcher over `spots` (at most 4 Spots, times 2 epochs during the grace period).
    #[must_use]
    pub const fn new(spots: &'spots [ActiveSpot]) -> Self {
        Self { spots }
    }

    /// Every record of `page` that matches an active Spot, skipping `own` records. A record
    /// whose tag matches but whose seal does not open is treated as no match.
    ///
    /// # Errors
    ///
    /// [`PresenceError::Proto`] if the page's segment counts do not add up to its records;
    /// [`PresenceError::Crypto`] for an internal crypto failure (not for a failed open).
    pub fn match_page(
        &self,
        page: &BoardPage,
        own: &OwnRecords,
    ) -> Result<Vec<BoardMatch>, PresenceError> {
        not_implemented((self.spots, page, own))
    }
}

/// Picks up to [`MAX_KNOCKS_PER_ARRIVAL`] matches to knock, preferring records that carry a
/// Meetup hint, then keeping board order (design B.4.4).
#[must_use]
pub fn knock_targets(matches: &[BoardMatch]) -> Vec<&BoardMatch> {
    let with_hint = matches
        .iter()
        .filter(|found| found.beacon.meetup_hint.is_some());
    let without_hint = matches
        .iter()
        .filter(|found| found.beacon.meetup_hint.is_none());
    with_hint
        .chain(without_hint)
        .take(MAX_KNOCKS_PER_ARRIVAL)
        .collect()
}
