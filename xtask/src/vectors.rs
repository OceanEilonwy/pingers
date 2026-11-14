// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask vectors [--check]`: writes the generated vector files, `vectors/proto/*.json`
//! and `vectors/meetup/*.json`, or with `--check` (run by `cargo xtask ci`) fails if any
//! committed file differs from what the generator writes.
//!
//! The generators are deliberately independent of the crates the vectors test: the proto
//! files come from the CBOR writer in [`cbor`], not from pingers-proto's encoder, and the
//! meetup files from the formulas written out in [`meetup`], not from pingers-meetup. A vector
//! that both sides agree on was therefore computed twice.
//!
//! The other vector files (`vectors/crypto/`, `vectors/geo/`, `vectors/ohttp/` and so on) are
//! written by hand or by their crate's tests and are not touched here.

mod cbor;
mod json;
mod meetup;
mod proto;

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::Outcome;
use json::{Json, object};

/// What `cargo xtask vectors` does with the generated files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Write them into `vectors/`.
    Write,
    /// Compare them with the committed files and report every difference.
    Check,
}

impl Mode {
    /// Parses the options after `vectors`; `None` for anything not understood.
    pub(crate) fn parse(options: &[String]) -> Option<Self> {
        match options {
            [] => Some(Self::Write),
            [check] if check == "--check" => Some(Self::Check),
            _ => None,
        }
    }
}

/// One generated file: where it goes under the workspace root, and its exact contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VectorFile {
    pub(crate) path: PathBuf,
    pub(crate) contents: String,
}

/// Every generated file, proto first, each group in the order its generator writes them.
pub(crate) fn generate() -> Result<Vec<VectorFile>> {
    let mut files = proto::files()?;
    files.extend(meetup::files()?);
    Ok(files)
}

/// Runs `cargo xtask vectors` in `mode`.
pub(crate) fn run(root: &Path, mode: Mode) -> Result<Outcome> {
    let files = generate()?;
    let mut stdout = io::stdout().lock();
    match mode {
        Mode::Write => {
            for file in &files {
                let path = root.join(&file.path);
                fs::write(&path, &file.contents)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
            writeln!(stdout, "vectors: wrote {} files", files.len())?;
            Ok(Outcome::Passed)
        }
        Mode::Check => {
            let stale = stale_files(root, &files);
            if stale.is_empty() {
                writeln!(
                    stdout,
                    "vectors: {} generated files match the generator",
                    files.len()
                )?;
                return Ok(Outcome::Passed);
            }
            for path in &stale {
                writeln!(
                    stdout,
                    "vectors: {} differs from the generator",
                    path.display()
                )?;
            }
            writeln!(
                stdout,
                "vectors: {} stale file(s); edit xtask/src/vectors/ and run `cargo xtask vectors`",
                stale.len()
            )?;
            Ok(Outcome::Failed)
        }
    }
}

/// The generated files whose committed copy under `root` is missing or differs.
fn stale_files<'files>(root: &Path, files: &'files [VectorFile]) -> Vec<&'files Path> {
    files
        .iter()
        .filter(|file| {
            fs::read_to_string(root.join(&file.path))
                .map_or(true, |committed| committed != file.contents)
        })
        .map(|file| file.path.as_path())
        .collect()
}

/// A vector file: `{ name, description, vectors }` at `vectors/<directory>/<stem>.json`,
/// `name` being the loader's name for it.
fn vector_file(
    directory: &str,
    name: &str,
    description: &str,
    vectors: Vec<Json>,
) -> Result<VectorFile> {
    let stem = name.rsplit('/').next().context("empty vector file name")?;
    let document = object! {
        "name": name,
        "description": description,
        "vectors": vectors,
    };
    Ok(VectorFile {
        path: Path::new("vectors")
            .join(directory)
            .join(format!("{stem}.json")),
        contents: document.to_file_text(),
    })
}

/// One case: `{ name, inputs, expected }`.
fn case(name: &str, inputs: Json, expected: Json) -> Json {
    object! { "name": name, "inputs": inputs, "expected": expected }
}

/// The `expected` of a case that must be rejected with error variant `kind`.
fn error(kind: &str) -> Json {
    object! { "error": kind }
}

/// Bytes as the vector format writes them: lower-case hex.
fn hex(data: &[u8]) -> Json {
    Json::Text(hex::encode(data))
}

