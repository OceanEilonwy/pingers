// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask local-app [<local-stack options>]`: the foss debug APK built against the local
//! stack. foss needs no FCM, so it works on any emulator; push goes through UnifiedPush and the
//! stack's ntfy.
//!
//! It prepares the stack's keys if they are not there yet (as `local-stack --prepare-only`
//! would), then runs `./gradlew :app:assembleFossDebug` with the variables of `<dir>/app.env`:
//! `PINGERS_SERVER_HOST`, `PINGERS_OHTTP_RELAY`, `PINGERS_PUSH_RELAY`, `PINGERS_MAP_HOST`,
//! `PINGERS_GATEWAY_KEY_PINS`, `PINGERS_PUSH_RELAY_KEY_PINS`, `PINGERS_EXTRA_CA` (debug
//! builds only) and `PINGERS_DEV_REGISTRATION_CODE`. The same build by hand:
//!
//! ```sh
//! set -a; . <dir>/app.env; set +a
//! (cd android && ./gradlew :app:assembleFossDebug)
//! ```

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

use crate::Outcome;
use crate::stack::{self, Layout, StackOptions};

/// Which flavour of the debug build: foss (`UnifiedPush` only) or gms (Play services location,
/// FCM when the build has a Firebase configuration, and `UnifiedPush`, which wins when a
/// distributor is installed).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Flavour {
    /// `assembleFossDebug`.
    #[default]
    Foss,
    /// `assembleGmsDebug`.
    Gms,
}

impl Flavour {
    /// The flavour named on the command line.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "foss" => Some(Self::Foss),
            "gms" => Some(Self::Gms),
            _ => None,
        }
    }

    const fn task(self) -> &'static str {
        match self {
            Self::Foss => ":app:assembleFossDebug",
            Self::Gms => ":app:assembleGmsDebug",
        }
    }

    /// The APK the task writes, relative to the workspace.
    const fn apk(self) -> &'static str {
        match self {
            Self::Foss => "android/app/build/outputs/apk/foss/debug/app-foss-debug.apk",
            Self::Gms => "android/app/build/outputs/apk/gms/debug/app-gms-debug.apk",
        }
    }

    /// The debug build's application id.
    pub(crate) const fn package(self) -> &'static str {
        match self {
            Self::Foss => "org.pingers.app.foss.debug",
            Self::Gms => "org.pingers.app.debug",
        }
    }
}

/// `cargo xtask local-app`.
///
/// # Errors
///
/// When the stack cannot be prepared or Gradle fails.
pub(crate) fn run(workspace: &Path, options: &StackOptions) -> Result<Outcome> {
    let apk = build(workspace, options, Flavour::Foss)?;
    writeln!(io::stdout(), "local-app: {}", apk.display())?;
    Ok(Outcome::Passed)
}

/// Builds the APK and returns its path.
///
/// # Errors
///
/// When the stack cannot be prepared or Gradle fails.
pub(crate) fn build(workspace: &Path, options: &StackOptions, flavour: Flavour) -> Result<PathBuf> {
    let layout = Layout::new(options.resolve_dir()?);
    build_with(
        workspace,
        options,
        AppBuild {
            env: &layout.app_env(),
            flavour,
        },
    )
}

/// Which APK to build: the build variables to take and the flavour.
#[derive(Clone, Copy)]
pub(crate) struct AppBuild<'env> {
    /// `app.env` for the stack's server, or `second-server.env` for an APK whose built-in
    /// server is server B.
    pub(crate) env: &'env Path,
    pub(crate) flavour: Flavour,
}

/// Builds the APK `app` names and returns its path.
///
/// # Errors
///
/// When the stack cannot be prepared or Gradle fails.
pub(crate) fn build_with(
    workspace: &Path,
    options: &StackOptions,
    app: AppBuild<'_>,
) -> Result<PathBuf> {
    let AppBuild { env, flavour } = app;
    if !env.is_file() || options.fresh {
        stack::prepare(workspace, options)?;
    }
    let variables = stack::read_env_file(env)?;
    let mut command = Command::new(workspace.join("android/gradlew"));
    command
        .current_dir(workspace.join("android"))
        .arg(flavour.task());
    for (name, value) in &variables {
        command.env(name, value.as_str());
    }
    let names: Vec<&str> = variables
        .iter()
        .map(|(name, _value)| name.as_str())
        .collect();
    writeln!(
        io::stdout(),
        "local-app: ./gradlew {} with {} from {}",
        flavour.task(),
        names.join(", "),
        env.display()
    )?;
    if !command.status()?.success() {
        bail!("the debug build failed ({})", flavour.task());
    }
    Ok(workspace.join(flavour.apk()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Gradle names each flavour stands for, built from its names the way the
    /// Android plugin builds them (`android/app/build.gradle.kts`: application id
    /// `org.pingers.app`, foss adds `.foss`, debug adds `.debug`), so a renamed task, APK or
    /// package shows up here instead of as an emulator run that installs nothing.
    #[test]
    fn each_flavour_names_its_gradle_task_apk_and_package() {
        for (name, gradle_name, flavour, suffix) in [
            ("foss", "Foss", Flavour::Foss, ".foss"),
            ("gms", "Gms", Flavour::Gms, ""),
        ] {
            assert_eq!(Flavour::parse(name), Some(flavour));
            assert_eq!(flavour.task(), format!(":app:assemble{gradle_name}Debug"));
            assert_eq!(
                flavour.apk(),
                format!("android/app/build/outputs/apk/{name}/debug/app-{name}-debug.apk")
            );
            assert_eq!(flavour.package(), format!("org.pingers.app{suffix}.debug"));
        }
        assert_eq!(Flavour::parse("play"), None);
    }
}
