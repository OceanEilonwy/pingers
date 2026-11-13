// SPDX-License-Identifier: AGPL-3.0-only
//! Headless client and load generator.
//!
//! A headless client over pingers-client for manual testing, end-to-end scenarios (G.3 to G.7
//! in the design), and load generation for the board-size and server-cost experiments.
//!
//! - `scenario check|run FILE`: scripted timelines (format: `scenarios/README.md`) replayed on
//!   pingers-testkit's fake platforms and manual clock.
//! - `e2e g3..g7|all`: the bundled design scenarios.
//! - `load`: load plans (board size, knock rate); generation needs the in-process harness.
//!
//! Until the L3 crates land only `--dry-run` runs: the timeline is replayed without clients
//! and expectations are skipped. The runner calls the `ScenarioDriver` trait in
//! `scenario/runner.rs`; `scenario/client_driver.rs` implements it over pingers-client and
//! needs only the real crates to build each device's client from.
#![forbid(unsafe_code)]

mod args;
mod config;
mod load;
mod scenario;

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use pingers_testkit::{HarnessConfig, InProcessHarness};

use crate::args::{Command, LoadMode, RunMode};
use crate::config::{Config, Target};
use crate::scenario::{Checks, DryRunDriver};

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
            let harness_config = HarnessConfig {
                seed: plan.seed,
                ..HarnessConfig::default()
            };
            let _harness = InProcessHarness::start(&harness_config)
                .context("load generation needs the in-process harness")?;
            anyhow::bail!("the load generator's client driver arrives with pingers-client (wave 3)")
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
            RunMode::Client => anyhow::bail!(
                "the client driver has no L3 crates to build clients from yet (target: {}); use --dry-run",
                describe(&config.target)
            ),
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

fn describe(target: &Target) -> String {
    match target {
        Target::InProcess => String::from("in-process harness"),
        Target::Remote { server, .. } => server.to_string(),
    }
}

fn read(path: &Path) -> anyhow::Result<String> {
    fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}
