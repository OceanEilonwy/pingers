// SPDX-License-Identifier: AGPL-3.0-only
//! Runs a [`Scenario`] against a [`ScenarioDriver`] on fake platforms and a manual clock.
//!
//! The runner owns time and the platforms; the driver owns the clients. For each step in time
//! order the runner advances the shared clock (delivering any wake a device scheduled that
//! falls due on the way), applies the step to the device's [`FakePlatform`], hands the
//! resulting input to the driver and lets it settle, then delivers any fixes the device
//! requested and the script answered, until the device is quiet. Expectations are checked
//! against the device's platform and recorded events.

use core::fmt;
use core::future::{self, Future};
use core::time::Duration;

use pingers_geo::Fix;
use pingers_proto::PushToken;
use pingers_testkit::{
    FakePlatform, LocationMode, ManualClock, MemberFixture, PartyFixture, PushRegistration,
    RecordingEvents, advance_to,
};
use pingers_time::{Timer as _, UnixSeconds};

use super::format::{Action, Expectation, Scenario, Transition};

/// How many fix round trips one step may cause before the runner gives up: a core that asks
/// for a fix every time it receives one would otherwise loop for ever.
const MAX_FIX_ROUNDS: usize = 16;

/// What a device's core receives from its platform.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DeviceInput {
    /// A location fix (answering a request, or an unsolicited update).
    Fix(Fix),
    /// An OS geofence transition.
    Geofence {
        /// The region id: the Spot name, for drivers to map to their own region ids.
        region: String,
        /// Enter or exit.
        transition: Transition,
    },
    /// The interruption filter changed; read it from the platform.
    FocusChanged,
    /// A background wake the core scheduled fell due.
    ScheduledWake,
    /// A push woke the device.
    Push,
}

/// One device under test.
#[derive(Clone, Debug)]
pub(crate) struct Device {
    /// The member's name in the scenario.
    pub(crate) name: String,
    /// The member's fixture (account, leaf).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by the pingers-client driver (wave 3)")
    )]
    pub(crate) member: MemberFixture,
    /// The device's fake platform.
    pub(crate) platform: FakePlatform,
    /// The device's recorded events.
    pub(crate) events: RecordingEvents,
}

/// Everything a driver needs to build its clients.
#[derive(Clone, Debug)]
pub(crate) struct World {
    /// The clock every device and the harness share.
    pub(crate) clock: ManualClock,
    /// The Party all devices belong to.
    #[expect(dead_code, reason = "read by the pingers-client driver (wave 3)")]
    pub(crate) party: PartyFixture,
    /// The devices, in member order.
    pub(crate) devices: Vec<Device>,
}

/// What runs the clients. pingers-cli implements it for pingers-client in wave 3 (a driver
/// that builds one client per device from its `FakePlatform` and `RecordingEvents`, against
/// the in-process harness); until then [`DryRunDriver`] replays the timeline without clients.
pub(crate) trait ScenarioDriver {
    /// Builds the clients before the first step: one per device, all in the Party.
    async fn start(&mut self, world: &World) -> anyhow::Result<()>;

    /// Delivers one platform input to a device's core.
    async fn deliver(&mut self, device: &Device, input: DeviceInput) -> anyhow::Result<()>;

    /// Lets every client finish the work its inputs started (run until idle).
    async fn settle(&mut self, world: &World) -> anyhow::Result<()>;
}

/// Whether expectations are checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Checks {
    /// Check each expectation.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "used once the pingers-client driver exists (wave 3)"
        )
    )]
    Evaluate,
    /// Report expectations as skipped (the dry-run driver has no clients to check).
    Skip,
}

/// What happened at one step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// An input was delivered.
    Delivered,
    /// An expectation held.
    Passed,
    /// An expectation did not hold, and why.
    Failed(String),
    /// An expectation was not checked.
    Skipped,
}

/// One line of the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StepReport {
    /// The step's number in the file.
    pub(crate) number: usize,
    /// Its offset from the start.
    pub(crate) at: Duration,
    /// The device.
    pub(crate) device: String,
    /// What the step did, in words.
    pub(crate) summary: String,
    /// The outcome.
    pub(crate) outcome: Outcome,
}

/// The result of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    /// The scenario's name.
    pub(crate) scenario: String,
    /// One entry per step, plus one per wake that fell due.
    pub(crate) steps: Vec<StepReport>,
}

