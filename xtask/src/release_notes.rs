// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask release-notes --tag <tag> --dir <dir> [--repository <owner/name>]`: the body of
//! a GitHub Release, printed to stdout, and `<dir>/SHA256SUMS` for its assets.
//!
//! `<dir>` holds the files `release-android` named (`pingers-<version>-<flavour>.apk` and
//! `.aab`). The body has the release notes (the tag's annotation, or else the commit subjects
//! since the previous release tag), which APK to install, the upload certificate's SHA-256 for
//! `AppVerifier`, and every asset's SHA-256. `release.yml` runs it in its publish job; it runs the
//! same way on a developer's machine over `target/release-android/`.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use sha2::{Digest as _, Sha256};

use crate::Outcome;
use crate::release_android::{UPLOAD_CERTIFICATE_SHA256, colon_hex};
use crate::release_version::{Prerelease, ReleaseVersion, release_tags};

/// Parsed options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotesOptions {
    tag: String,
    dir: PathBuf,
    repository: Option<String>,
}

impl NotesOptions {
    /// Parses `--tag <tag> --dir <dir> [--repository <owner/name>]`; `None` otherwise.
    pub(crate) fn parse(options: &[String]) -> Option<Self> {
        let (mut tag, mut dir, mut repository) = (None, None, None);
        let mut options = options.iter();
        while let Some(option) = options.next() {
            let slot = match option.as_str() {
                "--tag" => &mut tag,
                "--dir" => &mut dir,
                "--repository" => &mut repository,
                _ => return None,
            };
            if slot.is_some() {
                return None;
            }
            *slot = Some(options.next()?.clone());
        }
        Some(Self {
            tag: tag?,
            dir: PathBuf::from(dir?),
            repository,
        })
    }
}

/// One release asset and its digest.
struct Asset {
    name: String,
    sha256: String,
}

/// Writes `SHA256SUMS` and prints the body.
pub(crate) fn run(root: &Path, options: &NotesOptions) -> Result<Outcome> {
    let version = ReleaseVersion::parse(&options.tag)?;
    let assets = assets(&options.dir)?;
    let sums = assets.iter().fold(String::new(), |mut sums, asset| {
        sums.push_str(&asset.sha256);
        sums.push_str("  ");
        sums.push_str(&asset.name);
        sums.push('\n');
        sums
    });
    fs::write(options.dir.join("SHA256SUMS"), &sums)
        .with_context(|| format!("writing SHA256SUMS in {}", options.dir.display()))?;
    let notes = notes(root, &options.tag, &version)?;
    let body = body(&BodyParts {
        version: &version,
        notes: &notes,
        repository: options.repository.as_deref(),
        tag: &options.tag,
        sums: &sums,
    });
    io::stdout().write_all(body.as_bytes())?;
    Ok(Outcome::Passed)
}

/// The APKs and AABs in `dir`, by name, with their SHA-256.
fn assets(dir: &Path) -> Result<Vec<Asset>> {
    let mut assets = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let is_asset = path
            .extension()
            .is_some_and(|extension| extension == "apk" || extension == "aab");
        if !is_asset {
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .context("an asset without a file name")?;
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        assets.push(Asset {
            name,
            sha256: hex::encode(Sha256::digest(&bytes)),
        });
    }
    if !assets.iter().any(|asset| {
        Path::new(&asset.name)
            .extension()
            .is_some_and(|extension| extension == "apk")
    }) {
        bail!("no .apk in {}", dir.display());
    }
    assets.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(assets)
}

/// The tag's annotation if it has one, else the commit subjects since the previous release
/// tag (by version code), else a first-release line.
fn notes(root: &Path, tag: &str, version: &ReleaseVersion) -> Result<String> {
    let reference = format!("refs/tags/{tag}");
    let kind = git(
        root,
        &["for-each-ref", "--format=%(objecttype)", &reference],
    )?;
    if kind.trim() == "tag" {
        let annotation = git(root, &["for-each-ref", "--format=%(contents)", &reference])?;
        // A signed tag's signature is not part of the notes.
        let message = annotation
            .split("-----BEGIN")
            .next()
            .unwrap_or_default()
            .trim();
        if !message.is_empty() {
            return Ok(message.to_owned());
        }
    }
    let previous = release_tags(root)?
        .into_iter()
        .filter_map(|other| {
            let earlier = ReleaseVersion::parse(&other).ok()?;
            (earlier.code < version.code).then_some((earlier.code, other))
        })
        .max_by_key(|(code, _)| *code)
        .map(|(_, other)| other);
    let Some(previous) = previous else {
        return Ok("The first release.".to_owned());
    };
    let range = format!("{previous}..{tag}");
    let subjects = git(root, &["log", "--no-merges", "--format=- %s", &range])?;
    Ok(format!(
        "Changes since {previous}:\n\n{}",
        subjects.trim_end()
    ))
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
    String::from_utf8(output.stdout).context("git printed non-UTF-8")
}
/// What the release body is made from.
struct BodyParts<'parts> {
    version: &'parts ReleaseVersion,
    notes: &'parts str,
    /// `owner/name` on GitHub, for a link to the install guide at this tag.
    repository: Option<&'parts str>,
    tag: &'parts str,
    /// The `SHA256SUMS` text.
    sums: &'parts str,
}

