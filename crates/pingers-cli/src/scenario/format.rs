// SPDX-License-Identifier: AGPL-3.0-only
//! The scenario file format (TOML) and its validation into a [`Scenario`].
//!
//! The format is documented with examples in `crates/pingers-cli/scenarios/README.md`.

use core::time::Duration;
use std::collections::BTreeMap;

use pingers_gate::FocusState;
use pingers_geo::{Coordinate, Fix, GeoError, Meters};
use pingers_proto::PushPlatform;
use pingers_testkit::{
    EventKind, FixtureError, LocationMode, NoticeKind, PartyBuilder, PartyFixture, TEST_START,
    seeded_rng,
};
use pingers_time::UnixSeconds;
use serde::Deserialize;

/// Why a scenario file was rejected. Steps are numbered from 1 in file order.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ScenarioError {
    /// Not valid TOML, or not the shape described in the README.
    #[error("scenario is not valid: {0}")]
    Parse(#[from] toml::de::Error),
    /// The Party could not be built.
    #[error("party: {0}")]
    Party(#[from] FixtureError),
    /// `[devices.<name>]` names someone who is not a Party member.
    #[error("[devices.{0}] is not a member of the party")]
    DeviceNotMember(String),
    /// A step's `device` is not a Party member.
    #[error("step {step}: device `{device}` is not a member of the party")]
    UnknownDevice {
        /// The step.
        step: usize,
        /// The name given.
        device: String,
    },
    /// A step names a Spot the Party does not have.
    #[error("step {step}: {source}")]
    Spot {
        /// The step.
        step: usize,
        /// Which Spot.
        source: FixtureError,
    },
    /// A step's position is not a valid coordinate or accuracy.
    #[error("step {step}: {source}")]
    Geo {
        /// The step.
        step: usize,
        /// Why.
        source: GeoError,
    },
    /// A step is earlier than the one before it.
    #[error("step {step}: at = {at} is earlier than the previous step")]
    OutOfOrder {
        /// The step.
        step: usize,
        /// Its offset.
        at: u64,
    },
    /// A step's time is past the end of representable time.
    #[error("step {step}: at = {at} overflows the start time")]
    TimeOverflow {
        /// The step.
        step: usize,
        /// Its offset.
        at: u64,
    },
    /// A step has no action, or more than one.
    #[error(
        "step {step}: needs exactly one of fix, location, geofence, focus, wake, expect (found {found})"
    )]
    ActionCount {
        /// The step.
        step: usize,
        /// How many were given.
        found: usize,
    },
}

/// A validated scenario, ready to run.
#[derive(Clone, Debug)]
pub(crate) struct Scenario {
    /// Short name, for the report.
    pub(crate) name: String,
    /// What it shows.
    pub(crate) description: String,
    /// Wall time at offset 0.
    pub(crate) start: UnixSeconds,
    /// The Party; its members are the devices.
    pub(crate) party: PartyFixture,
    /// Initial device state, one per member, in member order.
    pub(crate) devices: Vec<DeviceSpec>,
    /// The timeline, in time order.
    pub(crate) steps: Vec<Step>,
}

/// A device's state before the first step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceSpec {
    /// The member's name.
    pub(crate) name: String,
    /// Focus/DND.
    pub(crate) focus: FocusState,
    /// Push registration, if any.
    pub(crate) push: Option<PushPlatform>,
}

/// One timeline entry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Step {
    /// 1-based position in the file.
    pub(crate) number: usize,
    /// Offset from the start.
    pub(crate) at: Duration,
    /// The device it happens on.
    pub(crate) device: String,
    /// What happens.
    pub(crate) action: Action,
}

/// What a step does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// Queue a fix as the answer to the device's next fix request.
    ScriptFix(Fix),
    /// The OS delivers an unsolicited location update now.
    Location(Fix),
    /// The OS reports a geofence transition for a region.
    Geofence {
        /// The region: a Spot name.
        region: String,
        /// Enter or exit.
        transition: Transition,
    },
    /// The interruption filter changes.
    Focus(FocusState),
    /// A wake arrives that the core did not schedule through the fake (a push).
    Push,
    /// Check the device's state.
    Expect(Expectation),
}

