// SPDX-License-Identifier: AGPL-3.0-only
//! Packaging for monokulo-nodo (docs/deploy-nodo.md).
//!
//! - `cargo xtask nodo-dist [<commit>] [--nodo <checkout>]`: the source tarball and recipes for
//!   nodo's xbps-src builder. Writes, under `target/nodo/`:
//!   - `pingers-<commit>.tar.gz`: `git archive` of the commit, plus `cargo vendor` output in
//!     `cargo-vendor/` and a `.cargo/config.toml` source replacement, so the build needs no
//!     network and no repository. Reproducible: sorted, owned by root, dated at the commit, and
//!     gzip with no name or time in its header.
//!   - `srcpkgs/`: the commit's `packaging/nodo/srcpkgs` with `@VERSION@`, `@COMMIT@` and
//!     `@SOURCE_SHA256@` filled in, ready for nodo's `NODO_EXTRA_PACKAGES` (nodo change N2).
//!
//!   It prints `commit=`, `version=`, `sha256=`, `tarball=` and `recipes=` lines (a workflow can
//!   append them to `$GITHUB_OUTPUT`). With `--nodo`, it also copies the tarball to
//!   `<checkout>/build/hostdir/sources/pingers-server-<version>/`, where xbps-src looks before
//!   fetching. The version is the workspace version, `pre`, and the commit time in UTC
//!   (`0.1.0pre20261113.1930`), so later commits sort later.
//! - `cargo xtask dev-ca [--san <name or address>]...`: a throwaway CA and a server certificate
//!   for `localhost`, `127.0.0.1` and `10.0.2.2` (the emulator's view of this machine), in
//!   `target/dev-ca/`: `ca.pem` (for `PINGERS_EXTRA_CA`), `leaf.pem` and `leaf.key` (for the dev
//!   Caddy). The CA's private key is never written, so the CA can sign nothing else.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use sha2::{Digest as _, Sha256};

use crate::Outcome;

/// Options for `nodo-dist`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DistOptions {
    /// The commit to package; `HEAD` when not given.
    pub(crate) commit: Option<String>,
    /// A monokulo-nodo checkout whose xbps-src hostdir gets the tarball.
    pub(crate) nodo: Option<PathBuf>,
}

/// Parses the options after `nodo-dist`; `None` for anything not understood.
pub(crate) fn parse_dist_options(options: &[String]) -> Option<DistOptions> {
    let mut parsed = DistOptions::default();
    let mut options = options.iter();
    while let Some(option) = options.next() {
        match option.as_str() {
            "--nodo" => parsed.nodo = Some(PathBuf::from(options.next()?)),
            other if !other.starts_with('-') && parsed.commit.is_none() => {
                parsed.commit = Some(other.to_owned());
            }
            _ => return None,
        }
    }
    Some(parsed)
}

/// Parses the options after `dev-ca`: extra subject names; `None` for anything not understood.
pub(crate) fn parse_ca_options(options: &[String]) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut options = options.iter();
    while let Some(option) = options.next() {
        match option.as_str() {
            "--san" => names.push(options.next()?.clone()),
            _ => return None,
        }
    }
    Some(names)
}

/// Placeholders in `packaging/nodo/srcpkgs/pingers-server/template`.
struct Recipe<'values> {
    version: &'values str,
    commit: &'values str,
    sha256: &'values str,
}

impl Recipe<'_> {
    fn fill(&self, template: &str) -> String {
        template
            .replace("@VERSION@", self.version)
            .replace("@COMMIT@", self.commit)
            .replace("@SOURCE_SHA256@", self.sha256)
    }
}

/// The xbps version for a workspace version and a commit time (`YYYYMMDDHHMM`, UTC):
/// `<version>pre<YYYYMMDD>.<HHMM>`. The date and time are separate components because nodo-feed,
/// like xbps, compares each as a 32-bit number and refuses a version it cannot compare.
fn package_version(workspace_version: &str, commit_time: &str) -> Result<String> {
    let plain = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
    };
    let (Some(date), Some(time)) = (commit_time.get(..8), commit_time.get(8..)) else {
        bail!("cannot make an xbps version from the commit time {commit_time:?}");
    };
    if !plain(workspace_version)
        || time.len() != 4
        || !commit_time.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("cannot make an xbps version from {workspace_version:?} and {commit_time:?}");
    }
    Ok(format!("{workspace_version}pre{date}.{time}"))
}

