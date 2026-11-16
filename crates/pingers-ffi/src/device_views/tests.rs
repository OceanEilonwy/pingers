// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::sync::Arc;

use pingers_screens::fixtures;

use super::*;

fn snapshot() -> Result<Arc<Snapshot>, CoreError> {
    let snapshot = fixtures::snapshot().map_err(|_| CoreError::invalid("fixture"))?;
    Ok(Snapshot::new(Arc::new(snapshot)))
}

fn platform(push_service: PushServiceInfo) -> WakePlatform {
    WakePlatform {
        push_service,
        keep_running: false,
        battery: BatteryUse::Optimised,
        notifications_allowed: true,
        last_wake_at: None,
        last_wake_on_time: true,
        delayed_this_week: 6,
        downgraded_at: None,
    }
}

/// 2026-11-14 10:00 UTC.
const NOW: u64 = 1_763_114_400;

fn clock() -> LocalClock {
    LocalClock {
        now: NOW,
        utc_offset_seconds: 0,
    }
}

#[test]
fn setup_steps_chain_with_their_dots() {
    let first = setup_view(SetupStep::Location);
    assert_eq!(
        (first.next, first.dot, first.dots),
        (Some(SetupStep::Background), 0, 3)
    );
    assert_eq!(
        first.card_title.as_deref(),
        Some("Your location stays on this phone")
    );
    let last = setup_view(SetupStep::Notifications);
    assert_eq!((last.next, last.dot), (None, 2));
}

#[test]
fn the_setup_link_step_says_what_its_buttons_do() {
    let confirm = setup_link_view(SetupLinkState::Confirm {
        server: "pingers.example.org".into(),
    });
    assert_eq!(confirm.heading, "Set up Pingers on this phone");
    assert!(confirm.body.contains("pingers.example.org"));
    assert_eq!(
        (
            confirm.primary.as_deref(),
            confirm.action,
            confirm.not_now.as_deref()
        ),
        (
            Some("Set up"),
            Some(SetupLinkAction::SetUp),
            Some("Not now")
        )
    );
    assert!(!confirm.busy);

    let working = setup_link_view(SetupLinkState::Working {
        server: "pingers.example.org".into(),
    });
    assert!(working.busy);
    assert_eq!(
        (working.primary, working.action, working.not_now),
        (None, None, None)
    );

    let offline = setup_link_view(SetupLinkState::Unreachable {
        server: "pingers.example.org".into(),
    });
    assert_eq!(offline.action, Some(SetupLinkAction::TryAgain));
    let other = setup_link_view(SetupLinkState::OtherServer {
        app_server: "pingers.example.org".into(),
        link_server: "friends.example".into(),
    });
    assert!(other.body.contains("friends.example"));
    assert_eq!(other.action, Some(SetupLinkAction::Leave));
    assert_eq!(
        setup_link_view(SetupLinkState::SetUp).primary.as_deref(),
        Some("Continue")
    );
}

#[test]
fn a_setup_link_state_without_a_host_says_the_link_does_not_work() {
    assert_eq!(
        setup_link_view(SetupLinkState::Confirm { server: " ".into() }),
        setup_link_view(SetupLinkState::Broken)
    );
}

#[test]
fn the_hub_converts_the_phones_facts() -> Result<(), CoreError> {
    let view = snapshot()?.device_settings(&DevicePlatform {
        push_service: PushServiceInfo::PushApp {
            label: "ntfy".into(),
        },
        map_areas: 2,
        map_bytes: 246_000_000,
    });
    assert_eq!(view.wakeups_summary, "On · through ntfy");
    assert_eq!(view.maps_summary, "2 areas · 246 MB");
    let blank = snapshot()?.device_settings(&DevicePlatform {
        push_service: PushServiceInfo::PushApp { label: " ".into() },
        map_areas: 0,
        map_bytes: 0,
    });
    assert_eq!(blank.wakeups_summary, "On · choose a push app");
    Ok(())
}

#[test]
fn the_quiet_hours_editor_draws_the_bar_and_refuses_equal_times() -> Result<(), CoreError> {
    let eleven = TimeOfDay {
        hour: 23,
        minute: 0,
    };
    let seven = TimeOfDay { hour: 7, minute: 0 };
    let view = quiet_hours_editor(true, eleven, seven)?;
    assert_eq!(
        view.bar,
        [
            BarSpan {
                start_minute: 0,
                length_minutes: 420
            },
            BarSpan {
                start_minute: 1_380,
                length_minutes: 60
            }
        ]
    );
    assert!(view.can_save && view.on);
    assert_eq!(view.to_note.as_deref(), Some("next day"));
    let equal = quiet_hours_editor(true, eleven, eleven)?;
    assert!(!equal.can_save);
    assert!(equal.bar.is_empty());
    assert!(matches!(
        quiet_hours_editor(
            true,
            TimeOfDay {
                hour: 24,
                minute: 0
            },
            seven
        ),
        Err(CoreError::Invalid { .. })
    ));
    let stored = snapshot()?.quiet_hours_start();
    assert_eq!((stored.from, stored.to, stored.on), (eleven, seven, false));
    Ok(())
}

