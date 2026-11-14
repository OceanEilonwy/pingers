// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask release-android [--flavour gms|foss]...`: the signed release build that
//! `.github/workflows/release.yml` runs, on this machine.
//!
//! It reads the same environment variables as CI (`android/docs/release.md`):
//! `PINGERS_KEYSTORE_PATH`, `PINGERS_KEYSTORE_PASSWORD` and `PINGERS_KEY_ALIAS` for the upload
//! key, the deployment hosts (`PINGERS_SERVER_HOST`, `PINGERS_OHTTP_RELAY`,
//! `PINGERS_PUSH_RELAY`, `PINGERS_MAP_HOST`) and `PINGERS_GOOGLE_SERVICES_JSON` for the gms
//! flavour. Gradle reads them itself; this only checks that the key is there, warns about hosts
//! left at their `.invalid` defaults, runs `assemble<Flavour>Release` and
//! `bundle<Flavour>Release`, and then checks that every APK and AAB is signed by the keystore's
//! certificate and nothing else. The password is never read into this process.
//!
//! Without `--flavour`, foss is always built and gms only when `PINGERS_GOOGLE_SERVICES_JSON` is
//! set, as in CI.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use crate::Outcome;
use crate::release_version::{self, ReleaseVersion};

/// The variables the upload key needs, all of them required.
const SIGNING_VARIABLES: [&str; 3] = [
    "PINGERS_KEYSTORE_PATH",
    "PINGERS_KEYSTORE_PASSWORD",
    "PINGERS_KEY_ALIAS",
];

/// Deployment hosts; each falls back to a `.invalid` host when unset (app/build.gradle.kts).
const HOST_VARIABLES: [&str; 4] = [
    "PINGERS_SERVER_HOST",
    "PINGERS_OHTTP_RELAY",
    "PINGERS_PUSH_RELAY",
    "PINGERS_MAP_HOST",
];

/// Build-time key pins (app/build.gradle.kts); unset, the app pins on first use, which an
/// official build must not (proposal decision 7).
const PIN_VARIABLES: [&str; 2] = ["PINGERS_GATEWAY_KEY_PINS", "PINGERS_PUSH_RELAY_KEY_PINS"];

/// Where the gms flavour finds Firebase's configuration.
const GOOGLE_SERVICES_VARIABLE: &str = "PINGERS_GOOGLE_SERVICES_JSON";

/// One of the app's product flavours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flavour {
    /// Play services location, FCM and UnifiedPush.
    Gms,
    /// No Google code; UnifiedPush only.
    Foss,
}

impl Flavour {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "gms" => Some(Self::Gms),
            "foss" => Some(Self::Foss),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Gms => "gms",
            Self::Foss => "foss",
        }
    }

    /// The flavour as it appears inside Gradle task names.
    fn task_part(self) -> &'static str {
        match self {
            Self::Gms => "Gms",
            Self::Foss => "Foss",
        }
    }
}

/// Which flavours to build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FlavourChoice {
    /// foss, and gms when Firebase's configuration is set.
    Default,
    /// Exactly these, in this order.
    Listed(Vec<Flavour>),
}

/// What `release-android` was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseOptions {
    pub(crate) flavours: FlavourChoice,
    /// The release tag the version comes from; `None` means the `v*` tag on `HEAD`.
    pub(crate) tag: Option<String>,
}

/// Parses the options after `release-android`; `None` for anything not understood.
pub(crate) fn parse_options(options: &[String]) -> Option<ReleaseOptions> {
    let mut listed = Vec::new();
    let mut tag = None;
    let mut options = options.iter();
    while let Some(option) = options.next() {
        match option.as_str() {
            "--flavour" | "--flavor" => {
                let flavour = Flavour::parse(options.next()?)?;
                if !listed.contains(&flavour) {
                    listed.push(flavour);
                }
            }
            "--tag" if tag.is_none() => tag = Some(options.next()?.clone()),
            _ => return None,
        }
    }
    let flavours = if listed.is_empty() {
        FlavourChoice::Default
    } else {
        FlavourChoice::Listed(listed)
    };
    Some(ReleaseOptions { flavours, tag })
}

