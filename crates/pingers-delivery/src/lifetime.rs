// SPDX-License-Identifier: AGPL-3.0-only
//! How long a group may sit idle before it is deleted (decided 2026-11-13).
//!
//! The creator states the class when it creates the group, so the server learns whether a
//! group is a Meetup or a Party (design A.2). It already learned that a Meetup formed from the
//! group's size and timing; the class adds that a long-lived group is a Party's.

use core::time::Duration;

/// How long a Meetup group lives without reads or writes: two hours (design B.5, "End").
pub const MEETUP_IDLE_LIFETIME: Duration = Duration::from_hours(2);

/// How long a Party group lives without reads or writes: 180 days. Members renew their
/// subscriptions every 30 days and commit an Update at least every 7, so only a Party that
/// nobody has opened for half a year expires.
pub const PARTY_IDLE_LIFETIME: Duration = Duration::from_hours(180 * 24);

/// The lifetime class of a group, fixed at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GroupLifetime {
    /// A Meetup group: deleted after [`MEETUP_IDLE_LIFETIME`] idle.
    Meetup,
    /// A Party group: deleted after [`PARTY_IDLE_LIFETIME`] idle.
    Party,
}

impl GroupLifetime {
    /// How long a group of this class may be idle before it is deleted.
    #[must_use]
    pub const fn idle_lifetime(self) -> Duration {
        match self {
            Self::Meetup => MEETUP_IDLE_LIFETIME,
            Self::Party => PARTY_IDLE_LIFETIME,
        }
    }

    /// The stored value: 0 Meetup, 1 Party.
    #[must_use]
    pub const fn to_stored(self) -> u8 {
        match self {
            Self::Meetup => 0,
            Self::Party => 1,
        }
    }

    /// Parses a stored value.
    #[must_use]
    pub const fn from_stored(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Meetup),
            1 => Some(Self::Party),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meetups_live_two_hours_and_parties_half_a_year() {
        assert_eq!(GroupLifetime::Meetup.idle_lifetime().as_secs(), 7_200);
        assert_eq!(GroupLifetime::Party.idle_lifetime().as_secs(), 15_552_000);
    }

    #[test]
    fn stored_values_round_trip_and_unknown_ones_are_refused() {
        for lifetime in [GroupLifetime::Meetup, GroupLifetime::Party] {
            assert_eq!(
                GroupLifetime::from_stored(lifetime.to_stored()),
                Some(lifetime)
            );
        }
        assert_eq!(GroupLifetime::Meetup.to_stored(), 0);
        assert_eq!(GroupLifetime::Party.to_stored(), 1);
        assert_eq!(GroupLifetime::from_stored(2), None);
    }
}
