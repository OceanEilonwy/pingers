// SPDX-License-Identifier: AGPL-3.0-only
//! Wake-ups (canvas `WakeupsOn`, `Wakeups`, `WakeupsDelayed`, `WakeupsOffHome`,
//! `WakeupsHome`): the one home for delivery settings, and what Home says about them.
//!
//! "No wake-up pushes" is a choice, not a fault: Home shows a quiet grey line for it. The
//! orange banner is only for real faults: nothing can wake Pingers (no push app and no Google
//! push service), or Android is holding back urgent wake-ups (the core's `PushDowngraded`
//! notice). The battery, notification and last-wake facts are the phone's; the shell passes
//! them in as a [`WakePlatform`].

use pingers_time::UnixSeconds;

use crate::device::PushService;
use crate::snapshot::{ClientSnapshot, Toggle, WakePushes};

/// Whether Android may limit Pingers in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BatteryUse {
    /// "Unrestricted": exempt from battery optimisation.
    Unrestricted,
    /// "Optimised", Android's default.
    Optimised,
}

/// Whether Pingers may post notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Notifications {
    /// Allowed.
    Allowed,
    /// Turned off for Pingers.
    Blocked,
}

/// Whether a wake-up came at the priority it was sent with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WakeTiming {
    /// On time.
    OnTime,
    /// Held back by Android until the phone was awake anyway.
    Late,
}

/// The last wake-up push that reached this phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastWake {
    /// When it arrived.
    pub at: UnixSeconds,
    /// Whether it came on time.
    pub timing: WakeTiming,
}

/// Whether the core reported that Android holds back urgent wake-ups (`Notice::PushDowngraded`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Downgrade {
    /// The core raised the notice.
    Reported,
    /// It did not.
    None,
}

/// What the Wake-ups screens need from the phone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakePlatform {
    /// What can wake Pingers.
    pub push_service: PushService,
    /// "Keep Pingers running".
    pub keep_running: Toggle,
    /// Battery optimisation.
    pub battery: BatteryUse,
    /// Notification permission.
    pub notifications: Notifications,
    /// The last wake-up push, if any arrived.
    pub last_wake: Option<LastWake>,
    /// How many urgent wake-ups Android delayed in the last seven days.
    pub delayed_this_week: usize,
    /// Whether the core reported downgraded pushes.
    pub downgrade: Downgrade,
}

/// What state wake-ups are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeupsStatus {
    /// The user chose "No wake-up pushes".
    Off,
    /// Working; the last wake-up, if one arrived.
    Working(Option<LastWake>),
    /// Fault: nothing can wake Pingers.
    NothingCanWake,
    /// Fault: Android holds back urgent wake-ups; how many it delayed this week.
    Delayed(usize),
}

/// A Wake-ups screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wakeups {
    /// Which state, and so which screen.
    pub status: WakeupsStatus,
    /// "No wake-up pushes".
    pub pushes: WakePushes,
    /// What wakes Pingers.
    pub push_service: PushService,
    /// "Keep Pingers running".
    pub keep_running: Toggle,
    /// Battery optimisation, for "Try these".
    pub battery: BatteryUse,
    /// Notifications, for "Try these".
    pub notifications: Notifications,
}

/// A real fault, for Home's orange banner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WakeFault {
    /// Nothing can wake Pingers.
    NothingCanWake,
    /// Android holds back urgent wake-ups.
    Delayed,
}

/// What Home shows about wake-ups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HomeWakeups {
    /// Nothing: wake-ups work.
    Nothing,
    /// The quiet grey line: wake-ups off by choice.
    OffByChoice,
    /// The orange banner.
    Fault(WakeFault),
}

/// Which state wake-ups are in.
#[must_use]
pub fn status(snapshot: &ClientSnapshot, platform: &WakePlatform) -> WakeupsStatus {
    match (snapshot.settings.wake_pushes, &platform.push_service) {
        (WakePushes::NoWakePushes, _) => WakeupsStatus::Off,
        (WakePushes::On, PushService::Nothing) => WakeupsStatus::NothingCanWake,
        (WakePushes::On, _) if platform.downgrade == Downgrade::Reported => {
            WakeupsStatus::Delayed(platform.delayed_this_week)
        }
        (WakePushes::On, _) => WakeupsStatus::Working(platform.last_wake),
    }
}