/// An OS geofence transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Transition {
    /// Entered the region.
    Enter,
    /// Left the region.
    Exit,
}

/// A check on one device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Expectation {
    /// The location mode the core last set.
    LocationMode(ModeName),
    /// Whether a background wake is pending.
    WakePending(bool),
    /// A wake is pending and due within this many seconds.
    WakeWithinS(u64),
    /// How many OS geofences are registered.
    Geofences(usize),
    /// How many fixes the core has requested so far.
    FixRequests(usize),
    /// At least one event of this kind so far.
    Event(EventKind),
    /// No event of this kind so far.
    NoEvent(EventKind),
    /// At least one notice of this kind so far.
    Notice(NoticeKind),
}

/// [`LocationMode`] as spelled in scenario files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModeName {
    /// [`LocationMode::Idle`].
    Idle,
    /// [`LocationMode::Dwell`].
    Dwell,
    /// [`LocationMode::MeetupBackground`].
    MeetupBackground,
    /// [`LocationMode::MeetupForeground`].
    MeetupForeground,
}

impl From<ModeName> for LocationMode {
    fn from(mode: ModeName) -> Self {
        match mode {
            ModeName::Idle => Self::Idle,
            ModeName::Dwell => Self::Dwell,
            ModeName::MeetupBackground => Self::MeetupBackground,
            ModeName::MeetupForeground => Self::MeetupForeground,
        }
    }
}

