// SPDX-License-Identifier: AGPL-3.0-only
//! Headless client and load generator.
//!
//! A headless client over pingers-client for manual testing, end-to-end scenarios (G.3 to G.7
//! in the design), and load generation for the board-size and server-cost experiments.
//!
//! - `scenario check|run FILE`: scripted timelines (format: `scenarios/README.md`) replayed on
//!   pingers-testkit's fake platforms and manual clock.
//! - `e2e g3..g7|all`: the bundled design scenarios.
//! - `load`: N simulated devices (pingers-e2e's protocol-level devices) each post one lease
//!   through the relay to an in-process server; then one more device syncs and reads the
//!   whole board. Prints the board size, the bytes of one read and one upload, and the daily
//!   totals they imply. Knocks are planned but not generated yet.
//!
//! `--dry-run` replays the timeline without clients. Without it each device gets a real
//! pingers-client `Client` through `scenario/client_driver.rs`; until the L3 crates land the
//! clients run on testkit's `FakeCrates` and expectations are skipped. Protocol-level runs
//! against the real server live in pingers-e2e.
#![forbid(unsafe_code)]

mod args;
mod config;
mod load;
mod scenario;

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use pingers_client::ClientError;
use pingers_testkit::FakeCrates;

use crate::args::{Command, LoadMode, RunMode};
use crate::config::{Config, Target};
use crate::scenario::{Checks, ClientDriver, DryRunDriver};

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(code) => code,
        Err(error) => {
            let _ignored = writeln!(io::stderr(), "pingers-cli: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<ExitCode> {
    let invocation = args::parse(arguments)?;
    let config = match &invocation.config {
        Some(path) => {
            config::parse(&read(path)?).with_context(|| format!("in {}", path.display()))?
        }
        None => Config::default(),
    };
    let mut out = io::stdout().lock();
    match invocation.command {
        Command::Help => {
            out.write_all(args::USAGE.as_bytes())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::ScenarioCheck { file } => {
            let scenario = scenario::parse_with_seed(&read(&file)?, config.seed)
                .with_context(|| format!("in {}", file.display()))?;
            writeln!(
                out,
                "{}: valid ({} devices, {} steps): {}",
                scenario.name,
                scenario.devices.len(),
                scenario.steps.len(),
                scenario.description
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Command::ScenarioRun { file, mode } => {
            let text = read(&file)?;
            run_scenarios(
                &mut out,
                [(file.display().to_string(), text.as_str())],
                mode,
                &config,
            )
        }
        Command::E2e { scenarios, mode } => {
            let named = scenarios
                .iter()
                .map(|builtin| (String::from(builtin.name), builtin.text));
            run_scenarios(&mut out, named, mode, &config)
        }
        Command::Load { plan, mode } => {
            writeln!(out, "{plan}")?;
            if mode == LoadMode::PlanOnly {
                return Ok(ExitCode::SUCCESS);
            }
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .context("starting the async runtime")?;
            let report = runtime
                .block_on(pingers_e2e::load::run(pingers_e2e::load::LoadConfig {
                    devices: plan.devices(),
                    seed: plan.seed,
                }))
                .context("generating load against the in-process server")?;
            writeln!(out, "{report}")?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Runs each named scenario text, printing a report per scenario.
fn run_scenarios<'text>(
    out: &mut impl io::Write,
    scenarios: impl IntoIterator<Item = (String, &'text str)>,
    mode: RunMode,
    config: &Config,
) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let mut failed = false;
    for (name, text) in scenarios {
        let scenario =
            scenario::parse_with_seed(text, config.seed).with_context(|| format!("in {name}"))?;
        let report = match mode {
            RunMode::DryRun => {
                let mut driver = DryRunDriver::default();
                runtime.block_on(scenario::run(&scenario, &mut driver, Checks::Skip))?
            }
            RunMode::Client => {
                if !matches!(config.target, Target::InProcess) {
                    anyhow::bail!(
                        "client runs use the in-process harness only so far (target: {})",
                        describe(&config.target)
                    );
                }
                let directory = ScratchDirectory::new()?;
                // TODO(wave 3): build the real pingers-parties, pingers-sync, pingers-wake and
                // pingers-meetup-session against the in-process harness once they land, and
                // evaluate expectations. Until then each client runs on testkit's FakeCrates,
                // which record calls and raise no events, so expectations are skipped.
                let mut driver =
                    ClientDriver::new(directory.path().to_path_buf(), |_device, _database| {
                        FakeCrates::new()
                            .map(|fakes| fakes.crates())
                            .map_err(|_invalid| ClientError::Internal)
                    });
                let report =
                    runtime.block_on(scenario::run(&scenario, &mut driver, Checks::Skip))?;
                writeln!(
                    out,
                    "{name}: clients ran on stand-in L3 crates (FakeCrates); expectations are skipped until the real crates land"
                )?;
                report
            }
        };
        failed |= report.failed();
        writeln!(out, "{report}")?;
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// A fresh directory for one run's client databases, removed on drop.
struct ScratchDirectory(PathBuf);

impl ScratchDirectory {
    fn new() -> anyhow::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "pingers-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        // Best effort: a leftover temporary directory fails nothing.
        drop(fs::remove_dir_all(&self.0));
    }
}

fn describe(target: &Target) -> String {
    match target {
        Target::InProcess => String::from("in-process harness"),
        Target::Remote { server, .. } => server.to_string(),
    }
}

fn read(path: &Path) -> anyhow::Result<String> {
    fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn arguments(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_bundled_scenarios_run_on_clients() -> anyhow::Result<()> {
        assert_eq!(run(arguments("e2e all"))?, ExitCode::SUCCESS);
        Ok(())
    }

    #[test]
    fn the_bundled_scenarios_still_dry_run() -> anyhow::Result<()> {
        assert_eq!(run(arguments("e2e g4 --dry-run"))?, ExitCode::SUCCESS);
        Ok(())
    }
}
