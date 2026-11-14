// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask no-python`, also a step of `cargo xtask ci`: the repository holds no Python.
//! Tools are written in Rust (generators as `cargo xtask` commands), so a Python file is a
//! mistake to catch before review.
//!
//! The files checked are the ones git knows about or would add: tracked files plus untracked
//! files that are not ignored, so `target/` and other build output are skipped but a new
//! script is caught before it is committed. A file counts as Python if its name ends in a
//! Python extension or its first line is a shebang naming a Python interpreter.

use std::fs::File;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use crate::Outcome;

/// File name endings that mean Python source, bytecode or notebooks.
const PYTHON_EXTENSIONS: [&str; 6] = [".py", ".pyw", ".pyi", ".pyc", ".pyx", ".ipynb"];

/// Runs the check over the repository at `root`.
pub(crate) fn run(root: &Path) -> Result<Outcome> {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .context("running git ls-files")?;
    if !output.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let listing = String::from_utf8(output.stdout).context("git ls-files printed non-UTF-8")?;
    let mut found = Vec::new();
    for name in listing.split('\0').filter(|name| !name.is_empty()) {
        let path = root.join(name);
        // A tracked file deleted in the working tree is not there to hold Python.
        if !path.is_file() {
            continue;
        }
        if is_python(name, &path)? {
            found.push(name);
        }
    }
    let mut stdout = io::stdout().lock();
    if found.is_empty() {
        writeln!(stdout, "no-python: no Python files")?;
        return Ok(Outcome::Passed);
    }
    for name in &found {
        writeln!(
            stdout,
            "no-python: {name} is Python; write tools in Rust (a `cargo xtask` command)"
        )?;
    }
    Ok(Outcome::Failed)
}

/// Whether the file `name` at `path` is Python, by extension or shebang.
fn is_python(name: &str, path: &Path) -> Result<bool> {
    let lower = name.to_ascii_lowercase();
    if PYTHON_EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension))
    {
        return Ok(true);
    }
    let file = File::open(path).with_context(|| format!("opening {name}"))?;
    let mut first_line = Vec::new();
    // 256 bytes is more than any shebang needs.
    BufReader::new(file.take(256))
        .read_until(b'\n', &mut first_line)
        .with_context(|| format!("reading {name}"))?;
    Ok(is_python_shebang(&first_line))
}

/// Whether a first line is `#!` followed by a command naming a Python interpreter
/// (`/usr/bin/python3`, `/usr/bin/env python`, `/usr/bin/env -S python3 -u`).
fn is_python_shebang(first_line: &[u8]) -> bool {
    let Some(command) = first_line.strip_prefix(b"#!") else {
        return false;
    };
    String::from_utf8_lossy(command)
        .split_whitespace()
        .filter_map(|word| word.rsplit('/').next())
        .any(|program| program.starts_with("python"))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn shebangs_naming_python_are_python() {
        assert!(is_python_shebang(b"#!/usr/bin/python3\n"));
        assert!(is_python_shebang(b"#!/usr/bin/env python\n"));
        assert!(is_python_shebang(b"#!/usr/bin/env -S python3.12 -u\n"));
        assert!(!is_python_shebang(b"#!/bin/sh\n"));
        assert!(!is_python_shebang(b"#!/usr/bin/env bash\n"));
        assert!(!is_python_shebang(b"# python3 is not used here\n"));
        assert!(!is_python_shebang(b""));
    }

    #[test]
    fn python_is_found_by_extension_or_shebang() -> TestResult {
        let directory =
            std::env::temp_dir().join(format!("xtask-no-python-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let write = |name: &str, text: &str| -> io::Result<std::path::PathBuf> {
            let path = directory.join(name);
            std::fs::write(&path, text)?;
            Ok(path)
        };
        let script = write("generate", "#!/usr/bin/env python3\nprint(1)\n")?;
        let module = write("Helper.PY", "x = 1\n")?;
        let shell = write("run", "#!/bin/sh\necho python\n")?;
        let notes = write("notes.md", "python3 vectors/generate.py\n")?;
        assert!(is_python("generate", &script)?);
        assert!(is_python("Helper.PY", &module)?);
        assert!(!is_python("run", &shell)?);
        assert!(!is_python("notes.md", &notes)?);
        Ok(())
    }
}
