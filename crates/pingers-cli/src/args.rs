// SPDX-License-Identifier: AGPL-3.0-only
//! Command-line parsing. Hand-written: a handful of subcommands does not justify a new
//! workspace dependency (style guide §12).

use core::num::NonZeroU32;
use core::time::Duration;
use std::path::PathBuf;

use crate::load::LoadPlan;

/// Records one simulated device keeps on the board: one lease (design B.4.2).
const RECORDS_PER_DEVICE: NonZeroU32 = match NonZeroU32::new(4) {
    Some(records) => records,
    None => NonZeroU32::MIN,
};
use crate::scenario::{BUILTINS, Builtin, builtin};

/// The help text.
pub(crate) const USAGE: &str = "\
pingers-cli: headless Pingers client, scenario runner and load generator

Usage:
  pingers-cli [--config FILE] <command>

Commands:
  scenario check FILE               Validate a scenario file
  scenario run FILE [--dry-run]     Run a scenario file
  e2e NAME|all [--dry-run]          Run the bundled design scenarios (g3 to g7)
  load --board-size RECORDS | --devices N [--knock-rate PER_MINUTE]
       [--duration SECONDS] [--seed N] [--plan-only]
                                    Generate load against the server
  help                              Show this text

--dry-run replays a timeline on the fake platforms without clients and skips
expectations. Without it a client driver is needed (pingers-client, wave 3).
load runs N simulated devices (one lease of 4 records each) against the
in-process server and relay, then measures one full board read and one upload.
The scenario format is described in crates/pingers-cli/scenarios/README.md.
";

/// Why the command line was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ArgsError {
    /// Something required is missing.
    #[error("missing {0}")]
    Missing(&'static str),
    /// An argument was not recognised.
    #[error("unexpected argument `{0}`")]
    Unexpected(String),
    /// A flag's value did not parse.
    #[error("{flag} needs {expected}, not `{value}`")]
    Value {
        /// The flag.
        flag: &'static str,
        /// What it needs.
        expected: &'static str,
        /// What was given.
        value: String,
    },
    /// No bundled scenario has this name.
    #[error("no bundled scenario `{0}` (try g3, g4, g5, g6, g7 or all)")]
    UnknownScenario(String),
}

/// Clients or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunMode {
    /// Replay on the fake platforms only; skip expectations.
    DryRun,
    /// Drive real clients and check expectations.
    Client,
}

/// Plan only, or run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadMode {
    /// Print the plan and stop.
    PlanOnly,
    /// Generate the load.
    Run,
}

/// What to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Print [`USAGE`].
    Help,
    /// Validate a scenario file.
    ScenarioCheck {
        /// The file.
        file: PathBuf,
    },
    /// Run a scenario file.
    ScenarioRun {
        /// The file.
        file: PathBuf,
        /// Clients or none.
        mode: RunMode,
    },
    /// Run bundled scenarios.
    E2e {
        /// Which, in design order.
        scenarios: Vec<Builtin>,
        /// Clients or none.
        mode: RunMode,
    },
    /// Generate load.
    Load {
        /// The plan.
        plan: LoadPlan,
        /// Plan only, or run.
        mode: LoadMode,
    },
}

/// A parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invocation {
    /// `--config FILE`, if given.
    pub(crate) config: Option<PathBuf>,
    /// The command.
    pub(crate) command: Command,
}

/// Parses the arguments after the program name.
///
/// # Errors
///
/// [`ArgsError`] for the first problem.
pub(crate) fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Invocation, ArgsError> {
    let mut arguments = arguments.into_iter().peekable();
    let mut config = None;
    while arguments
        .peek()
        .is_some_and(|argument| argument == "--config")
    {
        arguments.next();
        config = Some(PathBuf::from(
            arguments
                .next()
                .ok_or(ArgsError::Missing("the --config file"))?,
        ));
    }
    let command = match arguments.next().as_deref() {
        None | Some("help" | "--help" | "-h") => Command::Help,
        Some("scenario") => scenario(&mut arguments)?,
        Some("e2e") => e2e(&mut arguments)?,
        Some("load") => load(&mut arguments)?,
        Some(other) => return Err(ArgsError::Unexpected(other.to_owned())),
    };
    if let Some(extra) = arguments.next() {
        return Err(ArgsError::Unexpected(extra));
    }
    Ok(Invocation { config, command })
}