/// `count` copies of `byte`.
fn fill(byte: u8, count: usize) -> Vec<u8> {
    vec![byte; count]
}

/// `count` bytes counting up from the low byte of `start`, wrapping after 0xff.
fn ramp(start: usize, count: usize) -> Vec<u8> {
    let [first, ..] = start.to_le_bytes();
    std::iter::successors(Some(first), |byte| Some(byte.wrapping_add(1)))
        .take(count)
        .collect()
}

/// The first `len` bytes of `data`.
fn prefix(data: &[u8], len: usize) -> Result<Vec<u8>> {
    data.get(..len)
        .map(<[u8]>::to_vec)
        .with_context(|| format!("prefix of {len} bytes from {}", data.len()))
}

/// `data` from byte `start` on.
fn suffix(data: &[u8], start: usize) -> Result<Vec<u8>> {
    data.get(start..)
        .map(<[u8]>::to_vec)
        .with_context(|| format!("suffix from byte {start} of {}", data.len()))
}

/// `data` without its last byte.
fn drop_last(data: &[u8]) -> Result<Vec<u8>> {
    data.split_last()
        .map(|(_, rest)| rest.to_vec())
        .context("dropping the last byte of nothing")
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// The proof that the generator replaces the old scripts: every file it writes is the
    /// committed file, byte for byte.
    #[test]
    fn generated_files_match_the_committed_files() -> TestResult {
        let root = crate::workspace_root()?;
        let files = generate()?;
        assert_eq!(files.len(), 21);
        assert_eq!(stale_files(&root, &files), Vec::<&Path>::new());
        Ok(())
    }

    #[test]
    fn a_changed_or_missing_file_is_stale() -> TestResult {
        let root = std::env::temp_dir().join(format!("xtask-vectors-stale-{}", std::process::id()));
        fs::create_dir_all(root.join("vectors"))?;
        let file = |name: &str, contents: &str| VectorFile {
            path: Path::new("vectors").join(name),
            contents: contents.to_owned(),
        };
        let files = [
            file("same.json", "{}\n"),
            file("changed.json", "{}\n"),
            file("missing.json", "{}\n"),
        ];
        fs::write(root.join("vectors/same.json"), "{}\n")?;
        fs::write(root.join("vectors/changed.json"), "{ }\n")?;
        let _absent = fs::remove_file(root.join("vectors/missing.json"));
        assert_eq!(
            stale_files(&root, &files),
            [
                Path::new("vectors/changed.json"),
                Path::new("vectors/missing.json")
            ]
        );
        Ok(())
    }

    #[test]
    fn every_file_in_the_generated_directories_is_generated() -> TestResult {
        let root = crate::workspace_root()?;
        let files = generate()?;
        for directory in ["proto", "meetup"] {
            for entry in fs::read_dir(root.join("vectors").join(directory))? {
                let path = entry?.path();
                let relative = path.strip_prefix(&root)?;
                assert!(
                    files.iter().any(|file| file.path == relative),
                    "{} is not generated",
                    relative.display()
                );
            }
        }
        Ok(())
    }

    #[test]
    fn ramp_wraps_and_starts_from_the_low_byte() {
        assert_eq!(ramp(0xfe, 3), [0xfe, 0xff, 0x00]);
        assert_eq!(ramp(288, 2), [0x20, 0x21]);
        assert_eq!(ramp(5, 0), Vec::<u8>::new());
    }

    #[test]
    fn slices_refuse_out_of_range() {
        assert!(prefix(&[1, 2], 3).is_err());
        assert!(suffix(&[1, 2], 3).is_err());
        assert!(drop_last(&[]).is_err());
        assert!(matches!(prefix(&[1, 2, 3], 2), Ok(bytes) if bytes == [1, 2]));
        assert!(matches!(suffix(&[1, 2, 3], 1), Ok(bytes) if bytes == [2, 3]));
        assert!(matches!(drop_last(&[1, 2, 3]), Ok(bytes) if bytes == [1, 2]));
    }

    #[test]
    fn check_mode_needs_exactly_the_flag() {
        let parse = |options: &[&str]| {
            Mode::parse(
                &options
                    .iter()
                    .map(|option| (*option).to_owned())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(parse(&[]), Some(Mode::Write));
        assert_eq!(parse(&["--check"]), Some(Mode::Check));
        assert_eq!(parse(&["--check", "x"]), None);
        assert_eq!(parse(&["--write"]), None);
    }
}
