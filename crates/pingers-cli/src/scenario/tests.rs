// SPDX-License-Identifier: AGPL-3.0-only
//! Runner tests: the bundled scenarios replay, and a toy core shows that the runner pumps
//! fixes, fires due wakes and checks expectations.

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::future::{self, Future};
use core::time::Duration;

use pingers_proto::GroupId;
use pingers_testkit::{FixQuality, LocationMode, RecordedEvent};

use super::format::{Transition, parse};
use super::runner::{Device, DeviceInput, Outcome, ScenarioDriver, World};
use super::{BUILTINS, Checks, DryRunDriver, builtin, run};

/// A stand-in for a core, just enough to exercise the runner: a geofence entry asks for a
/// fix; a fix inside the Spot switches to Dwell and schedules a wake `wake_after` later; a
/// scheduled wake asks for another fix; a push starts a Meetup.
struct ToyCore {
    wake_after: Duration,
    inputs: Vec<(String, DeviceInput)>,
}

impl ToyCore {
    fn new(wake_after: Duration) -> Self {
        Self {
            wake_after,
            inputs: Vec::new(),
        }
    }
}

impl ScenarioDriver for ToyCore {
    fn start(&mut self, _world: &World) -> impl Future<Output = anyhow::Result<()>> {
        future::ready(Ok(()))
    }

    fn deliver(
        &mut self,
        device: &Device,
        input: DeviceInput,
    ) -> impl Future<Output = anyhow::Result<()>> {
        let platform = &device.platform;
        match &input {
            DeviceInput::Geofence {
                transition: Transition::Enter,
                ..
            }
            | DeviceInput::ScheduledWake => {
                platform.request_fix(FixQuality::Balanced, Duration::from_secs(30));
            }
            DeviceInput::Fix(_) => {
                platform.set_location_mode(LocationMode::Dwell);
                platform.schedule_wake(self.wake_after);
            }
            DeviceInput::Push => device.events.record(RecordedEvent::MeetupStarted {
                meetup: GroupId::from_bytes([1; 32]),
            }),
            DeviceInput::Geofence {
                transition: Transition::Exit,
                ..
            }
            | DeviceInput::FocusChanged => {}
        }
        self.inputs.push((device.name.clone(), input));
        future::ready(Ok(()))
    }

    fn settle(&mut self, _world: &World) -> impl Future<Output = anyhow::Result<()>> {
        future::ready(Ok(()))
    }
}

/// A core that asks for a new fix every time it gets one.
struct GreedyCore;

impl ScenarioDriver for GreedyCore {
    fn start(&mut self, _world: &World) -> impl Future<Output = anyhow::Result<()>> {
        future::ready(Ok(()))
    }

    fn deliver(
        &mut self,
        device: &Device,
        _input: DeviceInput,
    ) -> impl Future<Output = anyhow::Result<()>> {
        device
            .platform
            .request_fix(FixQuality::High, Duration::from_secs(1));
        future::ready(Ok(()))
    }

    fn settle(&mut self, _world: &World) -> impl Future<Output = anyhow::Result<()>> {
        future::ready(Ok(()))
    }
}

fn block_on<F: Future>(future: F) -> anyhow::Result<F::Output> {
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    Ok(runtime.block_on(future))
}

const ONE_DEVICE: &str = r#"
    name = "toy"
    [party]
    members = ["alice"]
    [[party.spots]]
    name = "anchor"
    latitude = 51.5
    longitude = -0.12
    radius_m = 150
"#;

#[test]
fn every_bundled_scenario_parses_and_replays_dry() -> anyhow::Result<()> {
    for scenario in BUILTINS {
        let parsed = parse(scenario.text)?;
        assert!(
            parsed.name.starts_with(scenario.name),
            "{} is named {}",
            scenario.name,
            parsed.name
        );
        let mut driver = DryRunDriver::default();
        let report = block_on(run(&parsed, &mut driver, Checks::Skip))??;
        assert!(!report.failed());
        assert_eq!(driver.devices, parsed.devices.len());
        assert!(!driver.delivered.is_empty());
        let skipped = report
            .steps
            .iter()
            .filter(|step| step.outcome == Outcome::Skipped)
            .count();
        assert!(skipped > 0, "{} checks nothing", scenario.name);
        assert!(
            report
                .to_string()
                .ends_with(&format!("replayed; {skipped} expectations skipped"))
        );
    }
    Ok(())
}