#[test]
fn the_zone_editor_starts_from_the_stored_times() -> Result<(), CoreError> {
    let snapshot = snapshot()?;
    let work = snapshot.zone_times_editor(ZoneId { row: 2 }, None)?;
    assert_eq!(work.title, "Work");
    assert!(!work.is_always && work.can_save);
    let eight = TimeOfDay { hour: 8, minute: 0 };
    let equal = ZoneTimes::Window {
        days: ZoneDays::Weekends,
        from: eight,
        to: eight,
    };
    let refused = snapshot.zone_times_editor(ZoneId { row: 2 }, Some(equal))?;
    assert!(!refused.can_save);
    assert_eq!(refused.times, equal);
    let home = snapshot.zone_times_editor(ZoneId { row: 1 }, None)?;
    assert!(home.is_always);
    assert!(matches!(
        snapshot.zone_times_editor(ZoneId { row: 9 }, None),
        Err(CoreError::NotFound { .. })
    ));
    Ok(())
}

#[test]
fn wakeups_pick_the_screen_for_the_fault() -> Result<(), CoreError> {
    let snapshot = snapshot()?;
    let nothing = snapshot.wakeups(&platform(PushServiceInfo::Nothing), clock())?;
    assert_eq!(nothing.kind, WakeupsKind::NothingCanWake);
    assert_eq!(nothing.install_action.as_deref(), Some("Get ntfy"));
    assert!(!nothing.push_service_choosable);
    let mut google = platform(PushServiceInfo::Google);
    google.downgraded_at = Some(NOW.saturating_sub(60));
    let delayed = snapshot.wakeups(&google, clock())?;
    assert_eq!(delayed.kind, WakeupsKind::Delayed);
    assert_eq!(delayed.battery_action.as_deref(), Some("Change"));
    assert!(matches!(
        snapshot.home_wakeups(&google, NOW),
        HomeWakeupsView::Fault {
            kind: WakeupsKind::Delayed,
            ..
        }
    ));
    let mut working = platform(PushServiceInfo::PushApp {
        label: "ntfy".into(),
    });
    working.last_wake_at = Some(NOW.saturating_sub(3_600));
    let on = snapshot.wakeups(&working, clock())?;
    assert_eq!(on.kind, WakeupsKind::Working);
    assert_eq!(
        on.status_detail.as_deref(),
        Some("Last wake-up arrived on time, today 09:00")
    );
    assert!(on.push_service_choosable && !on.no_wake_pushes_on);
    assert_eq!(
        snapshot.home_wakeups(&working, NOW),
        HomeWakeupsView::Nothing
    );
    Ok(())
}

#[test]
fn home_shows_the_most_serious_notice_it_knows() -> Result<(), CoreError> {
    let snapshot = snapshot()?;
    let raised = |notice| RaisedNotice { notice, at: NOW };
    let fixture = fixtures::snapshot().map_err(|_| CoreError::invalid("fixture"))?;
    let party = crate::ids::PartyId::from(fixtures::climbing_crew());
    let spot = fixture
        .parties
        .first()
        .and_then(|found| found.spots.get(1))
        .map(|spot| crate::ids::SpotId::from(spot.id))
        .ok_or_else(|| CoreError::invalid("spot"))?;
    let knock = snapshot
        .top_notice(
            vec![
                raised(Notice::PushDowngraded),
                raised(Notice::UnansweredKnock { party, spot }),
            ],
            NOW,
        )
        .ok_or_else(|| CoreError::invalid("no notice"))?;
    assert_eq!(knock.severity, NoticeSeverity::Info);
    assert_eq!(knock.title, "Someone from Climbing crew seemed to be here");
    let clock_wrong = snapshot
        .top_notice(
            vec![raised(Notice::BoardTooLarge), raised(Notice::ClockSkew)],
            NOW,
        )
        .ok_or_else(|| CoreError::invalid("no notice"))?;
    assert_eq!(clock_wrong.link, Some(NoticeLink::DateTime));
    assert_eq!(
        snapshot.top_notice(vec![raised(Notice::SameOperator)], NOW),
        None
    );
    Ok(())
}

#[test]
fn privacy_links_delivery_to_wakeups() -> Result<(), CoreError> {
    let text = snapshot()?.privacy_text(&PushServiceInfo::Google);
    assert_eq!(text.wakeups, "Wake-ups");
    assert_eq!(text.wakeups_summary, "On · through Google");
    assert_eq!(text.visibility.len(), 3);
    Ok(())
}
