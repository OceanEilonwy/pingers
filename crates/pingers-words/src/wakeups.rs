// SPDX-License-Identifier: AGPL-3.0-only
//! Wake-ups, in words (canvas `WakeupsOn`, `Wakeups`, `WakeupsDelayed`, `WakeupsOffHome`,
//! `WakeupsHome`).

use pingers_screens::clock::{LocalClock, RelativeDay};
use pingers_screens::device::{PushService, WakeupsSummary};
use pingers_screens::snapshot::WakePushes;
use pingers_screens::wakeups::{
    BatteryUse, HomeWakeups, LastWake, Notifications, WakeFault, WakeTiming, Wakeups, WakeupsStatus,
};

use crate::Language;
use crate::common::time;

/// "Wake-ups".
pub const TITLE: &str = "Wake-ups";

/// The push app suggested to people without one.
const SUGGESTED_APP: &str = "ntfy";

/// The Wake-ups row on Settings and Privacy: "On · through ntfy", or "Off".
#[must_use]
pub fn summary(language: Language, view: &WakeupsSummary) -> String {
    let Language::English = language;
    match (view.pushes, &view.service) {
        (WakePushes::NoWakePushes, _) => "Off".into(),
        (WakePushes::On, PushService::PushApp(app)) => format!("On · through {}", app.as_str()),
        (WakePushes::On, PushService::Google) => "On · through Google".into(),
        (WakePushes::On, PushService::NotChosen) => "On · choose a push app".into(),
        (WakePushes::On, PushService::Nothing) => "On · nothing can wake Pingers".into(),
    }
}

/// A row with a title and a line under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowText {
    /// "Keep Pingers running".
    pub title: String,
    /// The line under it.
    pub detail: String,
}

/// The status card on the working screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusText {
    /// "Working", "Off".
    pub title: String,
    /// "Last wake-up arrived on time, yesterday 18:42".
    pub detail: String,
}

/// The orange fault card at the top of a fault screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaultText {
    /// "Nothing can wake Pingers".
    pub title: String,
    /// What it costs.
    pub body: String,
}

/// "Install a push app, such as **ntfy**. It learns…": the bold app name between two parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallStepText {
    /// "Install a push app, such as ".
    pub before: String,
    /// "ntfy".
    pub app: String,
    /// ". It learns that a push arrived, never what it says."
    pub after: String,
    /// "Get ntfy".
    pub action: String,
}

/// "Battery use: Unrestricted" and whether a change is offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatteryText {
    /// "Battery use: Unrestricted".
    pub title: String,
    /// "Now “Optimised”. …" or "Already unrestricted".
    pub detail: String,
    /// "Change", while it is optimised.
    pub action: Option<String>,
}

/// A Wake-ups screen, in words. Which parts are present follows the state: the status card
/// and Delivery section when off or working, the fault card and its fixes otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeupsText {
    /// "Wake-ups".
    pub title: String,
    /// The intro, when off or working.
    pub intro: Option<String>,
    /// The status card, when off or working.
    pub status: Option<StatusText>,
    /// The fault card, on a fault screen.
    pub fault: Option<FaultText>,
    /// "How to fix it", when nothing can wake Pingers.
    pub fix_heading: Option<String>,
    /// Step 1, install a push app.
    pub install: Option<InstallStepText>,
    /// Step 2, come back.
    pub come_back: Option<String>,
    /// "Try these", when Android delays wake-ups.
    pub try_heading: Option<String>,
    /// The battery row, when Android delays wake-ups.
    pub battery: Option<BatteryText>,
    /// The notifications row, when Android delays wake-ups.
    pub notifications: Option<RowText>,
    /// "Delivery" when off or working, "On this phone" on a fault screen.
    pub settings_heading: String,
    /// "No wake-up pushes" and its line for this screen.
    pub no_wake_pushes: RowText,
    /// "Push service" and what it is.
    pub push_service: RowText,
    /// "Keep Pingers running" and its line for this screen.
    pub keep_running: RowText,
    /// Who learns what, when off or working.
    pub footnote: Option<String>,
}

fn push_service_name(language: Language, service: &PushService) -> String {
    let Language::English = language;
    match service {
        PushService::PushApp(app) => app.as_str().to_owned(),
        PushService::NotChosen => "Choose one".into(),
        PushService::Google => "Google (Firebase)".into(),
        PushService::Nothing => "None installed".into(),
    }
}

