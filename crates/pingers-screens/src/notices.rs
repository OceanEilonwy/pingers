// SPDX-License-Identifier: AGPL-3.0-only
//! Notices at the top of Home (canvas `Notices`), one at a time, most serious first.
//!
//! The core raises notices as events (pingers-client's `Notice`); the shell keeps the ones it
//! was told about, with when, and asks here which one to show. Each kind stays up for a while
//! after it was last raised, and the core raises it again on each wake while the cause lasts.
//! A notice that is about Wake-ups (`PushDowngraded`) is shown there instead, and those the
//! other screens already explain (the fallbacks) are not repeated here.

use core::time::Duration;

use pingers_client::Notice;
use pingers_proto::{PartyId, SpotId};
use pingers_time::UnixSeconds;

use crate::common::Name;
use crate::snapshot::{ClientSnapshot, RelayStatus};

/// A notice Home can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HomeNotice {
    /// The phone's clock is off; checking in stopped.
    ClockWrong,
    /// A privacy relay's key does not match its pin; anonymous traffic stopped.
    RelayKeyChanged,
    /// Meetup members were shown different check-ins.
    ServerMayHide,
    /// Do Not Disturb cannot be read, so "Pause during Focus" cannot work.
    DndUnreadable,
    /// Someone seemed to be at a Spot, but their phone did not answer.
    UnansweredKnock {
        /// The Party.
        party: PartyId,
        /// The Spot.
        spot: SpotId,
    },
    /// The board is too large to read quickly: checking takes longer.
    ServerBusy,
}

impl HomeNotice {
    /// The notice for a core notice, if Home shows it.
    #[must_use]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Notice is non-exhaustive; a notice this version does not know is not shown"
    )]
    pub const fn of(notice: &Notice) -> Option<Self> {
        Some(match *notice {
            Notice::ClockSkew => Self::ClockWrong,
            Notice::RelayKeyMismatch => Self::RelayKeyChanged,
            Notice::BoardEquivocation => Self::ServerMayHide,
            Notice::DndUnavailable => Self::DndUnreadable,
            Notice::UnansweredKnock { party, spot } => Self::UnansweredKnock { party, spot },
            Notice::BoardTooLarge => Self::ServerBusy,
            _ => return None,
        })
    }

    /// How serious it is: the more serious shows first.
    #[must_use]
    pub const fn severity(self) -> Severity {
        match self {
            Self::ClockWrong | Self::RelayKeyChanged => Severity::Stopped,
            Self::ServerMayHide | Self::DndUnreadable => Severity::Warning,
            Self::UnansweredKnock { .. } => Severity::Info,
            Self::ServerBusy => Severity::Slow,
        }
    }

    /// How long it stays up after it was last raised.
    #[must_use]
    pub const fn lasts(self) -> Duration {
        match self {
            Self::ClockWrong | Self::RelayKeyChanged | Self::UnansweredKnock { .. } => {
                Duration::from_hours(1)
            }
            Self::ServerBusy => Duration::from_hours(2),
            Self::ServerMayHide | Self::DndUnreadable => Duration::from_hours(24),
        }
    }

    /// The order among notices of the same severity, as the canvas lists them.
    const fn rank(self) -> u8 {
        match self {
            Self::ClockWrong => 0,
            Self::RelayKeyChanged => 1,
            Self::ServerMayHide => 2,
            Self::DndUnreadable => 3,
            Self::UnansweredKnock { .. } => 4,
            Self::ServerBusy => 5,
        }
    }
}

/// How serious a notice is, and so its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Something stopped: red.
    Stopped,
    /// Something may not work: orange.
    Warning,
    /// Worth knowing: white.
    Info,
    /// Things are slow: grey.
    Slow,
}

/// A notice the core raised, and when it was last raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaisedNotice {
    /// The notice.
    pub notice: HomeNotice,
    /// When.
    pub at: UnixSeconds,
}

/// Where a notice's link goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoticeAction {
    /// "Open date and time settings".
    DateTimeSettings,
    /// "Party settings", for this Party.
    PartySettings(PartyId),
    /// "Privacy settings".
    PrivacySettings,
}

/// The notice at the top of Home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeView {
    /// Which notice.
    pub notice: HomeNotice,
    /// Its colour.
    pub severity: Severity,
    /// The Party it is about, when only one could be.
    pub party: Option<Name>,
    /// The Spot it is about.
    pub spot: Option<Name>,
    /// Its link, if it has one.
    pub action: Option<NoticeAction>,
}

/// The notices still up at `now`, most serious first. A relay key mismatch the snapshot still
/// reports stays up however long ago it was raised.
#[must_use]
pub fn active(
    snapshot: &ClientSnapshot,
    raised: &[RaisedNotice],
    now: UnixSeconds,
) -> Vec<HomeNotice> {
    let mut up: Vec<HomeNotice> = raised
        .iter()
        .filter(|raised| {
            now.get().saturating_sub(raised.at.get()) < raised.notice.lasts().as_secs()
        })
        .map(|raised| raised.notice)
        .collect();
    if snapshot.relay == RelayStatus::KeyMismatch {
        up.push(HomeNotice::RelayKeyChanged);
    }
    up.sort_by_key(|notice| (notice.severity(), notice.rank()));
    up.dedup();
    up
}