/// The release body in Markdown.
fn body(parts: &BodyParts<'_>) -> String {
    let BodyParts {
        version,
        notes,
        repository,
        tag,
        sums,
    } = parts;
    let name = &version.name;
    let install = repository.map_or_else(
        || "`docs/install.md` in the repository".to_owned(),
        |repository| {
            format!("[docs/install.md](https://github.com/{repository}/blob/{tag}/docs/install.md)")
        },
    );
    let prerelease = match version.prerelease() {
        Prerelease::Yes => {
            "A pre-release for testing. Obtainium offers it only with \"Include prereleases\" on.\n\n"
        }
        Prerelease::No => "",
    };
    let certificate = colon_hex(UPLOAD_CERTIFICATE_SHA256);
    format!(
        "# Pingers {name}\n\n\
         {prerelease}\
         {notes}\n\n\
         ## Install and update\n\n\
         Install with [Obtainium](https://github.com/ImranR98/Obtainium) and it updates from these \
         releases; {install} explains how.\n\n\
         - `pingers-{name}-gms.apk`: phones with Google Play services (FCM and UnifiedPush).\n\
         - `pingers-{name}-foss.apk`: phones without Google Play services; no Google code, \
         UnifiedPush only.\n\
         - The `.aab` files are for app stores; a phone cannot install them.\n\n\
         ## Signing certificate\n\n\
         Both APKs (`org.pingers.app` for gms, `org.pingers.app.foss` for foss) are signed by the \
         certificate with this SHA-256. \
         Check it with AppVerifier, or `apksigner verify --print-certs <apk>`:\n\n\
         ```\n{certificate}\n```\n\n\
         ## SHA-256 checksums\n\n\
         Also attached as `SHA256SUMS` (`sha256sum -c SHA256SUMS`):\n\n\
         ```\n{sums}```\n"
    )
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn options_need_a_tag_and_a_directory() {
        let parse = |options: &[&str]| {
            NotesOptions::parse(
                &options
                    .iter()
                    .map(|option| (*option).to_owned())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            parse(&["--dir", "dist", "--tag", "v1.0.0", "--repository", "o/p"]),
            Some(NotesOptions {
                tag: "v1.0.0".to_owned(),
                dir: PathBuf::from("dist"),
                repository: Some("o/p".to_owned()),
            })
        );
        assert_eq!(parse(&["--tag", "v1.0.0"]), None);
        assert_eq!(parse(&["--tag", "v1", "--tag", "v2", "--dir", "d"]), None);
        assert_eq!(parse(&["--tag", "v1.0.0", "--dir"]), None);
        assert_eq!(parse(&["--tag", "v1.0.0", "--dir", "d", "--x", "y"]), None);
    }

    #[test]
    fn the_body_names_both_apks_the_certificate_and_the_sums() -> Result<()> {
        let version = ReleaseVersion::parse("v1.0.0-rc.1")?;
        let sums = "00ff  pingers-1.0.0-rc.1-foss.apk\n";
        let text = body(&BodyParts {
            version: &version,
            notes: "Notes here.",
            repository: Some("o/p"),
            tag: "v1.0.0-rc.1",
            sums,
        });
        assert!(text.starts_with("# Pingers 1.0.0-rc.1\n"));
        assert!(text.contains("A pre-release"));
        assert!(text.contains("Notes here."));
        assert!(text.contains("`pingers-1.0.0-rc.1-gms.apk`"));
        assert!(text.contains("`pingers-1.0.0-rc.1-foss.apk`"));
        assert!(text.contains("https://github.com/o/p/blob/v1.0.0-rc.1/docs/install.md"));
        assert!(text.contains(
            "AA:44:E4:54:B0:F8:FA:A8:9B:8F:69:EF:72:90:6D:AA:2A:B8:C3:F2:D6:BE:87:B1:23:C2:D3:30:C9:BD:18:16"
        ));
        assert!(text.ends_with("```\n00ff  pingers-1.0.0-rc.1-foss.apk\n```\n"));
        let final_version = ReleaseVersion::parse("v1.0.0")?;
        let final_release = body(&BodyParts {
            version: &final_version,
            notes: "n",
            repository: None,
            tag: "v1.0.0",
            sums: "",
        });
        assert!(!final_release.contains("pre-release"));
        assert!(final_release.contains("`docs/install.md` in the repository"));
        Ok(())
    }

    #[test]
    fn assets_are_the_apks_and_aabs_with_their_digests() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("xtask-release-notes-{}", std::process::id()));
        let _fresh = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("pingers-1.0.0-gms.apk"), b"abc")?;
        fs::write(dir.join("pingers-1.0.0-gms.aab"), b"")?;
        fs::write(dir.join("mapping.txt"), b"x")?;
        let found = assets(&dir)?;
        let names: Vec<&str> = found.iter().map(|asset| asset.name.as_str()).collect();
        assert_eq!(names, ["pingers-1.0.0-gms.aab", "pingers-1.0.0-gms.apk"]);
        assert_eq!(
            found.last().map(|asset| asset.sha256.as_str()),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        fs::remove_file(dir.join("pingers-1.0.0-gms.apk"))?;
        assert!(assets(&dir).is_err());
        Ok(())
    }
}