fn scenario(arguments: &mut impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let action = arguments.next().ok_or(ArgsError::Missing("check or run"))?;
    let file = PathBuf::from(
        arguments
            .next()
            .ok_or(ArgsError::Missing("a scenario file"))?,
    );
    match action.as_str() {
        "check" => Ok(Command::ScenarioCheck { file }),
        "run" => Ok(Command::ScenarioRun {
            file,
            mode: run_mode(arguments)?,
        }),
        _ => Err(ArgsError::Unexpected(action)),
    }
}

fn e2e(arguments: &mut impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let name = arguments
        .next()
        .ok_or(ArgsError::Missing("a scenario name"))?;
    let scenarios = if name == "all" {
        BUILTINS.to_vec()
    } else {
        vec![builtin(&name).ok_or(ArgsError::UnknownScenario(name))?]
    };
    Ok(Command::E2e {
        scenarios,
        mode: run_mode(arguments)?,
    })
}

fn run_mode(arguments: &mut impl Iterator<Item = String>) -> Result<RunMode, ArgsError> {
    match arguments.next() {
        None => Ok(RunMode::Client),
        Some(flag) if flag == "--dry-run" => Ok(RunMode::DryRun),
        Some(other) => Err(ArgsError::Unexpected(other)),
    }
}

fn load(arguments: &mut impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let mut board_records = None;
    let mut knocks_per_minute = 0;
    let mut duration = Duration::from_mins(15);
    let mut seed = 0;
    let mut mode = LoadMode::Run;
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--board-size" => {
                board_records = Some(value(
                    arguments,
                    "--board-size",
                    "a positive record count",
                    |text| text.parse::<NonZeroU32>().ok(),
                )?);
            }
            "--devices" => {
                let devices: NonZeroU32 =
                    value(arguments, "--devices", "a positive device count", |text| {
                        text.parse::<NonZeroU32>().ok()
                    })?;
                board_records = Some(devices.checked_mul(RECORDS_PER_DEVICE).ok_or(
                    ArgsError::Value {
                        flag: "--devices",
                        expected: "at most 1073741823 devices",
                        value: devices.to_string(),
                    },
                )?);
            }
            "--knock-rate" => {
                knocks_per_minute =
                    value(arguments, "--knock-rate", "knocks per minute", |text| {
                        text.parse::<u32>().ok()
                    })?;
            }
            "--duration" => {
                duration = value(
                    arguments,
                    "--duration",
                    "a positive number of seconds",
                    |text| {
                        text.parse::<u64>()
                            .ok()
                            .filter(|seconds| *seconds > 0)
                            .map(Duration::from_secs)
                    },
                )?;
            }
            "--seed" => {
                seed = value(arguments, "--seed", "an unsigned integer", |text| {
                    text.parse::<u64>().ok()
                })?;
            }
            "--plan-only" => mode = LoadMode::PlanOnly,
            _ => return Err(ArgsError::Unexpected(flag)),
        }
    }
    let plan = LoadPlan {
        board_records: board_records.ok_or(ArgsError::Missing("--board-size or --devices"))?,
        knocks_per_minute,
        duration,
        seed,
    };
    Ok(Command::Load { plan, mode })
}

