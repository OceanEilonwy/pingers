// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask ci --full`: the slow checks on top of `ci`.
//!
//! - `cargo mutants` on every crate changed relative to the base branch (`main` unless
//!   `--base <ref>` says otherwise), working-tree changes included. A surviving or timed-out
//!   mutant fails the run unless `mutants-allow.toml` lists it with a reason. An allow entry
//!   for a crate that was mutated in this run but that no longer survives also fails, like an
//!   unfulfilled `#[expect]`, so the file cannot go stale.
//! - Every fuzz target under `crates/*/fuzz` for a short time box (`--fuzz-seconds <n>`,
//!   default 10), with `cargo +nightly fuzz run`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use crate::{Outcome, announce};

/// The allow-file of accepted mutation survivors, at the workspace root.
pub(crate) const ALLOW_FILE: &str = "mutants-allow.toml";

/// Options for the slow checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FullOptions {
    /// The branch or commit changes are measured against.
    pub(crate) base: String,
    /// How long each fuzz target runs.
    pub(crate) fuzz_seconds: u32,
}

impl Default for FullOptions {
    fn default() -> Self {
        Self {
            base: "main".into(),
            fuzz_seconds: 10,
        }
    }
}

/// Runs mutation testing on changed crates, then every fuzz target.
pub(crate) fn run_full(root: &Path, options: &FullOptions) -> Result<Outcome> {
    let allowed = read_allow_file(&root.join(ALLOW_FILE))?;
    let changed = changed_crates(root, &options.base)?;
    let mut outcome = Outcome::Passed;
    if changed.is_empty() {
        writeln!(
            io::stdout(),
            "ci: no crate changed relative to {}",
            options.base
        )?;
    }
    for package in &changed {
        if mutate(root, package, &allowed)? == Outcome::Failed {
            outcome = Outcome::Failed;
        }
    }
    if fuzz(root, options.fuzz_seconds)? == Outcome::Failed {
        outcome = Outcome::Failed;
    }
    Ok(outcome)
}

/// Accepted survivors, by crate directory name: normalised mutant name to reason.
type Allowed = BTreeMap<String, BTreeMap<String, String>>;

/// Reads `mutants-allow.toml`: `[[mutant]]` tables with a `name` (as `cargo mutants` prints
/// it, without line and column) and a non-empty `reason`.
fn read_allow_file(path: &Path) -> Result<Allowed> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Allowed::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    parse_allow_file(&text).with_context(|| format!("in {}", path.display()))
}

fn parse_allow_file(text: &str) -> Result<Allowed> {
    let table: toml::Table = text.parse()?;
    let mut allowed = Allowed::new();
    let entries = match table.get("mutant") {
        None => return Ok(allowed),
        Some(toml::Value::Array(entries)) => entries,
        Some(_) => bail!("`mutant` must be an array of tables (`[[mutant]]`)"),
    };
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .context("a [[mutant]] entry has no `name`")?;
        let reason = entry
            .get("reason")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if reason.is_empty() {
            bail!("the allowed mutant `{name}` has no reason");
        }
        let package = crate_of(name)
            .with_context(|| format!("the allowed mutant `{name}` is not under crates/<name>/"))?;
        allowed
            .entry(package)
            .or_default()
            .insert(normalise(name), reason.to_owned());
    }
    Ok(allowed)
}

/// The crate directory a path under `crates/` belongs to.
fn crate_of(path: &str) -> Option<String> {
    let rest = path.strip_prefix("crates/")?;
    let (name, _) = rest.split_once('/')?;
    Some(name.to_owned())
}

/// A mutant's name without its line and column, so the allow-file survives unrelated edits:
/// `crates/a/src/b.rs:12:5: replace x` becomes `crates/a/src/b.rs: replace x`.
fn normalise(mutant: &str) -> String {
    let Some((path, rest)) = mutant.split_once(':') else {
        return mutant.trim().to_owned();
    };
    let mut rest = rest;
    for _ in 0..2 {
        if let Some((number, tail)) = rest.split_once(':')
            && !number.is_empty()
            && number.chars().all(|character| character.is_ascii_digit())
        {
            rest = tail;
        }
    }
    format!("{path}:{}", rest.trim_end())
}

