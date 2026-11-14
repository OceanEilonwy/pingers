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
//!   Library crates may not depend on `anyhow` (style guide §4).
//! - `cargo xtask ci`: the checks CI runs on every pull request, in order, stopping at the
//!   first failure.
//! - `cargo xtask ci --full [--base <ref>] [--fuzz-seconds <n>]`: `ci`, then `cargo mutants`
//!   on the crates changed since `<ref>` (default `main`) and every fuzz target for `<n>`
//!   seconds (default 10). See [`full`].
//! - `cargo xtask vectors [--check]`: writes the generated vector files, or checks them. See
//!   [`vectors`].
//! - `cargo xtask no-python`: fails if a Python file is in the repository (also a `ci` step).
//!   See [`no_python`].
//! - `cargo xtask pins <server.toml> <push-relay.toml>`: the app's two key pin build
//!   variables from the deployed configurations; `cargo xtask pins --relay <url> --push <url>`:
//!   the same from the running services. See [`pins`].
//! - `cargo xtask release-android [--flavour gms|foss]... [--tag <v-tag>]`: the signed release
//!   build CI's `release.yml` runs, from the same environment variables, named for Obtainium.
//!   See [`release_android`]. `cargo xtask release-version <v-tag>` and `cargo xtask
//!   release-notes`: the version numbers and the GitHub Release body. See [`release_version`]
//!   and [`release_notes`].
//! - `cargo xtask nodo-dist [<commit>] [--nodo <checkout>]`: the vendored source tarball and
//!   filled-in xbps recipes for monokulo-nodo's builder; `cargo xtask dev-ca`: a throwaway CA
//!   for the tier-1 local stack. See [`nodo`].
//! - `cargo xtask local-stack`: the tier-1 stack (four services, ntfy, a TLS front) with fresh
//!   keys outside the repository, until Ctrl-C; `--second-server` adds a second server and
//!   relay for multi-server tests. See [`stack`] and [`tls_front`].
//! - `cargo xtask local-app`: the foss debug APK built against it. See [`local_app`].
//! - `cargo xtask e2e-emulators`: the two-emulator scenario from invite to the end of a Meetup.
//!   See [`scenario`].
#![forbid(unsafe_code)]

mod device;
mod emulator;
mod full;
mod local_app;
mod no_python;
mod nodo;
mod pins;
mod release_android;
mod release_notes;
mod release_version;
mod scenario;
mod stack;
mod supervise;
mod tls_front;
mod vectors;
mod wait;

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context as _, Result, bail};

/// Crates that do I/O or need an async runtime; forbidden below L2 (architecture §2).
const IO_CRATES: [&str; 5] = ["tokio", "hyper", "reqwest", "rusqlite", "sled"];

/// An I/O crate a pure crate may link because nothing in Pingers can reach it.
struct IoException {
    package: &'static str,
    io_crate: &'static str,
}

/// Each exception names its reason here and in `deny.toml`.
const IO_EXCEPTIONS: [IoException; 1] = [
    // rln 3.x depends on sled unconditionally on every non-wasm target; pingers-anon never
    // constructs rln's PmTree (it keeps its own in-memory tree), so no sled code runs.
    IoException {
        package: "pingers-anon",
        io_crate: "sled",
    },
];

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