/// Rewrites `cargo vendor`'s printed configuration to name the vendor directory relative to the
/// tarball's root, which is where `.cargo/` sits.
fn relative_vendor_config(printed: &str, absolute: &str) -> Result<String> {
    let needle = format!("directory = \"{absolute}\"");
    if !printed.contains(&needle) {
        bail!("cargo vendor printed no `{needle}`");
    }
    Ok(printed.replace(&needle, "directory = \"cargo-vendor\""))
}

fn run(command: &mut Command) -> Result<String> {
    let shown = format!("{command:?}");
    let output = command
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("running {shown}"))?;
    if !output.status.success() {
        bail!("{shown} failed");
    }
    String::from_utf8(output.stdout).with_context(|| format!("{shown} printed non-UTF-8"))
}

fn git(root: &Path, arguments: &[&str]) -> Result<String> {
    Ok(run(Command::new("git").current_dir(root).args(arguments))?
        .trim()
        .to_owned())
}

pub(crate) fn nodo_dist(root: &Path, options: &DistOptions) -> Result<Outcome> {
    let wanted = options.commit.as_deref().unwrap_or("HEAD");
    let commit = git(
        root,
        &["rev-parse", "--verify", &format!("{wanted}^{{commit}}")],
    )?;
    let (seconds, version) = commit_version(root, &commit)?;

    let out = root.join("target").join("nodo");
    let stage = out.join("stage");
    let name = format!("pingers-{commit}");
    let tree = stage.join(&name);
    if stage.exists() {
        fs::remove_dir_all(&stage).with_context(|| format!("removing {}", stage.display()))?;
    }
    fs::create_dir_all(&tree)?;

    // The commit's files, as git stores them.
    let archive = stage.join("source.tar");
    run(Command::new("git").current_dir(root).args([
        "archive",
        "--format=tar",
        "-o",
        &archive.to_string_lossy(),
        &commit,
    ]))?;
    run(Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&tree))?;
    fs::remove_file(&archive)?;
    let recipes_source = tree.join("packaging").join("nodo").join("srcpkgs");
    if !recipes_source.is_dir() {
        bail!("{commit} has no packaging/nodo/srcpkgs");
    }
    vendor_crates(&tree)?;

    fs::create_dir_all(&out)?;
    let tarball = out.join(format!("{name}.tar.gz"));
    write_tarball(&stage, &name, &seconds, &tarball)?;
    let sha256 = file_sha256(&tarball)?;

    // The recipes, filled in.
    let recipes = out.join("srcpkgs");
    if recipes.exists() {
        fs::remove_dir_all(&recipes)?;
    }
    run(Command::new("cp")
        .arg("-a")
        .arg(&recipes_source)
        .arg(&recipes))?;
    let template = recipes.join("pingers-server").join("template");
    let filled = Recipe {
        version: &version,
        commit: &commit,
        sha256: &sha256,
    }
    .fill(&fs::read_to_string(&template)?);
    fs::write(&template, filled)?;
    fs::remove_dir_all(&stage)?;

    if let Some(nodo) = &options.nodo {
        let sources = nodo
            .join("build")
            .join("hostdir")
            .join("sources")
            .join(format!("pingers-server-{version}"));
        fs::create_dir_all(&sources)?;
        fs::copy(&tarball, sources.join(format!("{name}.tar.gz")))?;
    }

    let mut stdout = io::stdout().lock();
    writeln!(stdout, "commit={commit}")?;
    writeln!(stdout, "version={version}")?;
    writeln!(stdout, "sha256={sha256}")?;
    writeln!(stdout, "tarball={}", tarball.display())?;
    writeln!(stdout, "recipes={}", recipes.display())?;
    Ok(Outcome::Passed)
}