fn value<T>(
    arguments: &mut impl Iterator<Item = String>,
    flag: &'static str,
    expected: &'static str,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Result<T, ArgsError> {
    let text = arguments.next().ok_or(ArgsError::Value {
        flag,
        expected,
        value: String::new(),
    })?;
    parse(&text).ok_or(ArgsError::Value {
        flag,
        expected,
        value: text,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn args(line: &str) -> Result<Invocation, ArgsError> {
        parse(line.split_whitespace().map(str::to_owned))
    }

    fn command(line: &str) -> Result<Command, ArgsError> {
        args(line).map(|invocation| invocation.command)
    }

    #[test]
    fn no_arguments_and_help_flags_show_help() -> Result<(), ArgsError> {
        for line in ["", "help", "--help", "-h"] {
            assert_eq!(command(line)?, Command::Help);
        }
        Ok(())
    }

    #[test]
    fn config_comes_before_the_command() -> Result<(), ArgsError> {
        let invocation = args("--config pingers.toml scenario check g3.toml")?;
        assert_eq!(invocation.config, Some(PathBuf::from("pingers.toml")));
        assert_eq!(
            invocation.command,
            Command::ScenarioCheck {
                file: PathBuf::from("g3.toml")
            }
        );
        assert_eq!(
            args("--config"),
            Err(ArgsError::Missing("the --config file"))
        );
        Ok(())
    }

    #[test]
    fn scenario_run_defaults_to_clients() -> Result<(), ArgsError> {
        assert_eq!(
            command("scenario run a.toml")?,
            Command::ScenarioRun {
                file: PathBuf::from("a.toml"),
                mode: RunMode::Client
            }
        );
        assert_eq!(
            command("scenario run a.toml --dry-run")?,
            Command::ScenarioRun {
                file: PathBuf::from("a.toml"),
                mode: RunMode::DryRun
            }
        );
        Ok(())
    }

    #[test]
    fn scenario_needs_an_action_and_a_file() {
        assert_eq!(command("scenario"), Err(ArgsError::Missing("check or run")));
        assert_eq!(
            command("scenario run"),
            Err(ArgsError::Missing("a scenario file"))
        );
        assert_eq!(
            command("scenario lint a.toml"),
            Err(ArgsError::Unexpected(String::from("lint")))
        );
    }

    #[test]
    fn e2e_selects_one_or_all_bundled_scenarios() -> Result<(), ArgsError> {
        assert!(matches!(
            command("e2e G4 --dry-run")?,
            Command::E2e { scenarios, mode: RunMode::DryRun } if scenarios.iter().map(|s| s.name).eq(["g4"])
        ));
        assert!(matches!(
            command("e2e all")?,
            Command::E2e { scenarios, mode: RunMode::Client }
                if scenarios.iter().map(|s| s.name).eq(["g3", "g4", "g5", "g6", "g7"])
        ));
        assert_eq!(
            command("e2e g9"),
            Err(ArgsError::UnknownScenario(String::from("g9")))
        );
        assert_eq!(command("e2e"), Err(ArgsError::Missing("a scenario name")));
        Ok(())
    }

    #[test]
    fn load_parses_every_flag() -> Result<(), ArgsError> {
        let parsed =
            command("load --board-size 4000 --knock-rate 12 --duration 1800 --seed 5 --plan-only")?;
        assert_eq!(
            parsed,
            Command::Load {
                plan: LoadPlan {
                    board_records: NonZeroU32::MIN.saturating_add(3_999),
                    knocks_per_minute: 12,
                    duration: Duration::from_mins(30),
                    seed: 5,
                },
                mode: LoadMode::PlanOnly
            }
        );
        Ok(())
    }

    #[test]
    fn load_defaults_to_one_bucket_no_knocks_seed_zero() -> Result<(), ArgsError> {
        assert_eq!(
            command("load --board-size 8")?,
            Command::Load {
                plan: LoadPlan {
                    board_records: NonZeroU32::MIN.saturating_add(7),
                    knocks_per_minute: 0,
                    duration: Duration::from_mins(15),
                    seed: 0,
                },
                mode: LoadMode::Run
            }
        );
        Ok(())
    }

    #[test]
    fn load_rejects_missing_and_invalid_values() {
        assert_eq!(
            command("load"),
            Err(ArgsError::Missing("--board-size or --devices"))
        );
        assert!(matches!(
            command("load --board-size 0"),
            Err(ArgsError::Value {
                flag: "--board-size",
                ..
            })
        ));
        assert!(matches!(
            command("load --board-size 4 --duration 0"),
            Err(ArgsError::Value {
                flag: "--duration",
                ..
            })
        ));
        assert!(matches!(
            command("load --board-size 4 --knock-rate fast"),
            Err(ArgsError::Value { flag: "--knock-rate", value, .. }) if value == "fast"
        ));
        assert!(matches!(
            command("load --board-size 4 --seed"),
            Err(ArgsError::Value { flag: "--seed", .. })
        ));
        assert_eq!(
            command("load --board-size 4 --verbose"),
            Err(ArgsError::Unexpected(String::from("--verbose")))
        );
    }

    #[test]
    fn trailing_arguments_are_rejected() {
        assert_eq!(
            command("scenario check a.toml b.toml"),
            Err(ArgsError::Unexpected(String::from("b.toml")))
        );
        assert_eq!(
            command("scenario run a.toml --dry-run --fast"),
            Err(ArgsError::Unexpected(String::from("--fast")))
        );
        assert_eq!(
            command("frobnicate"),
            Err(ArgsError::Unexpected(String::from("frobnicate")))
        );
    }
}