const USAGE: &str = "usage: cargo xtask <command>\n\n\
                     commands:\n  \
                     layers  check the layer rule and the I/O ban below L2\n  \
                     ci      fmt --check, clippy -D warnings, test, layers, no-python, deny\n  \
                     ci --full [--base <ref>] [--fuzz-seconds <n>]\n          \
                     ci, then cargo mutants on crates changed since <ref> (main) and\n          \
                     every fuzz target for <n> seconds (10); survivors must be listed\n          \
                     with a reason in mutants-allow.toml\n  \
                     vectors [--check]\n          \
                     write vectors/proto/ and vectors/meetup/ from their generator, or\n          \
                     with --check fail if a committed file differs (cargo test checks too)\n  \
                     no-python\n          \
                     fail if the repository holds a Python file (extension or shebang)\n  \
                     pins <server.toml> <push-relay.toml>\n          \
                     print PINGERS_GATEWAY_KEY_PINS and PINGERS_PUSH_RELAY_KEY_PINS\n          \
                     from the deployed configurations (public values only)\n  \
                     pins --relay <ohttp relay url> --push <push relay url>\n          \
                     the same lines from the running services: the served gateway key\n          \
                     through the relay, the push relay's published and next keys\n  \
                     release-android [--flavour gms|foss]... [--tag <v-tag>]\n          \
                     signed release APKs and AABs from PINGERS_KEYSTORE_PATH,\n          \
                     PINGERS_KEYSTORE_PASSWORD and PINGERS_KEY_ALIAS, checked with\n          \
                     apksigner, versioned from the tag (default: HEAD's v* tag), copied\n          \
                     to target/release-android/pingers-<version>-<flavour>.apk|.aab\n  \
                     release-version <v-tag>\n          \
                     versionName, versionCode and prerelease for a tag, refusing one\n          \
                     that does not come after every other v* tag\n  \
                     release-notes --tag <v-tag> --dir <dir> [--repository <owner/name>]\n          \
                     the GitHub Release body for the files in <dir>, and its SHA256SUMS\n  \
                     nodo-dist [<commit>] [--nodo <checkout>]\n          \
                     vendored source tarball (prints its SHA-256) and filled-in xbps\n          \
                     recipes in target/nodo/ (docs/deploy-nodo.md)\n  \
                     dev-ca [--san <name>]...\n          \
                     throwaway CA and server certificate in target/dev-ca/\n  \
                     local-stack [--dir <dir>] [--packs <dir>] [--ntfy <bin>] [--caddy <bin>]\n          \
                     [--fresh] [--prepare-only] [--second-server]\n          \
                     the tier-1 stack with fresh keys in <dir> (default $PINGERS_LOCAL_STACK),\n          \
                     behind Caddy or the built-in TLS front, until Ctrl-C; --second-server\n          \
                     adds server B and its relay (https://10.0.2.2:9447, :9448/ohttp,\n          \
                     values in <dir>/second-server.env)\n  \
                     local-app [<local-stack options>]\n          \
                     the foss debug APK built against that stack (its app.env)\n  \
                     e2e-emulators [<local-stack options>] [--avd <name>] [--leases <dir>]\n          \
                     [--apk <file>] [--ntfy-apk <file>] [--keep-going] [--cross-server]\n          \
                     the two-emulator scenario (invite to Meetup end) against that stack\n  \
                     tls-front <caddy.json>\n          \
                     serve a caddy.json of the dev shape (local-stack runs it without Caddy)";

fn run() -> Result<Outcome> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (command, options) = arguments
        .split_first()
        .map_or((None, &[][..]), |(command, rest)| {
            (Some(command.as_str()), rest)
        });
    match command {
        Some("layers") if options.is_empty() => check_layers(&workspace_root()?),
        Some("ci") => match parse_ci_options(options) {
            Some(CiMode::Fast) => run_ci(&workspace_root()?),
            Some(CiMode::Full(full)) => {
                let root = workspace_root()?;
                if run_ci(&root)? == Outcome::Failed {
                    return Ok(Outcome::Failed);
                }
                let outcome = full::run_full(&root, &full)?;
                if outcome == Outcome::Passed {
                    writeln!(io::stdout(), "ci: full checks passed")?;
                }
                Ok(outcome)
            }
            None => usage(),
        },
        Some("vectors") => match vectors::Mode::parse(options) {
            Some(mode) => vectors::run(&workspace_root()?, mode),
            None => usage(),
        },
        Some("no-python") if options.is_empty() => no_python::run(&workspace_root()?),
        Some("pins") => {
            let output = match options {
                [server, push_relay] if !server.starts_with("--") => {
                    pins::run(Path::new(server), Path::new(push_relay))?
                }
                _ => match pins::LiveOptions::parse(options) {
                    Some(live) => pins::run_live(&live)?,
                    None => return usage(),
                },
            };
            io::stdout().write_all(output.as_bytes())?;
            Ok(Outcome::Passed)
        }
        Some("release-android") => match release_android::parse_options(options) {
            Some(choice) => release_android::run(&workspace_root()?, &choice),
            None => usage(),
        },
        Some("release-version") => match options {
            [tag] => release_version::run(&workspace_root()?, tag),
            _ => usage(),
        },
        Some("release-notes") => match release_notes::NotesOptions::parse(options) {
            Some(notes) => release_notes::run(&workspace_root()?, &notes),
            None => usage(),
        },
        Some("nodo-dist") => match nodo::parse_dist_options(options) {
            Some(dist) => nodo::nodo_dist(&workspace_root()?, &dist),
            None => usage(),
        },
        Some("dev-ca") => match nodo::parse_ca_options(options) {
            Some(names) => nodo::dev_ca(&workspace_root()?, &names),
            None => usage(),
        },
        Some("local-stack") => match stack::StackOptions::parse(options) {
            Some(stack) => stack::run(&workspace_root()?, &stack),
            None => usage(),
        },
        Some("local-app") => match stack::StackOptions::parse(options) {
            Some(stack) => local_app::run(&workspace_root()?, &stack),
            None => usage(),
        },
        Some("e2e-emulators") => match scenario::ScenarioOptions::parse(options) {
            Some(scenario) => scenario::run(&workspace_root()?, &scenario),
            None => usage(),
        },
        Some("tls-front") => match options {
            [config] => tls_front::run(Path::new(config)),
            _ => usage(),
        },
        _ => usage(),
    }
}