/// The SHA-256 of the upload key's certificate (android/docs/release.md), lowercase hex. Release
/// builds must be signed by exactly this certificate: the release notes and docs/install.md
/// publish it for users to check, so a build signed by any other key is refused.
pub(crate) const UPLOAD_CERTIFICATE_SHA256: &str =
    "aa44e454b0f8faa89b8f69ef72906daa2ab8c3f2d6be87b123c2d330c9bd1816";

/// Where the renamed release files go, under the workspace root.
pub(crate) const OUTPUT_DIR: &str = "target/release-android";

/// The file name users and Obtainium see: `pingers-<version>-<flavour>.<extension>`.
fn release_file_name(version: &ReleaseVersion, flavour: Flavour, extension: &str) -> String {
    format!("pingers-{}-{}.{extension}", version.name, flavour.name())
}

/// The release tag: the one given, or the `v*` tag pointing at `HEAD`.
fn release_tag(root: &Path, given: Option<&str>) -> Result<String> {
    if let Some(tag) = given {
        return Ok(tag.to_owned());
    }
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "describe",
            "--tags",
            "--exact-match",
            "--match",
            "v*",
            "HEAD",
        ])
        .output()
        .context("running git describe")?;
    if !output.status.success() {
        bail!(
            "HEAD has no v* tag; pass --tag v<major>.<minor>.<patch>[-rc.<n>] to name the version"
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("git describe printed non-UTF-8")?
        .trim()
        .to_owned())
}

/// The environment this command depends on, read once.
struct Environment {
    keystore: Option<PathBuf>,
    alias: Option<String>,
    password_set: PasswordState,
    google_services_set: GoogleServices,
    unset_hosts: Vec<&'static str>,
    unset_pins: Vec<&'static str>,
    java_home: Option<PathBuf>,
    android_home: Option<PathBuf>,
}

/// Whether `PINGERS_KEYSTORE_PASSWORD` has a value. Its value is never read here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PasswordState {
    Set,
    Unset,
}

/// Whether `PINGERS_GOOGLE_SERVICES_JSON` is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GoogleServices {
    Configured,
    Absent,
}

