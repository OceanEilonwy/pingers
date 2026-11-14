// SPDX-License-Identifier: AGPL-3.0-only
//! Android `versionCode` and `versionName` from a release tag, so every build of a tag gets the
//! same numbers and a later tag always gets a larger code (Obtainium and Android refuse an
//! update whose code is not larger).
//!
//! A tag is `v<major>.<minor>.<patch>` with an optional pre-release `-alpha.<n>`, `-beta.<n>` or
//! `-rc.<n>` (`n` from 1 to 99); major is at most 209, minor and patch at most 99. Then
//!
//! ```text
//! versionCode = major * 10_000_000 + minor * 100_000 + patch * 1_000 + stage
//! stage       = alpha.n: n, beta.n: 100 + n, rc.n: 200 + n, final: 999
//! versionName = the tag without its `v`
//! ```
//!
//! so `v1.0.0-rc.1` (`10_000_201`) < `v1.0.0-rc.2` < `v1.0.0` (`10_000_999`) < `v1.0.1-beta.1`, and
//! the largest code, `2_099_999_999` for `v209.99.99`, stays under Android's `2_100_000_000`.
//!
//! `cargo xtask release-version <tag>` prints the numbers as `name=value` lines for
//! `$GITHUB_OUTPUT`, after checking the tag's code is larger than every other `v*` tag's in the
//! repository; `release.yml` runs it before building, and `release-android` runs the same check.

use std::fmt;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use crate::Outcome;

/// The largest major version whose codes fit under Android's limit.
const MAX_MAJOR: u32 = 209;
/// The largest minor or patch version, and pre-release number.
const MAX_PART: u32 = 99;

/// A pre-release stage, or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Alpha(u32),
    Beta(u32),
    ReleaseCandidate(u32),
    Final,
}

impl Stage {
    /// The last three digits of the code.
    fn code(self) -> u32 {
        match self {
            Self::Alpha(number) => number,
            Self::Beta(number) => number.saturating_add(100),
            Self::ReleaseCandidate(number) => number.saturating_add(200),
            Self::Final => 999,
        }
    }
}

/// Whether a GitHub Release for the tag is a pre-release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Prerelease {
    Yes,
    No,
}

impl fmt::Display for Prerelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Yes => "true",
            Self::No => "false",
        })
    }
}

/// The version a release tag stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseVersion {
    /// `versionName`: the tag without its `v`, as in the APK file names.
    pub(crate) name: String,
    /// `versionCode`.
    pub(crate) code: u32,
    pub(crate) stage: Stage,
}

impl ReleaseVersion {
    /// Parses a tag such as `v1.2.3` or `v1.0.0-rc.1`.
    pub(crate) fn parse(tag: &str) -> Result<Self> {
        let name = tag
            .strip_prefix('v')
            .with_context(|| format!("release tag {tag:?} does not start with v"))?;
        let (numbers, pre) = name
            .split_once('-')
            .map_or((name, None), |(core, pre)| (core, Some(pre)));
        let mut parts = numbers.split('.');
        let (Some(major), Some(minor), Some(patch), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            bail!("release tag {tag:?} is not v<major>.<minor>.<patch>[-alpha|beta|rc.<n>]");
        };
        let major = number(major, MAX_MAJOR, tag)?;
        let minor = number(minor, MAX_PART, tag)?;
        let patch = number(patch, MAX_PART, tag)?;
        let stage = match pre {
            None => Stage::Final,
            Some(pre) => {
                let (kind, count) = pre.split_once('.').with_context(|| {
                    format!(
                        "release tag {tag:?}: pre-release must be alpha.<n>, beta.<n> or rc.<n>"
                    )
                })?;
                let count = number(count, MAX_PART, tag)?;
                if count == 0 {
                    bail!("release tag {tag:?}: pre-release numbers start at 1");
                }
                match kind {
                    "alpha" => Stage::Alpha(count),
                    "beta" => Stage::Beta(count),
                    "rc" => Stage::ReleaseCandidate(count),
                    _ => bail!(
                        "release tag {tag:?}: pre-release must be alpha.<n>, beta.<n> or rc.<n>"
                    ),
                }
            }
        };
        let code = major
            .checked_mul(10_000_000)
            .and_then(|code| code.checked_add(minor.checked_mul(100_000)?))
            .and_then(|code| code.checked_add(patch.checked_mul(1_000)?))
            .and_then(|code| code.checked_add(stage.code()))
            .with_context(|| format!("release tag {tag:?} is out of range"))?;
        Ok(Self {
            name: name.to_owned(),
            code,
            stage,
        })
    }

    /// Whether the GitHub Release is a pre-release.
    pub(crate) fn prerelease(&self) -> Prerelease {
        if self.stage == Stage::Final {
            Prerelease::No
        } else {
            Prerelease::Yes
        }
    }

    /// The `$GITHUB_OUTPUT` lines.
    pub(crate) fn output_lines(&self) -> String {
        format!(
            "version_name={}\nversion_code={}\nprerelease={}\n",
            self.name,
            self.code,
            self.prerelease()
        )
    }
}