// The file as written.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioFile {
    name: String,
    #[serde(default)]
    description: String,
    start: Option<u64>,
    #[serde(default)]
    seed: u64,
    party: PartyTable,
    #[serde(default)]
    devices: BTreeMap<String, DeviceTable>,
    #[serde(default)]
    steps: Vec<StepTable>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartyTable {
    members: Vec<String>,
    #[serde(default)]
    spots: Vec<SpotTable>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpotTable {
    name: String,
    latitude: f64,
    longitude: f64,
    radius_m: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceTable {
    #[serde(default)]
    focus: FocusName,
    #[serde(default)]
    push: PushName,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FocusName {
    On,
    #[default]
    Off,
    Unknown,
}

impl From<FocusName> for FocusState {
    fn from(focus: FocusName) -> Self {
        match focus {
            FocusName::On => Self::On,
            FocusName::Off => Self::Off,
            FocusName::Unknown => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PushName {
    #[default]
    Fcm,
    UnifiedPush,
    None,
}

impl From<PushName> for Option<PushPlatform> {
    fn from(push: PushName) -> Self {
        match push {
            PushName::Fcm => Some(PushPlatform::Fcm),
            PushName::UnifiedPush => Some(PushPlatform::UnifiedPush),
            PushName::None => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepTable {
    at: u64,
    device: String,
    fix: Option<FixTable>,
    location: Option<FixTable>,
    geofence: Option<GeofenceTable>,
    focus: Option<FocusName>,
    wake: Option<WakeName>,
    expect: Option<Expectation>,
}

/// A position: relative to a Spot, or explicit.
#[derive(Deserialize)]
#[serde(untagged)]
enum FixTable {
    Spot {
        spot: String,
        position: Position,
    },
    Coordinate {
        latitude: f64,
        longitude: f64,
        #[serde(default = "default_accuracy")]
        accuracy_m: u32,
    },
}

const fn default_accuracy() -> u32 {
    10
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Position {
    Inside,
    Outside,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeofenceTable {
    region: String,
    transition: Transition,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WakeName {
    Push,
}

/// Parses and validates a scenario file.
///
/// # Errors
///
/// [`ScenarioError`] naming the first problem.
#[cfg(test)]
pub(crate) fn parse(text: &str) -> Result<Scenario, ScenarioError> {
    parse_with_seed(text, None)
}

/// Parses and validates a scenario file, replacing its `seed` if `seed` is given (the
/// configuration file's `seed`).
///
/// # Errors
///
/// [`ScenarioError`] naming the first problem.
pub(crate) fn parse_with_seed(text: &str, seed: Option<u64>) -> Result<Scenario, ScenarioError> {
    let file: ScenarioFile = toml::from_str(text)?;
    let start = file.start.map_or(TEST_START, UnixSeconds::new);
    let party = build_party(&file.party, seed.unwrap_or(file.seed))?;
    let devices = device_specs(&party, file.devices)?;
    let mut steps = Vec::with_capacity(file.steps.len());
    let mut previous = 0;
    for (index, table) in file.steps.into_iter().enumerate() {
        let number = index.saturating_add(1);
        if table.at < previous {
            return Err(ScenarioError::OutOfOrder {
                step: number,
                at: table.at,
            });
        }
        previous = table.at;
        steps.push(step(number, table, start, &party)?);
    }
    Ok(Scenario {
        name: file.name,
        description: file.description,
        start,
        party,
        devices,
        steps,
    })
}

fn build_party(table: &PartyTable, seed: u64) -> Result<PartyFixture, ScenarioError> {
    let mut builder = PartyBuilder::new();
    for member in &table.members {
        builder = builder.member(member);
    }
    for spot in &table.spots {
        let center = Coordinate::new(spot.latitude, spot.longitude).map_err(FixtureError::from)?;
        builder = builder.circle_spot(&spot.name, center, Meters::from_whole(spot.radius_m))?;
    }
    Ok(builder.build(&mut seeded_rng(seed))?)
}

fn device_specs(
    party: &PartyFixture,
    mut tables: BTreeMap<String, DeviceTable>,
) -> Result<Vec<DeviceSpec>, ScenarioError> {
    let specs = party
        .members
        .iter()
        .map(|member| {
            let table = tables.remove(&member.name).unwrap_or(DeviceTable {
                focus: FocusName::default(),
                push: PushName::default(),
            });
            DeviceSpec {
                name: member.name.clone(),
                focus: table.focus.into(),
                push: table.push.into(),
            }
        })
        .collect();
    match tables.into_keys().next() {
        Some(stranger) => Err(ScenarioError::DeviceNotMember(stranger)),
        None => Ok(specs),
    }
}

fn step(
    number: usize,
    table: StepTable,
    start: UnixSeconds,
    party: &PartyFixture,
) -> Result<Step, ScenarioError> {
    if party.member(&table.device).is_err() {
        return Err(ScenarioError::UnknownDevice {
            step: number,
            device: table.device,
        });
    }
    let at = Duration::from_secs(table.at);
    let taken_at = start.checked_add(at).ok_or(ScenarioError::TimeOverflow {
        step: number,
        at: table.at,
    })?;
    let fix = |fix_table: FixTable| resolve_fix(number, fix_table, taken_at, party);
    let mut actions = Vec::new();
    if let Some(table) = table.fix {
        actions.push(Action::ScriptFix(fix(table)?));
    }
    if let Some(table) = table.location {
        actions.push(Action::Location(fix(table)?));
    }
    if let Some(geofence) = table.geofence {
        party
            .spot(&geofence.region)
            .map_err(|source| ScenarioError::Spot {
                step: number,
                source,
            })?;
        actions.push(Action::Geofence {
            region: geofence.region,
            transition: geofence.transition,
        });
    }
    if let Some(focus) = table.focus {
        actions.push(Action::Focus(focus.into()));
    }
    if let Some(WakeName::Push) = table.wake {
        actions.push(Action::Push);
    }
    if let Some(expectation) = table.expect {
        actions.push(Action::Expect(expectation));
    }
    let found = actions.len();
    match (actions.pop(), found) {
        (Some(action), 1) => Ok(Step {
            number,
            at,
            device: table.device,
            action,
        }),
        _ => Err(ScenarioError::ActionCount {
            step: number,
            found,
        }),
    }
}

fn resolve_fix(
    step: usize,
    table: FixTable,
    taken_at: UnixSeconds,
    party: &PartyFixture,
) -> Result<Fix, ScenarioError> {
    match table {
        FixTable::Spot { spot, position } => {
            let spot = party
                .spot(&spot)
                .map_err(|source| ScenarioError::Spot { step, source })?;
            match position {
                Position::Inside => Ok(spot.fix_inside(taken_at)),
                Position::Outside => spot
                    .fix_outside(taken_at)
                    .map_err(|source| ScenarioError::Spot { step, source }),
            }
        }
        FixTable::Coordinate {
            latitude,
            longitude,
            accuracy_m,
        } => Ok(Fix {
            coordinate: Coordinate::new(latitude, longitude)
                .map_err(|source| ScenarioError::Geo { step, source })?,
            accuracy: Meters::from_whole(accuracy_m),
            taken_at,
        }),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    const HEADER: &str = r#"
        name = "test"
        [party]
        members = ["alice", "bob"]
        [[party.spots]]
        name = "anchor"
        latitude = 51.5
        longitude = -0.12
        radius_m = 150
    "#;

    fn with_steps(steps: &str) -> String {
        format!("{HEADER}\n{steps}")
    }

    #[test]
    fn a_minimal_scenario_has_defaults() -> Result<(), ScenarioError> {
        let scenario = parse(HEADER)?;
        assert_eq!(scenario.start, TEST_START);
        assert_eq!(
            scenario.devices,
            [
                DeviceSpec {
                    name: String::from("alice"),
                    focus: FocusState::Off,
                    push: Some(PushPlatform::Fcm)
                },
                DeviceSpec {
                    name: String::from("bob"),
                    focus: FocusState::Off,
                    push: Some(PushPlatform::Fcm)
                },
            ]
        );
        assert!(scenario.steps.is_empty());
        Ok(())
    }

    #[test]
    fn device_tables_override_defaults() -> Result<(), ScenarioError> {
        let scenario = parse(&with_steps(
            "[devices.bob]\nfocus = \"on\"\npush = \"none\"",
        ))?;
        assert_eq!(
            scenario.devices.get(1),
            Some(&DeviceSpec {
                name: String::from("bob"),
                focus: FocusState::On,
                push: None
            })
        );
        Ok(())
    }

    #[test]
    fn a_device_table_for_a_stranger_is_rejected() {
        assert!(matches!(
            parse(&with_steps("[devices.mallory]\nfocus = \"on\"")),
            Err(ScenarioError::DeviceNotMember(name)) if name == "mallory"
        ));
    }

    #[test]
    fn every_action_parses() -> Result<(), ScenarioError> {
        let scenario = parse(&with_steps(
            r#"
            [[steps]]
            at = 0
            device = "alice"
            fix = { spot = "anchor", position = "inside" }
            [[steps]]
            at = 5
            device = "alice"
            location = { latitude = 1.0, longitude = 2.0, accuracy_m = 30 }
            [[steps]]
            at = 5
            device = "bob"
            geofence = { region = "anchor", transition = "exit" }
            [[steps]]
            at = 6
            device = "bob"
            focus = "unknown"
            [[steps]]
            at = 7
            device = "bob"
            wake = "push"
            [[steps]]
            at = 8
            device = "bob"
            expect = { location_mode = "meetup_background" }
            "#,
        ))?;
        let actions: Vec<&Action> = scenario.steps.iter().map(|step| &step.action).collect();
        let anchor = scenario.party.spot("anchor")?;
        let at_five = UnixSeconds::new(1_763_000_105);
        assert_eq!(
            actions,
            [
                &Action::ScriptFix(anchor.fix_inside(TEST_START)),
                &Action::Location(Fix {
                    coordinate: Coordinate::new(1.0, 2.0).map_err(FixtureError::from)?,
                    accuracy: Meters::from_whole(30),
                    taken_at: at_five,
                }),
                &Action::Geofence {
                    region: String::from("anchor"),
                    transition: Transition::Exit
                },
                &Action::Focus(FocusState::Unknown),
                &Action::Push,
                &Action::Expect(Expectation::LocationMode(ModeName::MeetupBackground)),
            ]
        );
        let numbers: Vec<usize> = scenario.steps.iter().map(|step| step.number).collect();
        assert_eq!(numbers, [1, 2, 3, 4, 5, 6]);
        Ok(())
    }

    #[test]
    fn an_outside_fix_is_outside_the_spot() -> Result<(), ScenarioError> {
        let scenario = parse(&with_steps(
            "[[steps]]\nat = 0\ndevice = \"alice\"\nfix = { spot = \"anchor\", position = \"outside\" }",
        ))?;
        let anchor = scenario.party.spot("anchor")?;
        assert_eq!(
            scenario.steps.first().map(|step| &step.action),
            Some(&Action::ScriptFix(anchor.fix_outside(TEST_START)?))
        );
        Ok(())
    }

    #[test]
    fn the_start_time_offsets_every_fix() -> Result<(), ScenarioError> {
        let scenario = parse(&format!(
            "start = 1000\n{}",
            with_steps(
                "[[steps]]\nat = 30\ndevice = \"alice\"\nfix = { spot = \"anchor\", position = \"inside\" }"
            )
        ))?;
        assert_eq!(scenario.start, UnixSeconds::new(1_000));
        assert!(matches!(
            scenario.steps.first().map(|step| &step.action),
            Some(Action::ScriptFix(fix)) if fix.taken_at == UnixSeconds::new(1_030)
        ));
        Ok(())
    }

    #[test]
    fn steps_must_be_in_time_order() {
        let text = with_steps(
            "[[steps]]\nat = 10\ndevice = \"alice\"\nwake = \"push\"\n[[steps]]\nat = 9\ndevice = \"alice\"\nwake = \"push\"",
        );
        assert!(matches!(
            parse(&text),
            Err(ScenarioError::OutOfOrder { step: 2, at: 9 })
        ));
    }

    #[test]
    fn a_step_needs_exactly_one_action() {
        let none = with_steps("[[steps]]\nat = 0\ndevice = \"alice\"");
        assert!(matches!(
            parse(&none),
            Err(ScenarioError::ActionCount { step: 1, found: 0 })
        ));
        let two =
            with_steps("[[steps]]\nat = 0\ndevice = \"alice\"\nwake = \"push\"\nfocus = \"on\"");
        assert!(matches!(
            parse(&two),
            Err(ScenarioError::ActionCount { step: 1, found: 2 })
        ));
    }

    #[test]
    fn unknown_names_are_rejected_with_the_step_number() {
        let device = with_steps("[[steps]]\nat = 0\ndevice = \"carol\"\nwake = \"push\"");
        assert!(matches!(
            parse(&device),
            Err(ScenarioError::UnknownDevice { step: 1, device }) if device == "carol"
        ));
        let spot = with_steps(
            "[[steps]]\nat = 0\ndevice = \"alice\"\ngeofence = { region = \"pub\", transition = \"enter\" }",
        );
        assert!(matches!(
            parse(&spot),
            Err(ScenarioError::Spot { step: 1, source: FixtureError::UnknownSpot(name) }) if name == "pub"
        ));
        let fix_spot = with_steps(
            "[[steps]]\nat = 0\ndevice = \"alice\"\nfix = { spot = \"pub\", position = \"inside\" }",
        );
        assert!(matches!(
            parse(&fix_spot),
            Err(ScenarioError::Spot { step: 1, .. })
        ));
    }

    #[test]
    fn invalid_coordinates_are_rejected() {
        let text = with_steps(
            "[[steps]]\nat = 0\ndevice = \"alice\"\nlocation = { latitude = 95.0, longitude = 0.0 }",
        );
        assert!(matches!(
            parse(&text),
            Err(ScenarioError::Geo {
                step: 1,
                source: GeoError::LatitudeOutOfRange
            })
        ));
    }

    #[test]
    fn a_step_past_the_end_of_time_is_rejected() {
        let text = format!(
            "start = {}\n{}",
            u64::MAX,
            with_steps("[[steps]]\nat = 1\ndevice = \"alice\"\nwake = \"push\"")
        );
        assert!(matches!(
            parse(&text),
            Err(ScenarioError::TimeOverflow { step: 1, at: 1 })
        ));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let text = with_steps("[[steps]]\nat = 0\ndevise = \"alice\"\nwake = \"push\"");
        assert!(matches!(parse(&text), Err(ScenarioError::Parse(_))));
    }

    #[test]
    fn duplicate_members_are_a_party_error() {
        let text = "name = \"x\"\n[party]\nmembers = [\"alice\", \"alice\"]";
        assert!(matches!(
            parse(text),
            Err(ScenarioError::Party(FixtureError::DuplicateMember(_)))
        ));
    }

    #[test]
    fn mode_names_map_to_location_modes() {
        assert_eq!(LocationMode::from(ModeName::Idle), LocationMode::Idle);
        assert_eq!(LocationMode::from(ModeName::Dwell), LocationMode::Dwell);
        assert_eq!(
            LocationMode::from(ModeName::MeetupForeground),
            LocationMode::MeetupForeground
        );
    }
}