/// The commit's time in Unix seconds, and the xbps version made from it.
fn commit_version(root: &Path, commit: &str) -> Result<(String, String)> {
    let seconds = git(root, &["show", "-s", "--format=%ct", commit])?;
    let time = run(Command::new("git")
        .current_dir(root)
        .env("TZ", "UTC0")
        .args([
            "show",
            "-s",
            "--format=%cd",
            "--date=format-local:%Y%m%d%H%M",
            commit,
        ]))?;
    let manifest: toml::Table =
        toml::from_str(&git(root, &["show", &format!("{commit}:Cargo.toml")])?)
            .context("parsing the commit's Cargo.toml")?;
    let workspace_version = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .context("the commit's Cargo.toml has no workspace.package.version")?;
    let version = package_version(workspace_version, time.trim())?;
    Ok((seconds, version))
}

/// Every crate the lock file names, and the source replacement that points cargo at them.
fn vendor_crates(tree: &Path) -> Result<()> {
    let vendor = tree.join("cargo-vendor");
    let printed = run(Command::new("cargo")
        .current_dir(tree)
        .args(["vendor", "--locked", "--versioned-dirs"])
        .arg("--manifest-path")
        .arg(tree.join("Cargo.toml"))
        .arg(&vendor))?;
    let config = relative_vendor_config(&printed, &vendor.to_string_lossy())?;
    let cargo_dir = tree.join(".cargo");
    fs::create_dir_all(&cargo_dir)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cargo_dir.join("config.toml"))?;
    writeln!(file, "\n# Added by `cargo xtask nodo-dist`.\n{config}")?;
    Ok(())
}

/// A reproducible tarball of `stage/name`: sorted, root-owned, dated at the commit, and gzip
/// with no name or time in its header.
fn write_tarball(stage: &Path, name: &str, seconds: &str, tarball: &Path) -> Result<()> {
    let mut tar = Command::new("tar")
        .current_dir(stage)
        .args([
            "--sort=name",
            "--format=gnu",
            &format!("--mtime=@{seconds}"),
            "--owner=0",
            "--group=0",
            "--numeric-owner",
            "--mode=u+rw,go-w,go+rX",
            "-cf",
            "-",
            name,
        ])
        .stdout(Stdio::piped())
        .spawn()
        .context("running tar")?;
    let mut encoder =
        flate2::GzBuilder::new().write(fs::File::create(tarball)?, flate2::Compression::best());
    io::copy(
        tar.stdout.as_mut().context("tar has no stdout")?,
        &mut encoder,
    )?;
    encoder.finish()?.sync_all()?;
    if !tar.wait()?.success() {
        bail!("tar failed");
    }
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        let Some(chunk) = buffer.get(..read) else {
            bail!("read past the buffer");
        };
        if chunk.is_empty() {
            break;
        }
        hasher.update(chunk);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Names every dev certificate covers: this machine, and an Android emulator's view of it.
const DEV_NAMES: [&str; 3] = ["localhost", "127.0.0.1", "10.0.2.2"];

pub(crate) fn dev_ca(root: &Path, extra: &[String]) -> Result<Outcome> {
    let dir = root.join("target").join("dev-ca");
    let names = write_dev_ca(&dir, extra)?;
    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "dev-ca: {} (CA, for PINGERS_EXTRA_CA), leaf.pem and leaf.key for {}; the CA key was not kept",
        dir.join("ca.pem").display(),
        names.join(", ")
    )?;
    Ok(Outcome::Passed)
}