/// Crate directories with a file changed between the merge base with `base` and the working
/// tree, that are workspace members (fuzz crates and vendored code are not).
fn changed_crates(root: &Path, base: &str) -> Result<BTreeSet<String>> {
    let merge_base = git(root, &["merge-base", base, "HEAD"])?;
    let mut changed = BTreeSet::new();
    let diff = git(root, &["diff", "--name-only", merge_base.trim()])?;
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard"])?;
    for path in diff.lines().chain(untracked.lines()) {
        let Some(name) = crate_of(path) else {
            continue;
        };
        let in_fuzz_crate = path
            .strip_prefix(&format!("crates/{name}/"))
            .is_some_and(|inner| inner.starts_with("fuzz/"));
        if !in_fuzz_crate && root.join("crates").join(&name).join("Cargo.toml").is_file() {
            changed.insert(name);
        }
    }
    Ok(changed)
}

fn git(root: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .with_context(|| format!("running git {}", arguments.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("git printed something that is not UTF-8")
}

/// Runs `cargo mutants` on one crate and checks its survivors against the allow-file.
fn mutate(root: &Path, package: &str, allowed: &Allowed) -> Result<Outcome> {
    let output = root.join("target").join("xtask-mutants").join(package);
    let output_text = output.to_string_lossy().into_owned();
    let arguments = [
        "mutants",
        "--package",
        package,
        "--all-features",
        "--no-shuffle",
        "--output",
        output_text.as_str(),
    ];
    announce(&format!("cargo {}", arguments.join(" ")))?;
    let status = Command::new("cargo")
        .current_dir(root)
        .args(arguments)
        .status()
        .context("running cargo mutants (`cargo install --locked cargo-mutants`)")?;
    // 0: all caught; 2: some missed; 3: some timed out. Anything else (a broken baseline, a
    // usage error) means the results cannot be trusted.
    if !matches!(status.code(), Some(0 | 2 | 3)) {
        writeln!(io::stdout(), "ci: cargo mutants failed for {package}")?;
        return Ok(Outcome::Failed);
    }
    let results = output.join("mutants.out");
    let mut survivors = BTreeSet::new();
    for file in ["missed.txt", "timeout.txt"] {
        survivors.extend(
            read_lines(&results.join(file))?
                .iter()
                .map(|line| normalise(line)),
        );
    }
    let empty = BTreeMap::new();
    let accepted = allowed.get(package).unwrap_or(&empty);
    report_survivors(package, &survivors, accepted)
}

/// Fails on a survivor the allow-file does not list, and on an allow entry that no longer
/// survives.
fn report_survivors(
    package: &str,
    survivors: &BTreeSet<String>,
    accepted: &BTreeMap<String, String>,
) -> Result<Outcome> {
    let mut outcome = Outcome::Passed;
    for survivor in survivors {
        if !accepted.contains_key(survivor) {
            writeln!(
                io::stdout(),
                "ci: surviving mutant in {package}: {survivor}"
            )?;
            outcome = Outcome::Failed;
        }
    }
    for name in accepted.keys() {
        if !survivors.contains(name) {
            writeln!(
                io::stdout(),
                "ci: {ALLOW_FILE} lists `{name}`, which no longer survives; remove it"
            )?;
            outcome = Outcome::Failed;
        }
    }
    if outcome == Outcome::Failed {
        writeln!(
            io::stdout(),
            "ci: add a test that kills each new survivor, or list it in {ALLOW_FILE} with a reason"
        )?;
    }
    Ok(outcome)
}

fn read_lines(path: &Path) -> Result<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// The crates with a `fuzz/` directory.
fn fuzzed_crates(root: &Path) -> Result<Vec<PathBuf>> {
    let mut crates = Vec::new();
    for entry in fs::read_dir(root.join("crates")).context("listing crates/")? {
        let path = entry?.path();
        if path.join("fuzz").join("Cargo.toml").is_file() {
            crates.push(path);
        }
    }
    crates.sort();
    Ok(crates)
}

/// Runs each fuzz target for `seconds`.
fn fuzz(root: &Path, seconds: u32) -> Result<Outcome> {
    let time_box = format!("-max_total_time={seconds}");
    let mut outcome = Outcome::Passed;
    for crate_dir in fuzzed_crates(root)? {
        let listed = Command::new("cargo")
            .current_dir(&crate_dir)
            .args(["+nightly", "fuzz", "list"])
            .output()
            .context("running cargo +nightly fuzz list (`cargo install cargo-fuzz`)")?;
        if !listed.status.success() {
            writeln!(
                io::stdout(),
                "ci: cargo fuzz list failed in {}",
                crate_dir.display()
            )?;
            outcome = Outcome::Failed;
            continue;
        }
        for target in String::from_utf8_lossy(&listed.stdout).lines() {
            let target = target.trim();
            if target.is_empty() {
                continue;
            }
            let arguments = ["+nightly", "fuzz", "run", target, "--", time_box.as_str()];
            announce(&format!(
                "cargo {} (in {})",
                arguments.join(" "),
                crate_dir.display()
            ))?;
            let status = Command::new("cargo")
                .current_dir(&crate_dir)
                .args(arguments)
                .status()
                .with_context(|| format!("running fuzz target {target}"))?;
            if !status.success() {
                writeln!(io::stdout(), "ci: fuzz target {target} failed")?;
                outcome = Outcome::Failed;
            }
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn mutant_names_lose_line_and_column() {
        assert_eq!(
            normalise(
                "crates/pingers-geo/src/measure.rs:36:9: replace Meters::constant -> Self with Default::default()"
            ),
            "crates/pingers-geo/src/measure.rs: replace Meters::constant -> Self with Default::default()"
        );
        assert_eq!(
            normalise("crates/a/src/b.rs: replace x with y"),
            "crates/a/src/b.rs: replace x with y"
        );
        // A colon inside the description is kept.
        assert_eq!(
            normalise("crates/a/src/b.rs:1:2: replace a::b with c"),
            "crates/a/src/b.rs: replace a::b with c"
        );
    }

    #[test]
    fn allow_entries_need_a_reason_and_a_crate() -> Result<()> {
        let allowed = parse_allow_file(
            "[[mutant]]\nname = \"crates/a/src/b.rs:3:4: replace x\"\nreason = \"equivalent\"\n",
        )?;
        assert_eq!(
            allowed
                .get("a")
                .and_then(|entries| entries.get("crates/a/src/b.rs: replace x"))
                .map(String::as_str),
            Some("equivalent")
        );
        assert!(parse_allow_file("[[mutant]]\nname = \"crates/a/src/b.rs: x\"\n").is_err());
        assert!(
            parse_allow_file("[[mutant]]\nname = \"crates/a/src/b.rs: x\"\nreason = \" \"\n")
                .is_err()
        );
        assert!(parse_allow_file("[[mutant]]\nname = \"src/b.rs: x\"\nreason = \"r\"\n").is_err());
        assert!(parse_allow_file("").map(|allowed| allowed.is_empty())?);
        Ok(())
    }

    #[test]
    fn unlisted_survivors_and_stale_entries_both_fail() -> Result<()> {
        let survivor = "crates/a/src/b.rs: replace x".to_owned();
        let listed = BTreeMap::from([(survivor.clone(), "equivalent".to_owned())]);
        let survivors = BTreeSet::from([survivor]);
        assert_eq!(report_survivors("a", &survivors, &listed)?, Outcome::Passed);
        assert_eq!(
            report_survivors("a", &survivors, &BTreeMap::new())?,
            Outcome::Failed
        );
        assert_eq!(
            report_survivors("a", &BTreeSet::new(), &listed)?,
            Outcome::Failed
        );
        Ok(())
    }

    #[test]
    fn paths_map_to_their_crate() {
        assert_eq!(
            crate_of("crates/pingers-geo/src/lib.rs").as_deref(),
            Some("pingers-geo")
        );
        assert_eq!(crate_of("xtask/src/main.rs"), None);
        assert_eq!(crate_of("crates/README.md"), None);
    }

    /// A scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Result<Self> {
            let path = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        fn write(&self, relative: &str, text: &str) -> Result<()> {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, text)?;
            Ok(())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_files_read_as_empty_and_blank_lines_are_skipped() -> Result<()> {
        let scratch = Scratch::new("files")?;
        assert!(read_lines(&scratch.0.join("absent.txt"))?.is_empty());
        assert!(read_allow_file(&scratch.0.join("absent.toml"))?.is_empty());
        scratch.write(
            "missed.txt",
            "crates/a/src/b.rs:1:2: one\n\n  \ncrates/a/src/b.rs:3:4: two\n",
        )?;
        assert_eq!(
            read_lines(&scratch.0.join("missed.txt"))?,
            ["crates/a/src/b.rs:1:2: one", "crates/a/src/b.rs:3:4: two"]
        );
        scratch.write(
            "allow.toml",
            "[[mutant]]\nname = \"crates/a/src/b.rs: one\"\nreason = \"equivalent\"\n",
        )?;
        assert_eq!(read_allow_file(&scratch.0.join("allow.toml"))?.len(), 1);
        // A directory is not a readable file: an error, not an empty list.
        assert!(read_lines(&scratch.0).is_err());
        assert!(read_allow_file(&scratch.0).is_err());
        Ok(())
    }

    #[test]
    fn only_crates_with_a_fuzz_manifest_are_fuzzed() -> Result<()> {
        let scratch = Scratch::new("fuzzed")?;
        scratch.write("crates/b/fuzz/Cargo.toml", "[package]\n")?;
        scratch.write("crates/a/fuzz/Cargo.toml", "[package]\n")?;
        scratch.write("crates/c/fuzz/README.md", "\n")?;
        scratch.write("crates/d/src/lib.rs", "\n")?;
        assert_eq!(
            fuzzed_crates(&scratch.0)?,
            [scratch.0.join("crates/a"), scratch.0.join("crates/b")]
        );
        Ok(())
    }

    #[test]
    fn crates_with_changes_since_the_base_are_found() -> Result<()> {
        let scratch = Scratch::new("changes")?;
        let root = scratch.0.as_path();
        for name in ["kept", "edited", "fuzzed", "added"] {
            scratch.write(&format!("crates/{name}/Cargo.toml"), "[package]\n")?;
            scratch.write(&format!("crates/{name}/src/lib.rs"), "\n")?;
        }
        scratch.write("crates/fuzzed/fuzz/Cargo.toml", "[package]\n")?;
        for arguments in [
            &["init", "--quiet", "--initial-branch", "base"][..],
            &["add", "."],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "-m",
                "base",
            ],
        ] {
            git(root, arguments)?;
        }
        scratch.write("crates/edited/src/lib.rs", "// changed\n")?;
        scratch.write("crates/fuzzed/fuzz/target.rs", "\n")?;
        scratch.write("crates/added/src/new.rs", "\n")?;
        scratch.write("xtask/src/main.rs", "\n")?;
        let changed = changed_crates(root, "base")?;
        assert_eq!(
            changed.into_iter().collect::<Vec<_>>(),
            ["added", "edited"],
            "fuzz-only changes and paths outside crates/ are not crates"
        );
        assert!(git(root, &["rev-parse", "no-such-ref"]).is_err());
        Ok(())
    }
}