fn last_wake(language: Language, last: Option<LastWake>, clock: LocalClock) -> String {
    let Language::English = language;
    let Some(last) = last else {
        return "No wake-up has arrived yet".into();
    };
    let at = time(language, clock.time_of(last.at));
    let when = match clock.day_of(last.at) {
        RelativeDay::Today => format!("today {at}"),
        RelativeDay::Yesterday => format!("yesterday {at}"),
        RelativeDay::DaysAgo(days) => format!("{days} days ago"),
    };
    match last.timing {
        WakeTiming::OnTime => format!("Last wake-up arrived on time, {when}"),
        WakeTiming::Late => format!("Last wake-up arrived late, {when}"),
    }
}

/// A Wake-ups screen, in words. `clock` places the last wake-up in local time.
#[must_use]
pub fn wakeups(language: Language, view: &Wakeups, clock: LocalClock) -> WakeupsText {
    let blank = blank(language, view);
    match view.status {
        WakeupsStatus::Off | WakeupsStatus::Working(_) => on_or_off(language, view, clock, blank),
        WakeupsStatus::NothingCanWake => nothing_can_wake(language, blank),
        WakeupsStatus::Delayed(count) => delayed(language, view, count, blank),
    }
}

/// The parts every Wake-ups screen has, as a fault screen words them.
fn blank(language: Language, view: &Wakeups) -> WakeupsText {
    let Language::English = language;
    let keep_running_battery = RowText {
        title: "Keep Pingers running".into(),
        detail: "Shows a lasting notification so Android doesn't put Pingers to sleep. Uses \
                 more battery."
            .into(),
    };
    let push_service = RowText {
        title: "Push service".into(),
        detail: push_service_name(language, &view.push_service),
    };
    WakeupsText {
        title: TITLE.into(),
        intro: None,
        status: None,
        fault: None,
        fix_heading: None,
        install: None,
        come_back: None,
        try_heading: None,
        battery: None,
        notifications: None,
        settings_heading: "On this phone".into(),
        no_wake_pushes: RowText {
            title: "No wake-up pushes".into(),
            detail: String::new(),
        },
        push_service,
        keep_running: keep_running_battery,
        footnote: None,
    }
}

/// Wake-ups off by choice, or working: the status card and Delivery.
fn on_or_off(
    language: Language,
    view: &Wakeups,
    clock: LocalClock,
    blank: WakeupsText,
) -> WakeupsText {
    let Language::English = language;
    WakeupsText {
        intro: Some(
            "When a friend arrives at a Spot you're in, their phone asks for yours to be \
                 woken, so the Meetup starts in seconds. These settings are for this phone and \
                 every Party on it."
                .into(),
        ),
        status: Some(match view.status {
            WakeupsStatus::Working(last) => StatusText {
                title: "Working".into(),
                detail: last_wake(language, last, clock),
            },
            WakeupsStatus::Off | WakeupsStatus::NothingCanWake | WakeupsStatus::Delayed(_) => {
                StatusText {
                    title: "Off".into(),
                    detail: "Meetups can take up to an hour to start.".into(),
                }
            }
        }),
        settings_heading: "Delivery".into(),
        no_wake_pushes: RowText {
            title: "No wake-up pushes".into(),
            detail: "Nobody, not even Apple or Google, sees when you're woken. Meetups can \
                         take up to an hour to start."
                .into(),
        },
        keep_running: RowText {
            title: "Keep Pingers running".into(),
            detail: "Shows a lasting notification so Android doesn't put Pingers to sleep. \
                         Friends nearby are noticed on time."
                .into(),
        },
        footnote: Some(
            "The push app learns that a push arrived, never what it says. Each Party's push \
                 relay sees when your phone is woken, never why or by whom; Party settings \
                 shows who runs it."
                .into(),
        ),
        ..blank
    }
}

/// Fault: nothing can wake Pingers; the two steps to fix it.
fn nothing_can_wake(language: Language, blank: WakeupsText) -> WakeupsText {
    let Language::English = language;
    WakeupsText {
        fault: Some(FaultText {
            title: "Nothing can wake Pingers".into(),
            body: "This phone has no push app and no Google push service. Pingers only \
                       notices friends when it wakes by itself, which can take up to an hour, \
                       and a short visit can be missed."
                .into(),
        }),
        fix_heading: Some("How to fix it".into()),
        install: Some(InstallStepText {
            before: "Install a push app, such as ".into(),
            app: SUGGESTED_APP.into(),
            after: ". It learns that a push arrived, never what it says.".into(),
            action: format!("Get {SUGGESTED_APP}"),
        }),
        come_back: Some(
            "Come back here. Pingers picks it up, or asks which one to use if you have more \
                 than one."
                .into(),
        ),
        no_wake_pushes: RowText {
            title: "No wake-up pushes".into(),
            detail: "Choose this if you'd rather nobody, not even a push app, sees when \
                         you're woken."
                .into(),
        },
        ..blank
    }
}