fn usage() -> Result<Outcome> {
    writeln!(io::stderr(), "{USAGE}")?;
    Ok(Outcome::Failed)
}

/// Which `ci` to run.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CiMode {
    /// The checks every pull request runs.
    Fast,
    /// Those, then mutation testing and fuzzing.
    Full(full::FullOptions),
}

/// Parses the options after `ci`; `None` for anything not understood.
fn parse_ci_options(options: &[String]) -> Option<CiMode> {
    let mut full = None::<full::FullOptions>;
    let mut options = options.iter();
    while let Some(option) = options.next() {
        match option.as_str() {
            "--full" => {
                full.get_or_insert_with(full::FullOptions::default);
            }
            "--base" => full.as_mut()?.base.clone_from(options.next()?),
            "--fuzz-seconds" => full.as_mut()?.fuzz_seconds = options.next()?.parse().ok()?,
            _ => return None,
        }
    }
    Some(full.map_or(CiMode::Fast, CiMode::Full))
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
    target: TargetKind,
    dependencies: Vec<Dependency>,
}

/// Whether a crate is a library (has `src/lib.rs`) or only a binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetKind {
    Library,
    Binary,
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
        if manifest.target == TargetKind::Library && dependency.package == "anyhow" {
            violations.push(format!(
                "{} is a library and depends on anyhow: use a thiserror enum (style guide §4)",
                manifest.name
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
        // Every feature: a crate's optional sides (pingers-anon's prover, verifier, issuer)
        // must be pure too.
        .args([
            "tree",
            "--quiet",
            "--all-features",
            "--package",
            &manifest.name,
        ])
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
        .filter(|package| {
            !IO_EXCEPTIONS.iter().any(|exception| {
                exception.package == manifest.name && exception.io_crate == *package
            })
        })
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
    let target = if path.with_file_name("src").join("lib.rs").is_file() {
        TargetKind::Library
    } else {
        TargetKind::Binary
    };
    Ok(CrateManifest {
        name,
        layer,
        target,
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

    announce("cargo xtask no-python")?;
    if no_python::run(root)? == Outcome::Failed {
        return Ok(Outcome::Failed);
    }

    // Not optional: the advisory ignores and the licence allow-list are only checked here.
    let deny_installed = Command::new("cargo")
        .args(["deny", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !deny_installed {
        writeln!(
            io::stdout(),
            "ci: cargo-deny is not installed (`cargo install --locked cargo-deny`)"
        )?;
        return Ok(Outcome::Failed);
    }
    if run_cargo(root, &["deny", "check"])? == Outcome::Failed {
        return Ok(Outcome::Failed);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(options: &[&str]) -> Option<CiMode> {
        let options: Vec<String> = options.iter().map(|option| (*option).to_owned()).collect();
        parse_ci_options(&options)
    }

    #[test]
    fn plain_ci_stays_fast_and_full_takes_options() {
        assert_eq!(parse(&[]), Some(CiMode::Fast));
        assert_eq!(
            parse(&["--full"]),
            Some(CiMode::Full(full::FullOptions::default()))
        );
        assert_eq!(
            parse(&["--full", "--base", "origin/main", "--fuzz-seconds", "3"]),
            Some(CiMode::Full(full::FullOptions {
                base: "origin/main".into(),
                fuzz_seconds: 3,
            }))
        );
    }

    #[test]
    fn options_without_full_or_a_value_are_refused() {
        assert_eq!(parse(&["--base", "main"]), None);
        assert_eq!(parse(&["--full", "--fuzz-seconds"]), None);
        assert_eq!(parse(&["--full", "--fuzz-seconds", "soon"]), None);
        assert_eq!(parse(&["--fast"]), None);
    }
}