/// The one notice Home shows now, if any.
#[must_use]
pub fn top_notice(
    snapshot: &ClientSnapshot,
    raised: &[RaisedNotice],
    now: UnixSeconds,
) -> Option<NoticeView> {
    let notice = active(snapshot, raised, now).into_iter().next()?;
    // Every Party is on the instance's server and relay, so a relay notice is about all of
    // them; it names the Party only when there is one.
    let (party, spot, action) = match notice {
        HomeNotice::ClockWrong => (None, None, Some(NoticeAction::DateTimeSettings)),
        HomeNotice::RelayKeyChanged => match snapshot.parties.as_slice() {
            [only] => (
                Some(only.name.clone()),
                None,
                Some(NoticeAction::PartySettings(only.id)),
            ),
            [first, ..] => (None, None, Some(NoticeAction::PartySettings(first.id))),
            [] => (None, None, None),
        },
        HomeNotice::DndUnreadable => (None, None, Some(NoticeAction::PrivacySettings)),
        HomeNotice::UnansweredKnock { party, spot } => {
            let party = snapshot.party(party).ok();
            (
                party.map(|party| party.name.clone()),
                party
                    .and_then(|party| party.spot(spot))
                    .map(|spot| spot.name.clone()),
                None,
            )
        }
        HomeNotice::ServerMayHide | HomeNotice::ServerBusy => (None, None, None),
    };
    Some(NoticeView {
        notice,
        severity: notice.severity(),
        party,
        spot,
        action,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    const NOW: UnixSeconds = UnixSeconds::new(100_000);

    fn raised(notice: HomeNotice, ago: u64) -> RaisedNotice {
        RaisedNotice {
            notice,
            at: UnixSeconds::new(NOW.get().saturating_sub(ago)),
        }
    }

    #[test]
    fn the_most_serious_notice_shows_first() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let all = [
            raised(HomeNotice::ServerBusy, 10),
            raised(HomeNotice::DndUnreadable, 10),
            raised(HomeNotice::ClockWrong, 10),
            raised(HomeNotice::ServerMayHide, 10),
        ];
        assert_eq!(
            active(&snapshot, &all, NOW),
            [
                HomeNotice::ClockWrong,
                HomeNotice::ServerMayHide,
                HomeNotice::DndUnreadable,
                HomeNotice::ServerBusy
            ]
        );
        let top = top_notice(&snapshot, &all, NOW).ok_or(FixtureError::Missing)?;
        assert_eq!(top.notice, HomeNotice::ClockWrong);
        assert_eq!(top.severity, Severity::Stopped);
        assert_eq!(top.action, Some(NoticeAction::DateTimeSettings));
        Ok(())
    }

    #[test]
    fn notices_go_once_they_have_lasted() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let hour = 3_600;
        let old = [
            raised(HomeNotice::ClockWrong, hour),
            raised(HomeNotice::ServerBusy, 2 * hour),
            raised(HomeNotice::DndUnreadable, 24 * hour),
        ];
        assert_eq!(top_notice(&snapshot, &old, NOW), None);
        let fresh = [raised(HomeNotice::DndUnreadable, 24 * hour - 1)];
        assert_eq!(active(&snapshot, &fresh, NOW), [HomeNotice::DndUnreadable]);
        Ok(())
    }

    #[test]
    fn a_relay_key_mismatch_stays_while_the_snapshot_reports_it() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.relay = RelayStatus::KeyMismatch;
        let view = top_notice(&snapshot, &[raised(HomeNotice::RelayKeyChanged, 10)], NOW)
            .ok_or(FixtureError::Missing)?;
        assert_eq!(view.notice, HomeNotice::RelayKeyChanged);
        // Two Parties share the relay, so no one Party is named; the link opens the first.
        assert_eq!(view.party, None);
        assert_eq!(
            view.action,
            Some(NoticeAction::PartySettings(fixtures::climbing_crew()))
        );
        snapshot.parties.truncate(1);
        let named = top_notice(&snapshot, &[], NOW).ok_or(FixtureError::Missing)?;
        assert_eq!(
            named.party.as_ref().map(Name::as_str),
            Some("Climbing crew")
        );
        Ok(())
    }

    #[test]
    fn an_unanswered_knock_names_its_party_and_spot() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let party = snapshot.parties.first().ok_or(FixtureError::Missing)?;
        let spot = party.spots.get(1).ok_or(FixtureError::Missing)?.id;
        let knock = HomeNotice::UnansweredKnock {
            party: party.id,
            spot,
        };
        let view = top_notice(&snapshot, &[raised(knock, 10)], NOW).ok_or(FixtureError::Missing)?;
        assert_eq!(view.party.as_ref().map(Name::as_str), Some("Climbing crew"));
        assert_eq!(
            view.spot.as_ref().map(Name::as_str),
            Some("The Wall climbing gym")
        );
        assert_eq!(view.severity, Severity::Info);
        assert_eq!(view.action, None);
        Ok(())
    }

    #[test]
    fn only_home_notices_are_kept() {
        assert_eq!(
            HomeNotice::of(&Notice::ClockSkew),
            Some(HomeNotice::ClockWrong)
        );
        assert_eq!(
            HomeNotice::of(&Notice::BoardTooLarge),
            Some(HomeNotice::ServerBusy)
        );
        assert_eq!(HomeNotice::of(&Notice::PushDowngraded), None);
        assert_eq!(
            HomeNotice::of(&Notice::FallbackActive(
                pingers_client::Fallback::SameOperator
            )),
            None
        );
    }
}
