// SPDX-License-Identifier: AGPL-3.0-only
//! Workspace checks that cargo cannot express on its own.
//!
//! - `cargo xtask layers`: every crate declares its layer in
//!   `[package.metadata.pingers] layer = N` (0 to 5, or `"dev"`). A crate may depend only on
//!   crates in strictly lower layers, no production crate may depend on a dev crate, and no
//!   crate in L0 or L1 may depend on an I/O crate (`tokio`, `hyper`, `reqwest`, `rusqlite`),
//!   directly or transitively. A same-layer dependency is allowed only if it is listed, with a
//!   reason, under `[workspace.metadata.pingers] same-layer` in the workspace manifest; an
//!   entry that is no longer used is itself a violation, like an unfulfilled `#[expect]`.
//! - `cargo xtask ci`: the checks CI runs on every pull request, in order, stopping at the
//!   first failure.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context as _, Result, bail};

/// Crates that do I/O or need an async runtime; forbidden below L2 (architecture §2).
const IO_CRATES: [&str; 4] = ["tokio", "hyper", "reqwest", "rusqlite"];

/// The highest layer whose crates must stay free of I/O.
const LAST_PURE_LAYER: u8 = 1;

/// The highest numbered layer in the architecture.
const TOP_LAYER: u8 = 5;

fn main() -> ExitCode {
    match run() {
        Ok(Outcome::Passed) => ExitCode::SUCCESS,
        Ok(Outcome::Failed) => ExitCode::FAILURE,
        Err(error) => {
            let _ignored = writeln!(io::stderr(), "xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Whether a check passed; failures have already been reported to the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
}

fn run() -> Result<Outcome> {
    let command = std::env::args().nth(1);
    match command.as_deref() {
        Some("layers") => check_layers(&workspace_root()?),
        Some("ci") => run_ci(&workspace_root()?),
        _ => {
            writeln!(
                io::stderr(),
                "usage: cargo xtask <command>\n\n\
                 commands:\n  \
                 layers  check the layer rule and the I/O ban below L2\n  \
                 ci      fmt --check, clippy -D warnings, test, layers, deny (if installed)"
            )?;
            Ok(Outcome::Failed)
        }
    }
}

fn workspace_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask is not inside a workspace")
}

// ---------------------------------------------------------------------------------------------
// Layers

/// A crate's position in the architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    /// L0 to L5.
    Numbered(u8),
    /// Test and development tooling; may depend on anything, nothing may depend on it.
    Dev,
}

impl fmt::Display for Layer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Numbered(number) => write!(formatter, "L{number}"),
            Self::Dev => formatter.write_str("dev"),
        }
    }
}

/// How a dependency is used, as far as the layer rule cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DependencyKind {
    /// `[dependencies]` or `[build-dependencies]`, including target-specific tables.
    Production,
    /// `[dev-dependencies]`: tests may reach upwards (for example to pingers-testkit).
    Development,
}

/// One entry in a manifest's dependency tables.
#[derive(Clone, Debug)]
struct Dependency {
    /// The package name (after resolving `package = "..."` renames).
    package: String,
    kind: DependencyKind,
}

/// What the layer check needs to know about one workspace crate.
#[derive(Clone, Debug)]
struct CrateManifest {
    name: String,
    layer: Layer,
    dependencies: Vec<Dependency>,
}

/// A same-layer dependency the architecture relies on, written down in the workspace manifest
/// under `[workspace.metadata.pingers] same-layer` with a reason.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SameLayerException {
    dependent: String,
    dependency: String,
}

fn check_layers(root: &Path) -> Result<Outcome> {
    let crates = read_crate_manifests(&root.join("crates"))?;
    let by_name: BTreeMap<&str, &CrateManifest> = crates
        .iter()
        .map(|manifest| (manifest.name.as_str(), manifest))
        .collect();
    let exceptions = read_same_layer_exceptions(&root.join("Cargo.toml"))?;

    let mut violations = Vec::new();
    let mut ledger = ExceptionLedger {
        exceptions: &exceptions,
        used: Vec::new(),
    };
    for manifest in &crates {
        check_direct_dependencies(manifest, &by_name, &mut ledger, &mut violations);
    }
    for exception in &exceptions {
        if !ledger.used.contains(&exception) {
            violations.push(format!(
                "same-layer exception {} -> {} is no longer needed; remove it",
                exception.dependent, exception.dependency
            ));
        }
    }
    for manifest in &crates {
        if matches!(manifest.layer, Layer::Numbered(number) if number <= LAST_PURE_LAYER) {
            check_transitive_io(root, manifest, &mut violations)?;
        }
    }

    let mut stdout = io::stdout().lock();
    if violations.is_empty() {
        writeln!(
            stdout,
            "layers: {} crates checked, no violations",
            crates.len()
        )?;
        Ok(Outcome::Passed)
    } else {
        for violation in &violations {
            writeln!(stdout, "layers: {violation}")?;
        }
        writeln!(stdout, "layers: {} violation(s)", violations.len())?;
        Ok(Outcome::Failed)
    }
}