/// The Wake-ups screen for the current state.
#[must_use]
pub fn wakeups(snapshot: &ClientSnapshot, platform: &WakePlatform) -> Wakeups {
    Wakeups {
        status: status(snapshot, platform),
        pushes: snapshot.settings.wake_pushes,
        push_service: platform.push_service.clone(),
        keep_running: platform.keep_running,
        battery: platform.battery,
        notifications: platform.notifications,
    }
}

/// What Home shows about wake-ups.
#[must_use]
pub fn home_wakeups(snapshot: &ClientSnapshot, platform: &WakePlatform) -> HomeWakeups {
    match status(snapshot, platform) {
        WakeupsStatus::Off => HomeWakeups::OffByChoice,
        WakeupsStatus::Working(_) => HomeWakeups::Nothing,
        WakeupsStatus::NothingCanWake => HomeWakeups::Fault(WakeFault::NothingCanWake),
        WakeupsStatus::Delayed(_) => HomeWakeups::Fault(WakeFault::Delayed),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::device::AppLabel;
    use crate::fixtures::{self, FixtureError};

    fn working() -> Result<WakePlatform, FixtureError> {
        Ok(WakePlatform {
            push_service: PushService::PushApp(AppLabel::new("ntfy").ok_or(FixtureError::Missing)?),
            keep_running: Toggle::Off,
            battery: BatteryUse::Optimised,
            notifications: Notifications::Allowed,
            last_wake: Some(LastWake {
                at: UnixSeconds::new(1_000),
                timing: WakeTiming::OnTime,
            }),
            delayed_this_week: 0,
            downgrade: Downgrade::None,
        })
    }

    #[test]
    fn working_wakeups_show_the_last_one_and_nothing_on_home() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let platform = working()?;
        let view = wakeups(&snapshot, &platform);
        assert_eq!(view.status, WakeupsStatus::Working(platform.last_wake));
        assert_eq!(view.pushes, WakePushes::On);
        assert_eq!(home_wakeups(&snapshot, &platform), HomeWakeups::Nothing);
        Ok(())
    }

    #[test]
    fn choosing_no_wake_up_pushes_is_not_a_fault() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.settings.wake_pushes = WakePushes::NoWakePushes;
        let mut platform = working()?;
        // Off wins over every fault: nothing is expected to wake the phone.
        platform.push_service = PushService::Nothing;
        platform.downgrade = Downgrade::Reported;
        assert_eq!(wakeups(&snapshot, &platform).status, WakeupsStatus::Off);
        assert_eq!(home_wakeups(&snapshot, &platform), HomeWakeups::OffByChoice);
        Ok(())
    }

    #[test]
    fn no_push_service_is_a_fault() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let mut platform = working()?;
        platform.push_service = PushService::Nothing;
        platform.downgrade = Downgrade::Reported;
        assert_eq!(status(&snapshot, &platform), WakeupsStatus::NothingCanWake);
        assert_eq!(
            home_wakeups(&snapshot, &platform),
            HomeWakeups::Fault(WakeFault::NothingCanWake)
        );
        Ok(())
    }

    #[test]
    fn a_downgrade_notice_is_a_fault_with_the_weeks_count() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let mut platform = working()?;
        platform.push_service = PushService::Google;
        platform.downgrade = Downgrade::Reported;
        platform.delayed_this_week = 6;
        assert_eq!(status(&snapshot, &platform), WakeupsStatus::Delayed(6));
        assert_eq!(
            home_wakeups(&snapshot, &platform),
            HomeWakeups::Fault(WakeFault::Delayed)
        );
        // Delays without the core's notice are not a fault yet.
        platform.downgrade = Downgrade::None;
        assert!(matches!(
            status(&snapshot, &platform),
            WakeupsStatus::Working(_)
        ));
        Ok(())
    }
}