/// A variable's value, treating an empty one as unset.
fn variable(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

impl Environment {
    fn read() -> Self {
        Self {
            keystore: variable("PINGERS_KEYSTORE_PATH").map(PathBuf::from),
            alias: variable("PINGERS_KEY_ALIAS").map(|alias| alias.to_string_lossy().into_owned()),
            password_set: if variable("PINGERS_KEYSTORE_PASSWORD").is_some() {
                PasswordState::Set
            } else {
                PasswordState::Unset
            },
            google_services_set: if variable(GOOGLE_SERVICES_VARIABLE).is_some() {
                GoogleServices::Configured
            } else {
                GoogleServices::Absent
            },
            unset_hosts: HOST_VARIABLES
                .into_iter()
                .filter(|name| variable(name).is_none())
                .collect(),
            unset_pins: PIN_VARIABLES
                .into_iter()
                .filter(|name| variable(name).is_none())
                .collect(),
            java_home: variable("JAVA_HOME").map(PathBuf::from),
            android_home: variable("ANDROID_HOME")
                .or_else(|| variable("ANDROID_SDK_ROOT"))
                .map(PathBuf::from),
        }
    }

    /// The JDK tool `name`: from `JAVA_HOME` when set, else from `PATH`.
    fn java_tool(&self, name: &str) -> PathBuf {
        self.java_home
            .as_ref()
            .map_or_else(|| PathBuf::from(name), |home| home.join("bin").join(name))
    }
}

/// The upload key, once the environment has been checked.
struct SigningKey {
    keystore: PathBuf,
    alias: String,
}

/// Builds and verifies the signed release APKs and AABs.
pub(crate) fn run(root: &Path, options: &ReleaseOptions) -> Result<Outcome> {
    let environment = Environment::read();
    let tag = release_tag(root, options.tag.as_deref())?;
    let version = release_version::checked(root, &tag)?;
    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "release-android: {tag}: versionName {}, versionCode {}",
        version.name, version.code
    )?;

    let Some(key) = signing_key(&environment, &mut stdout)? else {
        return Ok(Outcome::Failed);
    };
    for host in &environment.unset_hosts {
        writeln!(
            stdout,
            "release-android: warning: {host} is unset; the app gets its .invalid default"
        )?;
    }
    for pins in &environment.unset_pins {
        writeln!(
            stdout,
            "release-android: warning: {pins} is unset; the app pins that key on first use"
        )?;
    }
    let flavours = choose_flavours(
        &options.flavours,
        environment.google_services_set,
        &mut stdout,
    )?;
    drop(stdout);

    let expected = keystore_fingerprint(&environment, &key)?;
    writeln!(
        io::stdout(),
        "release-android: upload certificate SHA-256 {}",
        colon_hex(&expected)
    )?;
    if expected != UPLOAD_CERTIFICATE_SHA256 {
        writeln!(
            io::stdout(),
            "release-android: that is not the published upload certificate {}; releases must be \
             signed by it (android/docs/release.md)",
            colon_hex(UPLOAD_CERTIFICATE_SHA256)
        )?;
        return Ok(Outcome::Failed);
    }

    let mut arguments = vec![
        format!("-PpingersVersionCode={}", version.code),
        format!("-PpingersVersionName={}", version.name),
    ];
    for flavour in &flavours {
        arguments.push(format!("assemble{}Release", flavour.task_part()));
        arguments.push(format!("bundle{}Release", flavour.task_part()));
    }
    if gradle(&root.join("android"), &arguments)? == Outcome::Failed {
        return Ok(Outcome::Failed);
    }

    let apksigner = apksigner(&environment)?;
    let outputs = root.join("android/app/build/outputs");
    let mut outcome = Outcome::Passed;
    let mut built = Vec::new();
    for flavour in &flavours {
        let name = flavour.name();
        // One APK per flavour: the release build never splits by ABI (-PpingersSplits is unset).
        let apks = files_with_extension(&outputs.join("apk").join(name).join("release"), "apk")?;
        let [apk] = apks.as_slice() else {
            bail!("expected one {name} release APK, found {}", apks.len());
        };
        let digests = apk_signers(&apksigner, apk)?;
        outcome = outcome.and(report(apk, &digests, &expected)?);
        let bundle = outputs
            .join("bundle")
            .join(format!("{name}Release"))
            .join(format!("app-{name}-release.aab"));
        let digests = jar_signers(&environment, &bundle)?;
        outcome = outcome.and(report(&bundle, &digests, &expected)?);
        built.push((*flavour, apk.clone(), bundle));
    }
    if outcome == Outcome::Failed {
        return Ok(outcome);
    }
    writeln!(
        io::stdout(),
        "release-android: every APK and AAB is signed by the upload key"
    )?;
    let destination = root.join(OUTPUT_DIR);
    fs::create_dir_all(&destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    for (flavour, apk, bundle) in built {
        for (source, extension) in [(apk, "apk"), (bundle, "aab")] {
            let target = destination.join(release_file_name(&version, flavour, extension));
            fs::copy(&source, &target)
                .with_context(|| format!("copying {} to {}", source.display(), target.display()))?;
            writeln!(io::stdout(), "release-android: wrote {}", target.display())?;
        }
    }
    Ok(Outcome::Passed)
}

impl Outcome {
    /// Failed if either is.
    fn and(self, other: Self) -> Self {
        if self == Self::Passed && other == Self::Passed {
            Self::Passed
        } else {
            Self::Failed
        }
    }
}

/// The upload key from the environment, or `None` after saying what is missing.
fn signing_key(environment: &Environment, out: &mut impl io::Write) -> Result<Option<SigningKey>> {
    let mut missing: Vec<&str> = Vec::new();
    if environment.keystore.is_none() {
        missing.push("PINGERS_KEYSTORE_PATH");
    }
    if environment.password_set == PasswordState::Unset {
        missing.push("PINGERS_KEYSTORE_PASSWORD");
    }
    if environment.alias.is_none() {
        missing.push("PINGERS_KEY_ALIAS");
    }
    if let (Some(keystore), Some(alias), []) = (
        &environment.keystore,
        &environment.alias,
        missing.as_slice(),
    ) {
        if !keystore.is_file() {
            writeln!(
                out,
                "release-android: PINGERS_KEYSTORE_PATH does not name a file: {}",
                keystore.display()
            )?;
            return Ok(None);
        }
        return Ok(Some(SigningKey {
            keystore: keystore.clone(),
            alias: alias.clone(),
        }));
    }
    writeln!(
        out,
        "release-android: the upload key is not configured; unset: {}\n\
         release-android: set {} (android/docs/release.md)",
        missing.join(", "),
        SIGNING_VARIABLES.join(", ")
    )?;
    Ok(None)
}

/// The flavours to build, saying why gms is left out when it is.
fn choose_flavours(
    choice: &FlavourChoice,
    google_services: GoogleServices,
    out: &mut impl io::Write,
) -> Result<Vec<Flavour>> {
    let flavours = match (choice, google_services) {
        (FlavourChoice::Listed(listed), _) => listed.clone(),
        (FlavourChoice::Default, GoogleServices::Configured) => vec![Flavour::Gms, Flavour::Foss],
        (FlavourChoice::Default, GoogleServices::Absent) => {
            writeln!(
                out,
                "release-android: {GOOGLE_SERVICES_VARIABLE} is unset; skipping gms (pass \
                 --flavour gms to build it anyway, with FCM off)"
            )?;
            vec![Flavour::Foss]
        }
    };
    if flavours.contains(&Flavour::Gms) && google_services == GoogleServices::Absent {
        writeln!(
            out,
            "release-android: warning: building gms without {GOOGLE_SERVICES_VARIABLE}; FCM is \
             off unless pingers.googleServicesJson is set in ~/.gradle/gradle.properties"
        )?;
    }
    Ok(flavours)
}

/// The SHA-256 fingerprint of the key's certificate, as lowercase hex. keytool reads the
/// password from the environment itself (`-storepass:env`).
fn keystore_fingerprint(environment: &Environment, key: &SigningKey) -> Result<String> {
    let keytool = environment.java_tool("keytool");
    let output = Command::new(&keytool)
        .args(["-list", "-v", "-storetype", "PKCS12", "-keystore"])
        .arg(&key.keystore)
        .args([
            "-alias",
            &key.alias,
            "-storepass:env",
            "PINGERS_KEYSTORE_PASSWORD",
        ])
        .output()
        .with_context(|| format!("running {}", keytool.display()))?;
    if !output.status.success() {
        bail!(
            "keytool could not open the keystore with that alias and password: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    let text = String::from_utf8(output.stdout).context("keytool printed non-UTF-8")?;
    let digests = keytool_sha256(&text);
    match digests.as_slice() {
        [digest] => Ok(digest.clone()),
        _ => bail!(
            "expected one certificate SHA-256 from keytool, found {}",
            digests.len()
        ),
    }
}

fn gradle(android: &Path, tasks: &[String]) -> Result<Outcome> {
    // No configuration cache: its entries would hold the signing config, password included.
    let arguments: Vec<&str> = ["--no-configuration-cache"]
        .into_iter()
        .chain(tasks.iter().map(String::as_str))
        .collect();
    writeln!(
        io::stdout(),
        "release-android: running `./gradlew {}`",
        arguments.join(" ")
    )?;
    let status = Command::new(android.join("gradlew"))
        .current_dir(android)
        .args(&arguments)
        .status()
        .context("running ./gradlew")?;
    if status.success() {
        Ok(Outcome::Passed)
    } else {
        writeln!(io::stdout(), "release-android: Gradle failed")?;
        Ok(Outcome::Failed)
    }
}

/// The SDK's apksigner, run as `java -jar` with `JAVA_HOME`'s java: its wrapper script looks
/// for `java` on `PATH` only.
struct ApkSigner {
    java: PathBuf,
    jar: PathBuf,
}

/// apksigner from the newest build tools in the SDK.
fn apksigner(environment: &Environment) -> Result<ApkSigner> {
    let sdk = environment
        .android_home
        .as_ref()
        .context("set ANDROID_HOME to the Android SDK, to find apksigner")?;
    let build_tools = sdk.join("build-tools");
    let mut versions: Vec<PathBuf> = fs::read_dir(&build_tools)
        .with_context(|| format!("reading {}", build_tools.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|version| version.join(APKSIGNER_JAR).is_file())
        .collect();
    versions.sort_by_key(|version| version_key(version));
    let newest = versions
        .pop()
        .with_context(|| format!("no {APKSIGNER_JAR} under {}", build_tools.display()))?;
    Ok(ApkSigner {
        java: environment.java_tool("java"),
        jar: newest.join(APKSIGNER_JAR),
    })
}

/// apksigner's jar inside a build-tools version directory.
const APKSIGNER_JAR: &str = "lib/apksigner.jar";

/// Orders build-tools directories by their numeric version (`36.1.0` after `36.0.0`).
fn version_key(version: &Path) -> Vec<u32> {
    version
        .file_name()
        .map(|name| {
            name.to_string_lossy()
                .split(['.', '-'])
                .map(|part| part.parse().unwrap_or(0))
                .collect()
        })
        .unwrap_or_default()
}

fn files_with_extension(directory: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .with_context(|| format!("reading {}", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|found| found == extension))
        .collect();
    files.sort();
    if files.is_empty() {
        bail!("no .{extension} in {}", directory.display());
    }
    Ok(files)
}

/// The SHA-256 digest of every signer `apksigner verify` reports, after it verified the APK.
fn apk_signers(apksigner: &ApkSigner, apk: &Path) -> Result<Vec<String>> {
    let output = Command::new(&apksigner.java)
        .arg("-jar")
        .arg(&apksigner.jar)
        .args(["verify", "--print-certs"])
        .arg(apk)
        .output()
        .with_context(|| format!("running {}", apksigner.jar.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        bail!(
            "apksigner rejected {}: {}{}",
            apk.display(),
            text.trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(apksigner_sha256(&text))
}

/// The SHA-256 digest of every certificate an AAB's JAR signature carries.
fn jar_signers(environment: &Environment, bundle: &Path) -> Result<Vec<String>> {
    let keytool = environment.java_tool("keytool");
    let output = Command::new(&keytool)
        .args(["-printcert", "-jarfile"])
        .arg(bundle)
        .output()
        .with_context(|| format!("running {}", keytool.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        bail!(
            "keytool could not read {}: {}",
            bundle.display(),
            text.trim()
        );
    }
    Ok(keytool_sha256(&text))
}

/// Prints what signed `file` and whether that is exactly the upload key.
fn report(file: &Path, digests: &[String], expected: &str) -> Result<Outcome> {
    let mut stdout = io::stdout().lock();
    let signed_by_key = !digests.is_empty() && digests.iter().all(|digest| digest == expected);
    let verdict = if signed_by_key { "ok" } else { "WRONG SIGNER" };
    let shown: Vec<String> = digests.iter().map(|digest| colon_hex(digest)).collect();
    writeln!(
        stdout,
        "release-android: {verdict}: {} signed by [{}]",
        file.display(),
        shown.join(", ")
    )?;
    Ok(if signed_by_key {
        Outcome::Passed
    } else {
        Outcome::Failed
    })
}

/// Digests from apksigner's `Signer #1 certificate SHA-256 digest: <hex>` lines.
fn apksigner_sha256(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split_once("certificate SHA-256 digest:"))
        .map(|(_, digest)| normalise_hex(digest))
        .collect()
}

/// Digests from keytool's `SHA256: AA:BB:...` lines.
fn keytool_sha256(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("SHA256:"))
        .map(normalise_hex)
        .collect()
}

/// Lowercase hex without separators.
fn normalise_hex(digest: &str) -> String {
    digest
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// `AA:BB:...`, as keytool and the Play Console show fingerprints.
pub(crate) fn colon_hex(digest: &str) -> String {
    let characters: Vec<char> = digest.to_ascii_uppercase().chars().collect();
    characters
        .chunks(2)
        .map(|pair| pair.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn parse_all(options: &[&str]) -> Option<ReleaseOptions> {
        let options: Vec<String> = options.iter().map(|option| (*option).to_owned()).collect();
        parse_options(&options)
    }

    fn parse(options: &[&str]) -> Option<FlavourChoice> {
        parse_all(options).map(|parsed| parsed.flavours)
    }

    #[test]
    fn flavours_parse_once_each_and_anything_else_is_refused() {
        assert_eq!(parse(&[]), Some(FlavourChoice::Default));
        assert_eq!(
            parse(&["--flavour", "foss", "--flavor", "gms", "--flavour", "foss"]),
            Some(FlavourChoice::Listed(vec![Flavour::Foss, Flavour::Gms]))
        );
        assert_eq!(parse(&["--flavour"]), None);
        assert_eq!(parse(&["--flavour", "huawei"]), None);
        assert_eq!(parse(&["--debug"]), None);
    }

    #[test]
    fn a_tag_may_be_given_once() {
        assert_eq!(parse_all(&[]).and_then(|parsed| parsed.tag), None);
        assert_eq!(
            parse_all(&["--tag", "v1.0.0-rc.1", "--flavour", "foss"]),
            Some(ReleaseOptions {
                flavours: FlavourChoice::Listed(vec![Flavour::Foss]),
                tag: Some("v1.0.0-rc.1".to_owned()),
            })
        );
        assert_eq!(parse_all(&["--tag"]), None);
        assert_eq!(parse_all(&["--tag", "v1.0.0", "--tag", "v1.0.1"]), None);
    }

    #[test]
    fn release_files_are_named_for_one_obtainium_filter_per_flavour() -> Result<()> {
        let version = ReleaseVersion::parse("v1.0.0-rc.1")?;
        assert_eq!(
            release_file_name(&version, Flavour::Gms, "apk"),
            "pingers-1.0.0-rc.1-gms.apk"
        );
        assert_eq!(
            release_file_name(&version, Flavour::Foss, "aab"),
            "pingers-1.0.0-rc.1-foss.aab"
        );
        Ok(())
    }

    #[test]
    fn the_published_certificate_is_the_documented_one() {
        assert_eq!(
            colon_hex(UPLOAD_CERTIFICATE_SHA256),
            "AA:44:E4:54:B0:F8:FA:A8:9B:8F:69:EF:72:90:6D:AA:2A:B8:C3:F2:D6:BE:87:B1:23:C2:D3:30:C9:BD:18:16"
        );
    }

    #[test]
    fn gms_is_skipped_by_default_only_without_firebase() -> Result<()> {
        let mut out = Vec::new();
        assert_eq!(
            choose_flavours(&FlavourChoice::Default, GoogleServices::Absent, &mut out)?,
            vec![Flavour::Foss]
        );
        assert!(String::from_utf8(out)?.contains("skipping gms"));
        assert_eq!(
            choose_flavours(
                &FlavourChoice::Default,
                GoogleServices::Configured,
                &mut Vec::new()
            )?,
            vec![Flavour::Gms, Flavour::Foss]
        );
        let mut out = Vec::new();
        assert_eq!(
            choose_flavours(
                &FlavourChoice::Listed(vec![Flavour::Gms]),
                GoogleServices::Absent,
                &mut out
            )?,
            vec![Flavour::Gms]
        );
        assert!(String::from_utf8(out)?.contains("FCM is off"));
        Ok(())
    }

    #[test]
    fn digests_are_read_from_both_tools_in_one_form() {
        let apksigner = "Signer #1 certificate DN: CN=Pingers\n\
                         Signer #1 certificate SHA-256 digest: aa44e454b0\n\
                         Signer #1 certificate SHA-1 digest: ffd540\n";
        let keytool = "Certificate fingerprints:\n\t SHA1: FF:D5:40\n\t SHA256: AA:44:E4:54:B0\n";
        assert_eq!(apksigner_sha256(apksigner), vec!["aa44e454b0".to_owned()]);
        assert_eq!(keytool_sha256(keytool), vec!["aa44e454b0".to_owned()]);
        assert_eq!(colon_hex("aa44e454b0"), "AA:44:E4:54:B0");
    }

    #[test]
    fn build_tools_sort_by_number() {
        let mut paths = [
            PathBuf::from("/sdk/build-tools/36.1.0"),
            PathBuf::from("/sdk/build-tools/9.0.0"),
            PathBuf::from("/sdk/build-tools/37.0.0"),
        ];
        paths.sort_by_key(|path| version_key(path));
        assert_eq!(
            paths.last(),
            Some(&PathBuf::from("/sdk/build-tools/37.0.0"))
        );
    }

    #[test]
    fn a_missing_key_names_every_unset_variable() -> Result<()> {
        let environment = Environment {
            keystore: None,
            alias: Some("pingers-upload".into()),
            password_set: PasswordState::Unset,
            google_services_set: GoogleServices::Absent,
            unset_hosts: Vec::new(),
            unset_pins: Vec::new(),
            java_home: None,
            android_home: None,
        };
        let mut out = Vec::new();
        assert!(signing_key(&environment, &mut out)?.is_none());
        let said = String::from_utf8(out)?;
        assert!(said.contains("unset: PINGERS_KEYSTORE_PATH, PINGERS_KEYSTORE_PASSWORD"));
        assert!(!said.contains(
            "unset: PINGERS_KEYSTORE_PATH, PINGERS_KEYSTORE_PASSWORD, PINGERS_KEY_ALIAS"
        ));
        Ok(())
    }
}