/// Fault: Android holds back urgent wake-ups; what to try.
fn delayed(language: Language, view: &Wakeups, count: usize, blank: WakeupsText) -> WakeupsText {
    let Language::English = language;
    WakeupsText {
        fault: Some(FaultText {
            title: "Android is holding back wake-ups".into(),
            body: match count {
                0 => "Android has been delaying urgent wake-ups until the phone was next \
                          awake anyway. Meetups can start late, or be missed if a visit is short."
                    .into(),
                1 => "This week Android delayed 1 urgent wake-up until the phone was next \
                          awake anyway. Meetups can start late, or be missed if a visit is short."
                    .into(),
                count => format!(
                    "This week Android delayed {count} urgent wake-ups until the phone was \
                         next awake anyway. Meetups can start late, or be missed if a visit is \
                         short."
                ),
            },
        }),
        try_heading: Some("Try these".into()),
        battery: Some(match view.battery {
            BatteryUse::Optimised => BatteryText {
                title: "Battery use: Unrestricted".into(),
                detail: "Now “Optimised”. Android limits apps it thinks you don't use.".into(),
                action: Some("Change".into()),
            },
            BatteryUse::Unrestricted => BatteryText {
                title: "Battery use: Unrestricted".into(),
                detail: "Already unrestricted.".into(),
                action: None,
            },
        }),
        notifications: Some(RowText {
            title: "Notifications".into(),
            detail: match view.notifications {
                Notifications::Allowed => "Allowed".into(),
                Notifications::Blocked => "Not allowed. Allow them in Android's settings.".into(),
            },
        }),
        push_service: RowText {
            title: "Push service".into(),
            detail: match &view.push_service {
                PushService::Google => "Google (Firebase). Install a push app such as ntfy \
                                            to use it instead."
                    .into(),
                other @ (PushService::PushApp(_)
                | PushService::NotChosen
                | PushService::Nothing) => push_service_name(language, other),
            },
        },
        no_wake_pushes: RowText {
            title: "No wake-up pushes".into(),
            detail: "Nobody, not even Google, sees when you're woken. Meetups can take up to \
                         an hour to start."
                .into(),
        },
        ..blank
    }
}

/// What Home says about wake-ups, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HomeWakeupsText {
    /// Nothing to say.
    Nothing,
    /// "Wake-ups off · Meetups can take up to an hour to start".
    OffByChoice(String),
    /// The orange banner.
    Fault {
        /// "Friends nearby may be noticed late".
        title: String,
        /// "Nothing can wake Pingers on this phone yet."
        detail: String,
        /// "Fix".
        fix: String,
    },
}