impl Report {
    /// Whether any expectation failed.
    pub(crate) fn failed(&self) -> bool {
        self.steps
            .iter()
            .any(|step| matches!(step.outcome, Outcome::Failed(_)))
    }
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "scenario {}", self.scenario)?;
        for step in &self.steps {
            let outcome = match &step.outcome {
                Outcome::Delivered => String::from("ok"),
                Outcome::Passed => String::from("passed"),
                Outcome::Failed(why) => format!("FAILED: {why}"),
                Outcome::Skipped => String::from("skipped"),
            };
            writeln!(
                formatter,
                "  #{:<3} +{:>5}s  {:<8} {:<40} {outcome}",
                step.number,
                step.at.as_secs(),
                step.device,
                step.summary
            )?;
        }
        let skipped = self
            .steps
            .iter()
            .filter(|step| step.outcome == Outcome::Skipped)
            .count();
        if self.failed() {
            write!(formatter, "FAILED")
        } else if skipped > 0 {
            write!(formatter, "replayed; {skipped} expectations skipped")
        } else {
            write!(formatter, "passed")
        }
    }
}

/// Builds the world for `scenario`: one fake platform per device on a clock at the start.
pub(crate) fn world(scenario: &Scenario) -> World {
    let clock = ManualClock::new(scenario.start);
    let devices = scenario
        .devices
        .iter()
        .zip(&scenario.party.members)
        .map(|(spec, member)| {
            let platform = FakePlatform::new(clock.clone());
            platform.set_focus(spec.focus);
            platform.set_push_registration(spec.push.map(|push| PushRegistration {
                platform: push,
                token: PushToken::from_bytes(format!("token-{}", spec.name).into_bytes()),
            }));
            Device {
                name: spec.name.clone(),
                member: member.clone(),
                platform,
                events: RecordingEvents::new(),
            }
        })
        .collect();
    World {
        clock,
        party: scenario.party.clone(),
        devices,
    }
}

/// Runs `scenario` on `driver`.
///
/// # Errors
///
/// If the driver fails, or a step causes more than `MAX_FIX_ROUNDS` fix round trips.
/// Failed expectations are not errors; they are in the [`Report`].
pub(crate) async fn run<D: ScenarioDriver>(
    scenario: &Scenario,
    driver: &mut D,
    checks: Checks,
) -> anyhow::Result<Report> {
    let world = world(scenario);
    driver.start(&world).await?;
    let mut run = Run {
        start: scenario.start,
        world,
        driver,
        report: Report {
            scenario: scenario.name.clone(),
            steps: Vec::new(),
        },
    };
    for step in &scenario.steps {
        let target = scenario
            .start
            .checked_add(step.at)
            .ok_or_else(|| anyhow::anyhow!("step {} is past the end of time", step.number))?;
        run.advance(target, step.number).await?;
        let device = device(&run.world, &step.device)?;
        let outcome = match &step.action {
            Action::Expect(expectation) => match checks {
                Checks::Evaluate => evaluate(&device, *expectation),
                Checks::Skip => Outcome::Skipped,
            },
            Action::ScriptFix(fix) => {
                device.platform.script_fix(*fix);
                Outcome::Delivered
            }
            Action::Location(fix) => {
                run.deliver(&device, DeviceInput::Fix(*fix)).await?;
                Outcome::Delivered
            }
            Action::Geofence { region, transition } => {
                let input = DeviceInput::Geofence {
                    region: region.clone(),
                    transition: *transition,
                };
                run.deliver(&device, input).await?;
                Outcome::Delivered
            }
            Action::Focus(focus) => {
                device.platform.set_focus(*focus);
                run.deliver(&device, DeviceInput::FocusChanged).await?;
                Outcome::Delivered
            }
            Action::Push => {
                run.deliver(&device, DeviceInput::Push).await?;
                Outcome::Delivered
            }
        };
        run.report.steps.push(StepReport {
            number: step.number,
            at: step.at,
            device: step.device.clone(),
            summary: summary(&step.action),
            outcome,
        });
    }
    Ok(run.report)
}

/// The state of one run.
struct Run<'driver, D> {
    start: UnixSeconds,
    world: World,
    driver: &'driver mut D,
    report: Report,
}

impl<D: ScenarioDriver> Run<'_, D> {
    /// Advances the clock to `target`, stopping at each wake a device scheduled on the way to
    /// deliver it, in due order (ties in member order). `step` is the step being approached.
    async fn advance(&mut self, target: UnixSeconds, step: usize) -> anyhow::Result<()> {
        loop {
            let now = self.world.clock.now();
            let remaining = target.saturating_duration_since(self.wall_now());
            let next = self
                .world
                .devices
                .iter()
                .filter_map(|device| Some((device.platform.pending_wake()?.due, device)))
                .filter(|(due, _device)| due.saturating_duration_since(now) <= remaining)
                .min_by_key(|(due, _device)| *due)
                .map(|(due, device)| (due, device.clone()));
            let Some((due, device)) = next else {
                advance_to(&self.world.clock, target)?;
                return Ok(());
            };
            self.world.clock.advance(due.saturating_duration_since(now));
            if device.platform.take_due_wake().is_some() {
                self.deliver(&device, DeviceInput::ScheduledWake).await?;
                self.report.steps.push(StepReport {
                    number: step,
                    at: self.wall_now().saturating_duration_since(self.start),
                    device: device.name.clone(),
                    summary: String::from("scheduled wake fell due"),
                    outcome: Outcome::Delivered,
                });
            }
        }
    }

    /// Delivers `input`, settles, then delivers the fixes the script answered until none are
    /// left.
    async fn deliver(&mut self, device: &Device, input: DeviceInput) -> anyhow::Result<()> {
        self.driver.deliver(device, input).await?;
        self.driver.settle(&self.world).await?;
        for _ in 0..MAX_FIX_ROUNDS {
            let answered = device.platform.take_answered_fixes();
            if answered.is_empty() {
                return Ok(());
            }
            for fix in answered {
                self.driver.deliver(device, DeviceInput::Fix(fix)).await?;
            }
            self.driver.settle(&self.world).await?;
        }
        anyhow::bail!(
            "{} kept requesting fixes: more than {MAX_FIX_ROUNDS} round trips in one step",
            device.name
        )
    }

    fn wall_now(&self) -> UnixSeconds {
        pingers_time::Clock::now(&self.world.clock)
    }
}