/// The same-layer exceptions, and which of them some dependency actually used.
struct ExceptionLedger<'exceptions> {
    exceptions: &'exceptions [SameLayerException],
    used: Vec<&'exceptions SameLayerException>,
}

fn check_direct_dependencies(
    manifest: &CrateManifest,
    by_name: &BTreeMap<&str, &CrateManifest>,
    ledger: &mut ExceptionLedger<'_>,
    violations: &mut Vec<String>,
) {
    for dependency in &manifest.dependencies {
        if dependency.kind == DependencyKind::Development {
            continue;
        }
        if let Layer::Numbered(number) = manifest.layer
            && number <= LAST_PURE_LAYER
            && IO_CRATES.contains(&dependency.package.as_str())
        {
            violations.push(format!(
                "{} ({}) depends on I/O crate {}",
                manifest.name, manifest.layer, dependency.package
            ));
        }
        let Some(target) = by_name.get(dependency.package.as_str()) else {
            continue;
        };
        let allowed = match (manifest.layer, target.layer) {
            (Layer::Dev, _) => true,
            (Layer::Numbered(_), Layer::Dev) => false,
            (Layer::Numbered(own), Layer::Numbered(theirs)) if theirs == own => {
                let exception = ledger.exceptions.iter().find(|exception| {
                    exception.dependent == manifest.name && exception.dependency == target.name
                });
                if let Some(exception) = exception {
                    ledger.used.push(exception);
                }
                exception.is_some()
            }
            (Layer::Numbered(own), Layer::Numbered(theirs)) => theirs < own,
        };
        if !allowed {
            violations.push(format!(
                "{} ({}) depends on {} ({}): a crate may depend only on lower layers",
                manifest.name, manifest.layer, target.name, target.layer
            ));
        }
    }
}

/// Uses `cargo tree` so that I/O crates pulled in by a third-party dependency are caught too.
/// `cargo tree -p` resolves features for that package alone, as `cargo build -p` would.
fn check_transitive_io(
    root: &Path,
    manifest: &CrateManifest,
    violations: &mut Vec<String>,
) -> Result<()> {
    let output = Command::new("cargo")
        .current_dir(root)
        .args(["tree", "--quiet", "--package", &manifest.name])
        .args([
            "--edges",
            "normal,build",
            "--target",
            "all",
            "--prefix",
            "none",
        ])
        .args(["--format", "{p}"])
        .output()
        .with_context(|| format!("running cargo tree for {}", manifest.name))?;
    if !output.status.success() {
        bail!(
            "cargo tree failed for {}: {}",
            manifest.name,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let tree = String::from_utf8(output.stdout).context("cargo tree printed non-UTF-8")?;
    let mut found: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|package| IO_CRATES.contains(package))
        .collect();
    found.sort_unstable();
    found.dedup();
    for package in found {
        violations.push(format!(
            "{} ({}) pulls in I/O crate {package} through its dependencies",
            manifest.name, manifest.layer
        ));
    }
    Ok(())
}

fn read_same_layer_exceptions(workspace_manifest: &Path) -> Result<Vec<SameLayerException>> {
    let text = fs::read_to_string(workspace_manifest)
        .with_context(|| format!("reading {}", workspace_manifest.display()))?;
    let table: toml::Table = toml::from_str(&text)
        .with_context(|| format!("parsing {}", workspace_manifest.display()))?;
    let Some(entries) = table
        .get("workspace")
        .and_then(|workspace| workspace.get("metadata"))
        .and_then(|metadata| metadata.get("pingers"))
        .and_then(|pingers| pingers.get("same-layer"))
    else {
        return Ok(Vec::new());
    };
    let entries = entries
        .as_array()
        .context("[workspace.metadata.pingers] same-layer must be an array")?;
    let mut exceptions = Vec::new();
    for entry in entries {
        let field = |name: &str| {
            entry
                .get(name)
                .and_then(toml::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .with_context(|| format!("same-layer entry {entry} needs a non-empty `{name}`"))
        };
        let dependent = field("crate")?;
        let dependency = field("dependency")?;
        field("reason")?;
        exceptions.push(SameLayerException {
            dependent,
            dependency,
        });
    }
    Ok(exceptions)
}

fn read_crate_manifests(crates_dir: &Path) -> Result<Vec<CrateManifest>> {
    let mut manifests = Vec::new();
    let entries =
        fs::read_dir(crates_dir).with_context(|| format!("reading {}", crates_dir.display()))?;
    for entry in entries {
        let path = entry?.path().join("Cargo.toml");
        if path.is_file() {
            manifests.push(read_crate_manifest(&path)?);
        }
    }
    manifests.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(manifests)
}

fn read_crate_manifest(path: &Path) -> Result<CrateManifest> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let table: toml::Table =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let package = table
        .get("package")
        .and_then(toml::Value::as_table)
        .with_context(|| format!("{}: no [package] table", path.display()))?;
    let name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .with_context(|| format!("{}: no package name", path.display()))?
        .to_owned();
    let layer = parse_layer(package)
        .with_context(|| format!("{name}: [package.metadata.pingers] layer"))?;

    let mut dependencies = Vec::new();
    collect_dependencies(&table, &mut dependencies);
    if let Some(targets) = table.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            collect_dependencies(target, &mut dependencies);
        }
    }
    Ok(CrateManifest {
        name,
        layer,
        dependencies,
    })
}