/// What Home says about wake-ups, in words.
#[must_use]
pub fn home_wakeups(language: Language, view: HomeWakeups) -> HomeWakeupsText {
    let Language::English = language;
    match view {
        HomeWakeups::Nothing => HomeWakeupsText::Nothing,
        HomeWakeups::OffByChoice => HomeWakeupsText::OffByChoice(
            "Wake-ups off · Meetups can take up to an hour to start".into(),
        ),
        HomeWakeups::Fault(fault) => HomeWakeupsText::Fault {
            title: "Friends nearby may be noticed late".into(),
            detail: match fault {
                WakeFault::NothingCanWake => "Nothing can wake Pingers on this phone yet.".into(),
                WakeFault::Delayed => "Android is holding back wake-ups.".into(),
            },
            fix: "Fix".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::ScreenError;
    use pingers_screens::clock::UtcOffset;
    use pingers_screens::device::AppLabel;
    use pingers_screens::snapshot::Toggle;
    use pingers_time::UnixSeconds;

    use super::*;

    const EN: Language = Language::English;
    /// 2026-11-14 10:00 UTC.
    const NOW: UnixSeconds = UnixSeconds::new(1_763_114_400);

    fn clock() -> LocalClock {
        LocalClock {
            now: NOW,
            offset: UtcOffset::UTC,
        }
    }

    fn view(status: WakeupsStatus, service: PushService) -> Wakeups {
        Wakeups {
            status,
            pushes: WakePushes::On,
            push_service: service,
            keep_running: Toggle::Off,
            battery: BatteryUse::Optimised,
            notifications: Notifications::Allowed,
        }
    }

    fn ntfy() -> Result<PushService, ScreenError> {
        Ok(PushService::PushApp(
            AppLabel::new("ntfy").ok_or(ScreenError::InvalidName)?,
        ))
    }

    #[test]
    fn working_wakeups_read_like_the_canvas() -> Result<(), ScreenError> {
        // Yesterday 18:42 UTC.
        let last = LastWake {
            at: UnixSeconds::new(NOW.get() - 15 * 3_600 - 18 * 60),
            timing: WakeTiming::OnTime,
        };
        let text = wakeups(
            EN,
            &view(WakeupsStatus::Working(Some(last)), ntfy()?),
            clock(),
        );
        let status = text.status.ok_or(ScreenError::InvalidName)?;
        assert_eq!(status.title, "Working");
        assert_eq!(
            status.detail,
            "Last wake-up arrived on time, yesterday 18:42"
        );
        assert_eq!(text.settings_heading, "Delivery");
        assert_eq!(text.push_service.detail, "ntfy");
        assert_eq!(
            text.no_wake_pushes.detail,
            "Nobody, not even Apple or Google, sees when you're woken. Meetups can take up to an \
             hour to start."
        );
        assert!(text.footnote.is_some());
        assert_eq!(text.fault, None);
        Ok(())
    }

    #[test]
    fn no_wake_up_yet_and_off_say_so() -> Result<(), ScreenError> {
        let none = wakeups(EN, &view(WakeupsStatus::Working(None), ntfy()?), clock());
        assert_eq!(
            none.status.map(|status| status.detail).as_deref(),
            Some("No wake-up has arrived yet")
        );
        let off = wakeups(EN, &view(WakeupsStatus::Off, ntfy()?), clock());
        assert_eq!(
            off.status.map(|status| status.title).as_deref(),
            Some("Off")
        );
        Ok(())
    }

    #[test]
    fn nothing_can_wake_explains_the_two_steps() {
        let text = wakeups(
            EN,
            &view(WakeupsStatus::NothingCanWake, PushService::Nothing),
            clock(),
        );
        assert_eq!(
            text.fault.as_ref().map(|fault| fault.title.as_str()),
            Some("Nothing can wake Pingers")
        );
        assert_eq!(text.fix_heading.as_deref(), Some("How to fix it"));
        let install = text
            .install
            .as_ref()
            .map(|step| (step.app.as_str(), step.action.as_str()));
        assert_eq!(install, Some(("ntfy", "Get ntfy")));
        assert_eq!(text.settings_heading, "On this phone");
        assert_eq!(text.push_service.detail, "None installed");
        assert_eq!(
            text.keep_running.detail,
            "Shows a lasting notification so Android doesn't put Pingers to sleep. Uses more \
             battery."
        );
        assert_eq!(text.status, None);
    }

    #[test]
    fn delayed_wakeups_count_the_week_and_offer_fixes() {
        let text = wakeups(
            EN,
            &view(WakeupsStatus::Delayed(6), PushService::Google),
            clock(),
        );
        assert_eq!(
            text.fault.as_ref().map(|fault| fault.body.as_str()),
            Some(
                "This week Android delayed 6 urgent wake-ups until the phone was next awake \
                 anyway. Meetups can start late, or be missed if a visit is short."
            )
        );
        assert_eq!(
            text.battery
                .as_ref()
                .and_then(|battery| battery.action.as_deref()),
            Some("Change")
        );
        assert_eq!(
            text.notifications.as_ref().map(|row| row.detail.as_str()),
            Some("Allowed")
        );
        assert_eq!(
            text.push_service.detail,
            "Google (Firebase). Install a push app such as ntfy to use it instead."
        );
    }

    #[test]
    fn home_says_off_quietly_and_faults_loudly() {
        assert_eq!(
            home_wakeups(EN, HomeWakeups::OffByChoice),
            HomeWakeupsText::OffByChoice(
                "Wake-ups off · Meetups can take up to an hour to start".into()
            )
        );
        assert_eq!(
            home_wakeups(EN, HomeWakeups::Fault(WakeFault::NothingCanWake)),
            HomeWakeupsText::Fault {
                title: "Friends nearby may be noticed late".into(),
                detail: "Nothing can wake Pingers on this phone yet.".into(),
                fix: "Fix".into()
            }
        );
        assert_eq!(
            home_wakeups(EN, HomeWakeups::Nothing),
            HomeWakeupsText::Nothing
        );
    }

    #[test]
    fn the_summary_names_what_wakes_pingers() -> Result<(), ScreenError> {
        let on = |service| WakeupsSummary {
            pushes: WakePushes::On,
            service,
        };
        assert_eq!(summary(EN, &on(ntfy()?)), "On · through ntfy");
        assert_eq!(summary(EN, &on(PushService::Google)), "On · through Google");
        assert_eq!(
            summary(
                EN,
                &WakeupsSummary {
                    pushes: WakePushes::NoWakePushes,
                    service: PushService::Google
                }
            ),
            "Off"
        );
        Ok(())
    }
}