/// Writes `ca.pem`, `leaf.pem` (leaf then CA) and `leaf.key` (0600) into `dir`, for
/// [`DEV_NAMES`] and `extra`, and returns the names the leaf covers. The CA key is dropped.
pub(crate) fn write_dev_ca(dir: &Path, extra: &[String]) -> Result<Vec<String>> {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose, date_time_ymd,
    };

    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.distinguished_name.push(
        DnType::CommonName,
        "Pingers dev CA (throwaway, tier-1 tests)",
    );
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.not_before = date_time_ymd(2026, 1, 1);
    ca_params.not_after = date_time_ymd(2036, 1, 1);
    let ca_key = KeyPair::generate()?;
    let ca = ca_params.self_signed(&ca_key)?;
    let issuer = Issuer::new(ca_params, ca_key);

    let names: Vec<String> = DEV_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .chain(extra.iter().cloned())
        .collect();
    let mut leaf_params = CertificateParams::new(names.clone())?;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "Pingers dev server");
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.not_before = date_time_ymd(2026, 1, 1);
    leaf_params.not_after = date_time_ymd(2028, 1, 1);
    let leaf_key = KeyPair::generate()?;
    let leaf = leaf_params.signed_by(&leaf_key, &issuer)?;

    fs::create_dir_all(dir)?;
    fs::write(dir.join("ca.pem"), ca.pem())?;
    fs::write(dir.join("leaf.pem"), format!("{}{}", leaf.pem(), ca.pem()))?;
    let key_path = dir.join("leaf.key");
    fs::write(&key_path, leaf_key.serialize_pem())?;
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    Ok(names)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn strings(options: &[&str]) -> Vec<String> {
        options.iter().map(|option| (*option).to_owned()).collect()
    }

    #[test]
    fn dist_takes_one_commit_and_an_optional_nodo_checkout() {
        assert_eq!(parse_dist_options(&[]), Some(DistOptions::default()));
        assert_eq!(
            parse_dist_options(&strings(&["abc123", "--nodo", "/n"])),
            Some(DistOptions {
                commit: Some("abc123".into()),
                nodo: Some(PathBuf::from("/n")),
            })
        );
        assert_eq!(parse_dist_options(&strings(&["a", "b"])), None);
        assert_eq!(parse_dist_options(&strings(&["--nodo"])), None);
        assert_eq!(parse_dist_options(&strings(&["--sign"])), None);
    }

    #[test]
    fn dev_ca_takes_only_extra_names() {
        assert_eq!(
            parse_ca_options(&strings(&["--san", "192.168.1.5"])),
            Some(vec!["192.168.1.5".to_owned()])
        );
        assert_eq!(parse_ca_options(&strings(&["--san"])), None);
        assert_eq!(parse_ca_options(&strings(&["x"])), None);
    }

    #[test]
    fn versions_sort_by_commit_time_and_stay_valid_for_xbps() -> Result<()> {
        assert_eq!(
            package_version("0.1.0", "202611131930")?,
            "0.1.0pre20261113.1930"
        );
        // xbps versions cannot hold '-' or '_', and the time must be the full minute stamp.
        assert!(package_version("0.1.0-rc1", "202611131930").is_err());
        assert!(package_version("0.1.0", "20261113").is_err());
        assert!(package_version("0.1.0", "2026111319_0").is_err());
        Ok(())
    }

    #[test]
    fn the_vendor_directory_is_made_relative() -> Result<()> {
        let printed = "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n\
                       [source.vendored-sources]\ndirectory = \"/tmp/x/cargo-vendor\"\n";
        let relative = relative_vendor_config(printed, "/tmp/x/cargo-vendor")?;
        assert!(relative.contains("directory = \"cargo-vendor\"\n"));
        assert!(!relative.contains("/tmp/x"));
        assert!(relative_vendor_config(printed, "/elsewhere").is_err());
        Ok(())
    }

    #[test]
    fn every_placeholder_in_the_recipe_is_filled() -> Result<()> {
        let template = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../packaging/nodo/srcpkgs/pingers-server/template"),
        )?;
        let filled = Recipe {
            version: "0.1.0pre20261113.1930",
            commit: "c0ffee",
            sha256: "ab",
        }
        .fill(&template);
        for placeholder in ["@VERSION@", "@COMMIT@", "@SOURCE_SHA256@"] {
            assert!(!filled.contains(placeholder), "{placeholder} is left");
        }
        assert!(filled.contains("version=0.1.0pre20261113.1930\n"));
        assert!(filled.contains("_commit=c0ffee\n"));
        assert!(filled.contains("checksum=\"ab\n"));
        // The manifest's own @VERSION@ is filled at install time by the recipe, so its sed
        // pattern must survive.
        assert!(filled.contains("sed -i \"s/@\"\"VERSION@/${version}/\""));
        Ok(())
    }
}