fn parse_layer(package: &toml::Table) -> Result<Layer> {
    let value = package
        .get("metadata")
        .and_then(|metadata| metadata.get("pingers"))
        .and_then(|pingers| pingers.get("layer"))
        .context("missing; declare `layer = N` (0 to 5) or `layer = \"dev\"`")?;
    if value.as_str() == Some("dev") {
        return Ok(Layer::Dev);
    }
    value
        .as_integer()
        .and_then(|number| u8::try_from(number).ok())
        .filter(|layer| *layer <= TOP_LAYER)
        .map(Layer::Numbered)
        .with_context(|| format!("{value} is not a layer; use 0 to {TOP_LAYER} or \"dev\""))
}

fn collect_dependencies(table: &toml::Table, dependencies: &mut Vec<Dependency>) {
    let sections = [
        ("dependencies", DependencyKind::Production),
        ("build-dependencies", DependencyKind::Production),
        ("dev-dependencies", DependencyKind::Development),
    ];
    for (section, kind) in sections {
        let Some(entries) = table.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        for (key, value) in entries {
            let package = value
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key)
                .to_owned();
            dependencies.push(Dependency { package, kind });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// CI

fn run_ci(root: &Path) -> Result<Outcome> {
    let cargo_steps: [&[&str]; 3] = [
        &["fmt", "--all", "--check"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--workspace", "--all-features"],
    ];
    for arguments in cargo_steps {
        if run_cargo(root, arguments)? == Outcome::Failed {
            return Ok(Outcome::Failed);
        }
    }

    announce("cargo xtask layers")?;
    if check_layers(root)? == Outcome::Failed {
        return Ok(Outcome::Failed);
    }

    let deny_installed = Command::new("cargo")
        .args(["deny", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());
    if deny_installed {
        if run_cargo(root, &["deny", "check"])? == Outcome::Failed {
            return Ok(Outcome::Failed);
        }
    } else {
        writeln!(
            io::stdout(),
            "ci: cargo-deny is not installed; skipping `cargo deny check`"
        )?;
    }

    writeln!(io::stdout(), "ci: all checks passed")?;
    Ok(Outcome::Passed)
}

fn run_cargo(root: &Path, arguments: &[&str]) -> Result<Outcome> {
    announce(&format!("cargo {}", arguments.join(" ")))?;
    let status = Command::new("cargo")
        .current_dir(root)
        .args(arguments)
        .status()
        .with_context(|| format!("running cargo {}", arguments.join(" ")))?;
    if status.success() {
        Ok(Outcome::Passed)
    } else {
        writeln!(io::stdout(), "ci: `cargo {}` failed", arguments.join(" "))?;
        Ok(Outcome::Failed)
    }
}

fn announce(step: &str) -> Result<()> {
    writeln!(io::stdout(), "ci: running `{step}`")?;
    Ok(())
}
