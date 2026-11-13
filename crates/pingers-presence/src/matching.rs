// SPDX-License-Identifier: AGPL-3.0-only
//! Trial matching the board against Spots the device is Inside (design B.4.3).

use pingers_crypto::{CryptoError, derive_seal_key, open_board_beacon, tag_matches};
use pingers_proto::{
    BoardBeacon, BoardPage, Epoch, GroupId, MailboxId, MeetupHint, ProtoError, Record,
    RecordRandomness,
};
use pingers_time::Bucket;

use crate::lease::segment_expired;
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
/// not call it at all (design B.4.3). During the grace period of design B.4.7 pass the
/// previous epoch's keys too (see [`crate::matching_epochs`]).
pub struct TrialMatcher<'spots> {
    spots: &'spots [ActiveSpot],
    now: Bucket,
}

impl<'spots> TrialMatcher<'spots> {
    /// A matcher over `spots` (at most 4 Spots, times 2 epochs during the grace period), for
    /// a board read in bucket `now` (the device's own clock).
    #[must_use]
    pub const fn new(spots: &'spots [ActiveSpot], now: Bucket) -> Self {
        Self { spots, now }
    }

    /// Every record of `page` that matches an active Spot, skipping `own` records, in board
    /// order. A record whose tag matches but whose seal does not open is treated as no match.
    ///
    /// Replays are dropped: records in a segment that has expired at `now`, or that starts
    /// after the next bucket (more than the allowed clock skew ahead), are not tried, and a
    /// record repeated within the page is reported once.
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
        let mut found: Vec<BoardMatch> = Vec::new();
        let mut rest = page.records.as_slice();
        for segment in &page.segments {
            let count = usize::try_from(segment.record_count).map_err(|_overflow| counts())?;
            let (records, after) = rest.split_at_checked(count).ok_or_else(counts)?;
            rest = after;
            if !self.segment_acceptable(segment.start) {
                continue;
            }
            for record in records {
                if own.contains(&record.randomness)
                    || found
                        .iter()
                        .any(|earlier| earlier.record.randomness == record.randomness)
                {
                    continue;
                }
                if let Some(matched) = self.match_record(segment.start, record)? {
                    found.push(matched);
                }
            }
        }
        if rest.is_empty() {
            Ok(found)
        } else {
            Err(counts())
        }
    }

    /// Not expired, and not further ahead than the next bucket.
    fn segment_acceptable(&self, start: Bucket) -> bool {
        let not_ahead = self.now.next().is_none_or(|next| start <= next);
        not_ahead && !segment_expired(start, self.now)
    }

    /// The first active Spot whose tag and seal both verify.
    fn match_record(
        &self,
        start: Bucket,
        record: &Record,
    ) -> Result<Option<BoardMatch>, PresenceError> {
        for spot in self.spots {
            if !tag_matches(spot.tag_key(), start, record)? {
                continue;
            }
            let seal_key = derive_seal_key(spot.spot_key(), start)?;
            match open_board_beacon(&seal_key, start, record) {
                Ok(beacon) => {
                    return Ok(Some(BoardMatch {
                        spot: spot.spot(),
                        epoch: spot.epoch(),
                        start,
                        record: *record,
                        beacon,
                    }));
                }
                // A forged tag over a sealed field that does not open, or a beacon with bad
                // reserved bytes, is no match; keep trying the other Spots.
                Err(CryptoError::OpenFailed | CryptoError::Encoding(_)) => {}
                Err(other) => return Err(other.into()),
            }
        }
        Ok(None)
    }
}

/// The page's segments do not cover its records exactly.
const fn counts() -> PresenceError {
    PresenceError::Proto(ProtoError::OutOfRange {
        field: "segment record counts",
    })
}

/// What a device must not knock again.
#[derive(Clone, Copy, Debug, Default)]
pub struct KnockFilter<'filter> {
    /// Knock mailboxes already knocked. A refreshed record names a new mailbox, so it is
    /// knocked again (design B.4.4 step 7).
    pub already_knocked: &'filter [MailboxId],
    /// Meetups this device is already in: records hinting at one of them are its fellow
    /// members' and need no knock.
    pub own_meetups: &'filter [GroupId],
}

/// Picks up to [`MAX_KNOCKS_PER_ARRIVAL`] matches to knock, preferring records that carry a
/// Meetup hint, then keeping board order (design B.4.4). Skips what `filter` excludes.
#[must_use]
pub fn knock_targets<'matches>(
    matches: &'matches [BoardMatch],
    filter: &KnockFilter<'_>,
) -> Vec<&'matches BoardMatch> {
    let own_hints: Vec<MeetupHint> = filter
        .own_meetups
        .iter()
        .map(MeetupHint::for_group)
        .collect();
    let wanted = |found: &&BoardMatch| {
        !filter.already_knocked.contains(&found.beacon.knock_mailbox)
            && found
                .beacon
                .meetup_hint
                .is_none_or(|hint| !own_hints.contains(&hint))
    };
    let with_hint = matches
        .iter()
        .filter(|found| found.beacon.meetup_hint.is_some())
        .filter(wanted);
    let without_hint = matches
        .iter()
        .filter(|found| found.beacon.meetup_hint.is_none())
        .filter(wanted);
    with_hint
        .chain(without_hint)
        .take(MAX_KNOCKS_PER_ARRIVAL)
        .collect()
}
