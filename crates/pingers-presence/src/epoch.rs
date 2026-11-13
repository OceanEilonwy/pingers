// SPDX-License-Identifier: AGPL-3.0-only
//! Epoch lag: which Party epochs a reader matches with (design B.4.7).

use pingers_proto::Epoch;
use pingers_time::UnixSeconds;

use crate::LEASE_DURATION;

/// How the Party reached its current epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EpochTransition {
    /// The Commit removed at least one member. No grace: the removed member held the previous
    /// epoch's keys, so they are dropped at once and a lagging poster stays invisible until it
    /// syncs (fails safe).
    Removal,
    /// Any other Commit (an add, a key update, a Spot change).
    Other,
}

/// What the device knows about its Party's newest epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpochState {
    /// The newest epoch this device has processed.
    pub current: Epoch,
    /// When this device processed the Commit that started it (its own clock).
    pub entered_at: UnixSeconds,
    /// What that Commit did.
    pub transition: EpochTransition,
}

/// The epochs to derive Spot keys for when matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct MatchingEpochs {
    /// Always matched.
    pub current: Epoch,
    /// Also matched during the grace period, so records of members who have not yet
    /// processed the newest Commit still match.
    pub previous: Option<Epoch>,
}

/// For one lease horizon after an epoch change that was not a removal, match with the
/// previous epoch's keys too (design B.4.7). Posting always uses the current epoch.
pub fn matching_epochs(state: &EpochState, now: UnixSeconds) -> MatchingEpochs {
    let in_grace = state.transition == EpochTransition::Other
        && now.saturating_duration_since(state.entered_at) < LEASE_DURATION;
    MatchingEpochs {
        current: state.current,
        previous: in_grace
            .then(|| state.current.get().checked_sub(1).map(Epoch::new))
            .flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTERED: UnixSeconds = UnixSeconds::new(100_000);

    fn state(transition: EpochTransition) -> EpochState {
        EpochState {
            current: Epoch::new(7),
            entered_at: ENTERED,
            transition,
        }
    }

    #[test]
    fn previous_epoch_is_matched_for_one_lease_after_an_add() {
        let state = state(EpochTransition::Other);
        let last_second = UnixSeconds::new(100_000 + 3_599);
        assert_eq!(
            matching_epochs(&state, last_second),
            MatchingEpochs {
                current: Epoch::new(7),
                previous: Some(Epoch::new(6)),
            }
        );
        let lease_later = UnixSeconds::new(100_000 + 3_600);
        assert_eq!(matching_epochs(&state, lease_later).previous, None);
    }

    #[test]
    fn removal_has_no_grace() {
        let epochs = matching_epochs(&state(EpochTransition::Removal), ENTERED);
        assert_eq!(epochs.current, Epoch::new(7));
        assert_eq!(epochs.previous, None);
    }

    #[test]
    fn epoch_zero_has_no_previous() {
        let state = EpochState {
            current: Epoch::new(0),
            entered_at: ENTERED,
            transition: EpochTransition::Other,
        };
        assert_eq!(matching_epochs(&state, ENTERED).previous, None);
    }
}