/// A decimal number without sign or leading zeros (other than `0` itself), at most `max`.
fn number(text: &str, max: u32, tag: &str) -> Result<u32> {
    let canonical = !text.is_empty()
        && text.chars().all(|character| character.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    let value = text.parse::<u32>().ok().filter(|_| canonical);
    match value {
        Some(value) if value <= max => Ok(value),
        _ => bail!("release tag {tag:?}: {text:?} is not a number from 0 to {max}"),
    }
}

/// Fails unless `version`'s code is larger than that of every tag in `others` other than
/// `tag` itself. Other tags that are not release tags are skipped.
pub(crate) fn check_increasing(
    tag: &str,
    version: &ReleaseVersion,
    others: &[String],
) -> Result<()> {
    for other in others.iter().filter(|other| other.as_str() != tag) {
        let Ok(earlier) = ReleaseVersion::parse(other) else {
            continue;
        };
        if earlier.code >= version.code {
            bail!(
                "release tag {tag} (versionCode {}) does not come after {other} (versionCode {}); \
                 a release tag must be newer than every earlier one",
                version.code,
                earlier.code
            );
        }
    }
    Ok(())
}

/// Every `v*` tag in the repository at `root`.
pub(crate) fn release_tags(root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["tag", "--list", "v*"])
        .output()
        .context("running git tag")?;
    if !output.status.success() {
        bail!(
            "git tag failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("git tag printed non-UTF-8")?
        .lines()
        .map(str::to_owned)
        .collect())
}

/// The version `tag` stands for, checked against the repository's other release tags.
pub(crate) fn checked(root: &Path, tag: &str) -> Result<ReleaseVersion> {
    let version = ReleaseVersion::parse(tag)?;
    check_increasing(tag, &version, &release_tags(root)?)?;
    Ok(version)
}

/// `cargo xtask release-version <tag>`.
pub(crate) fn run(root: &Path, tag: &str) -> Result<Outcome> {
    let version = checked(root, tag)?;
    io::stdout().write_all(version.output_lines().as_bytes())?;
    Ok(Outcome::Passed)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn code(tag: &str) -> Result<u32> {
        Ok(ReleaseVersion::parse(tag)?.code)
    }

    #[test]
    fn codes_follow_the_formula() -> Result<()> {
        assert_eq!(code("v1.0.0-alpha.1")?, 10_000_001);
        assert_eq!(code("v1.0.0-beta.2")?, 10_000_102);
        assert_eq!(code("v1.0.0-rc.1")?, 10_000_201);
        assert_eq!(code("v1.0.0")?, 10_000_999);
        assert_eq!(code("v0.1.0")?, 100_999);
        assert_eq!(code("v2.3.4-rc.99")?, 20_304_299);
        assert_eq!(code("v209.99.99")?, 2_099_999_999);
        Ok(())
    }

    #[test]
    fn codes_increase_in_release_order() -> Result<()> {
        let order = [
            "v0.9.9",
            "v1.0.0-alpha.1",
            "v1.0.0-alpha.99",
            "v1.0.0-beta.1",
            "v1.0.0-rc.1",
            "v1.0.0-rc.2",
            "v1.0.0",
            "v1.0.1-rc.1",
            "v1.0.1",
            "v1.1.0",
            "v2.0.0",
        ];
        let codes = order
            .iter()
            .map(|tag| code(tag))
            .collect::<Result<Vec<_>>>()?;
        assert!(codes.windows(2).all(|pair| matches!(pair, [a, b] if a < b)));
        Ok(())
    }

    #[test]
    fn names_and_prerelease_flags() -> Result<()> {
        let candidate = ReleaseVersion::parse("v1.0.0-rc.1")?;
        assert_eq!(candidate.name, "1.0.0-rc.1");
        assert_eq!(candidate.prerelease(), Prerelease::Yes);
        assert_eq!(
            candidate.output_lines(),
            "version_name=1.0.0-rc.1\nversion_code=10000201\nprerelease=true\n"
        );
        assert_eq!(
            ReleaseVersion::parse("v1.0.0")?.prerelease(),
            Prerelease::No
        );
        Ok(())
    }

    #[test]
    fn malformed_tags_are_refused() {
        for tag in [
            "1.0.0",
            "v1.0",
            "v1.0.0.0",
            "v01.0.0",
            "v1.0.0-rc",
            "v1.0.0-rc.0",
            "v1.0.0-rc.100",
            "v1.0.0-preview.1",
            "v1.100.0",
            "v210.0.0",
            "v1.0.-1",
            "v1.0.0+build",
            "feed-2026",
        ] {
            assert!(ReleaseVersion::parse(tag).is_err(), "{tag}");
        }
    }

    #[test]
    fn a_tag_must_come_after_every_other_release_tag() -> Result<()> {
        let others = ["v1.0.0-rc.1", "v1.0.0", "feed-1", "vnot-a-version"].map(str::to_owned);
        let next = ReleaseVersion::parse("v1.0.1")?;
        assert!(check_increasing("v1.0.1", &next, &others).is_ok());
        let backwards = ReleaseVersion::parse("v1.0.0-rc.2")?;
        assert!(check_increasing("v1.0.0-rc.2", &backwards, &others).is_err());
        // Its own tag, already pushed, does not count against it.
        let same = ReleaseVersion::parse("v1.0.0")?;
        assert!(check_increasing("v1.0.0", &same, &others).is_ok());
        Ok(())
    }
}