fn device(world: &World, name: &str) -> anyhow::Result<Device> {
    world
        .devices
        .iter()
        .find(|device| device.name == name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no device named {name}"))
}

/// Checks one expectation against a device's platform and events.
fn evaluate(device: &Device, expectation: Expectation) -> Outcome {
    let platform = &device.platform;
    let result = match expectation {
        Expectation::LocationMode(mode) => {
            let wanted = LocationMode::from(mode);
            let actual = platform.location_mode();
            check(actual == wanted, || {
                format!("location mode is {actual:?}, not {wanted:?}")
            })
        }
        Expectation::WakePending(wanted) => {
            let actual = platform.pending_wake().is_some();
            check(actual == wanted, || format!("wake pending is {actual}"))
        }
        Expectation::WakeWithinS(seconds) => {
            let now = platform.clock().now();
            let limit = Duration::from_secs(seconds);
            match platform.pending_wake() {
                Some(wake) => {
                    let due_in = wake.due.saturating_duration_since(now);
                    check(due_in <= limit, || {
                        format!("next wake is due in {} s", due_in.as_secs())
                    })
                }
                None => Err(String::from("no wake is pending")),
            }
        }
        Expectation::Geofences(wanted) => {
            let actual = platform.os_geofences().len();
            check(actual == wanted, || {
                format!("{actual} geofences registered")
            })
        }
        Expectation::FixRequests(wanted) => {
            let actual = platform.fix_requests().len();
            check(actual == wanted, || format!("{actual} fixes requested"))
        }
        Expectation::Event(kind) => device
            .events
            .expect_kind(kind)
            .map_err(|failure| failure.to_string()),
        Expectation::NoEvent(kind) => device
            .events
            .expect_no_kind(kind)
            .map_err(|failure| failure.to_string()),
        Expectation::Notice(kind) => device
            .events
            .expect_notice(kind)
            .map_err(|failure| failure.to_string()),
    };
    match result {
        Ok(()) => Outcome::Passed,
        Err(why) => Outcome::Failed(why),
    }
}

fn check(holds: bool, why: impl FnOnce() -> String) -> Result<(), String> {
    if holds { Ok(()) } else { Err(why()) }
}

fn summary(action: &Action) -> String {
    match action {
        Action::ScriptFix(_) => String::from("script fix"),
        Action::Location(_) => String::from("location update"),
        Action::Geofence { region, transition } => format!("geofence {transition:?} {region}"),
        Action::Focus(focus) => format!("focus {focus:?}"),
        Action::Push => String::from("push"),
        Action::Expect(expectation) => format!("expect {expectation:?}"),
    }
}

/// Replays the timeline without clients: records every input so the timeline itself can be
/// checked. Expectations need clients, so run it with [`Checks::Skip`].
#[derive(Debug, Default)]
pub(crate) struct DryRunDriver {
    /// Every input delivered, as `(device, input)`, in order.
    pub(crate) delivered: Vec<(String, DeviceInput)>,
    /// How many devices were set up.
    pub(crate) devices: usize,
    /// How many times the runner asked the clients to settle.
    pub(crate) settles: usize,
}

impl ScenarioDriver for DryRunDriver {
    // Nothing to await: plain functions returning ready futures implement the async methods.

    fn start(&mut self, world: &World) -> impl Future<Output = anyhow::Result<()>> {
        self.devices = world.devices.len();
        future::ready(Ok(()))
    }

    fn deliver(
        &mut self,
        device: &Device,
        input: DeviceInput,
    ) -> impl Future<Output = anyhow::Result<()>> {
        self.delivered.push((device.name.clone(), input));
        future::ready(Ok(()))
    }

    fn settle(&mut self, _world: &World) -> impl Future<Output = anyhow::Result<()>> {
        self.settles = self.settles.saturating_add(1);
        future::ready(Ok(()))
    }
}