#[test]
fn builtins_are_found_by_name_in_any_case() {
    assert_eq!(builtin("G3").map(|found| found.name), Some("g3"));
    assert_eq!(builtin("g8"), None);
}

#[test]
fn the_dry_run_delivers_inputs_and_settles_after_each() -> anyhow::Result<()> {
    let scenario = parse(&format!(
        "{ONE_DEVICE}
        [[steps]]
        at = 0
        device = \"alice\"
        geofence = {{ region = \"anchor\", transition = \"enter\" }}
        [[steps]]
        at = 1
        device = \"alice\"
        focus = \"on\"
        [[steps]]
        at = 2
        device = \"alice\"
        wake = \"push\""
    ))?;
    let mut driver = DryRunDriver::default();
    block_on(run(&scenario, &mut driver, Checks::Skip))??;
    let inputs: Vec<&DeviceInput> = driver
        .delivered
        .iter()
        .map(|(_device, input)| input)
        .collect();
    assert_eq!(
        inputs,
        [
            &DeviceInput::Geofence {
                region: String::from("anchor"),
                transition: Transition::Enter
            },
            &DeviceInput::FocusChanged,
            &DeviceInput::Push
        ]
    );
    assert_eq!(driver.settles, 3);
    Ok(())
}

#[test]
fn g3_passes_against_a_core_that_behaves() -> anyhow::Result<()> {
    let scenario = parse(BUILTINS[0].text)?;
    let mut core = ToyCore::new(Duration::from_mins(10));
    let report = block_on(run(&scenario, &mut core, Checks::Evaluate))??;
    assert!(!report.failed(), "{report}");
    // The geofence asked for a fix and the runner delivered the scripted answer.
    assert!(matches!(
        core.inputs.get(1),
        Some((device, DeviceInput::Fix(_))) if device == "alice"
    ));
    Ok(())
}

#[test]
fn a_failed_expectation_is_reported_not_raised() -> anyhow::Result<()> {
    let scenario = parse(&format!(
        "{ONE_DEVICE}
        [[steps]]
        at = 0
        device = \"alice\"
        expect = {{ location_mode = \"dwell\" }}
        [[steps]]
        at = 0
        device = \"alice\"
        expect = {{ event = \"meetup_started\" }}
        [[steps]]
        at = 0
        device = \"alice\"
        expect = {{ wake_within_s = 60 }}"
    ))?;
    let report = block_on(run(
        &scenario,
        &mut ToyCore::new(Duration::ZERO),
        Checks::Evaluate,
    ))??;
    assert!(report.failed());
    let outcomes: Vec<&Outcome> = report.steps.iter().map(|step| &step.outcome).collect();
    assert_eq!(
        outcomes,
        [
            &Outcome::Failed(String::from("location mode is Idle, not Dwell")),
            &Outcome::Failed(String::from("expected a MeetupStarted event; recorded []")),
            &Outcome::Failed(String::from("no wake is pending")),
        ]
    );
    assert!(report.to_string().ends_with("FAILED"));
    Ok(())
}

#[test]
fn every_expectation_kind_can_pass() -> anyhow::Result<()> {
    let scenario = parse(&format!(
        "{ONE_DEVICE}
        [[steps]]
        at = 0
        device = \"alice\"
        location = {{ spot = \"anchor\", position = \"inside\" }}
        [[steps]]
        at = 0
        device = \"alice\"
        wake = \"push\"
        [[steps]]
        at = 1
        device = \"alice\"
        expect = {{ wake_pending = true }}
        [[steps]]
        at = 1
        device = \"alice\"
        expect = {{ wake_within_s = 599 }}
        [[steps]]
        at = 1
        device = \"alice\"
        expect = {{ geofences = 0 }}
        [[steps]]
        at = 1
        device = \"alice\"
        expect = {{ fix_requests = 0 }}
        [[steps]]
        at = 1
        device = \"alice\"
        expect = {{ no_event = \"meetup_ended\" }}
        [[steps]]
        at = 1
        device = \"alice\"
        expect = {{ event = \"meetup_started\" }}"
    ))?;
    let report = block_on(run(
        &scenario,
        &mut ToyCore::new(Duration::from_mins(10)),
        Checks::Evaluate,
    ))??;
    assert!(!report.failed(), "{report}");
    assert!(report.to_string().ends_with("passed"));
    Ok(())
}

#[test]
fn counting_expectations_fail_with_the_actual_count() -> anyhow::Result<()> {
    let scenario = parse(&format!(
        "{ONE_DEVICE}
        [[steps]]
        at = 0
        device = \"alice\"
        expect = {{ geofences = 1 }}
        [[steps]]
        at = 0
        device = \"alice\"
        expect = {{ fix_requests = 1 }}
        [[steps]]
        at = 0
        device = \"alice\"
        expect = {{ wake_pending = true }}
        [[steps]]
        at = 0
        device = \"alice\"
        expect = {{ no_event = \"meetup_started\" }}
        [[steps]]
        at = 0
        device = \"alice\"
        expect = {{ notice = \"clock_skew\" }}"
    ))?;
    let report = block_on(run(
        &scenario,
        &mut ToyCore::new(Duration::ZERO),
        Checks::Evaluate,
    ))??;
    let reasons: Vec<String> = report
        .steps
        .iter()
        .map(|step| match &step.outcome {
            Outcome::Failed(why) => why.clone(),
            other @ (Outcome::Passed | Outcome::Delivered | Outcome::Skipped) => {
                format!("{other:?}")
            }
        })
        .collect();
    assert_eq!(
        reasons,
        [
            "0 geofences registered",
            "0 fixes requested",
            "wake pending is false",
            "Passed",
            "expected a ClockSkew notice; recorded []",
        ]
    );
    Ok(())
}

#[test]
fn a_wake_that_falls_due_between_steps_is_delivered_on_time() -> anyhow::Result<()> {
    let scenario = parse(&format!(
        "{ONE_DEVICE}
        [[steps]]
        at = 0
        device = \"alice\"
        fix = {{ spot = \"anchor\", position = \"inside\" }}
        [[steps]]
        at = 0
        device = \"alice\"
        location = {{ spot = \"anchor\", position = \"inside\" }}
        [[steps]]
        at = 250
        device = \"alice\"
        expect = {{ fix_requests = 2 }}"
    ))?;
    let mut core = ToyCore::new(Duration::from_mins(1));
    let report = block_on(run(&scenario, &mut core, Checks::Evaluate))??;
    // The update at 0 schedules a wake at 60. That wake asks for a fix, the script answers it,
    // and the answer schedules a wake at 120, whose fix request finds the script empty.
    let wakes: Vec<u64> = report
        .steps
        .iter()
        .filter(|step| step.summary == "scheduled wake fell due")
        .map(|step| step.at.as_secs())
        .collect();
    assert_eq!(wakes, [60, 120]);
    assert!(!report.failed(), "{report}");
    Ok(())
}

#[test]
fn a_core_that_never_stops_asking_for_fixes_is_stopped() -> anyhow::Result<()> {
    let mut steps = String::new();
    for _ in 0..20 {
        steps.push_str(
            "[[steps]]\nat = 0\ndevice = \"alice\"\nfix = { spot = \"anchor\", position = \"inside\" }\n",
        );
    }
    steps.push_str("[[steps]]\nat = 0\ndevice = \"alice\"\nwake = \"push\"\n");
    let scenario = parse(&format!("{ONE_DEVICE}\n{steps}"))?;
    let error = block_on(run(&scenario, &mut GreedyCore, Checks::Evaluate))?.err();
    assert_eq!(
        error.map(|error| error.to_string()),
        Some(String::from(
            "alice kept requesting fixes: more than 16 round trips in one step"
        ))
    );
    Ok(())
}

#[test]
fn devices_start_with_their_configured_focus_and_push() -> anyhow::Result<()> {
    let scenario = parse(&format!(
        "{ONE_DEVICE}\n[devices.alice]\nfocus = \"on\"\npush = \"unified_push\""
    ))?;
    let world = super::runner::world(&scenario);
    let alice = world
        .devices
        .first()
        .ok_or_else(|| anyhow::anyhow!("no device"))?;
    assert_eq!(alice.platform.focus(), pingers_gate::FocusState::On);
    assert_eq!(
        alice.platform.push_registration().map(|push| push.platform),
        Some(pingers_proto::PushPlatform::UnifiedPush)
    );
    assert_eq!(alice.member.name, "alice");
    assert_eq!(pingers_time::Clock::now(&world.clock), scenario.start);
    Ok(())
}
