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

/// The APK `assembleFossDebug` writes, relative to the workspace.
const APK: &str = "android/app/build/outputs/apk/foss/debug/app-foss-debug.apk";

/// `cargo xtask local-app`.
///
/// # Errors
///
/// When the stack cannot be prepared or Gradle fails.
pub(crate) fn run(workspace: &Path, options: &StackOptions) -> Result<Outcome> {
    let apk = build(workspace, options)?;
    writeln!(io::stdout(), "local-app: {}", apk.display())?;
    Ok(Outcome::Passed)
}

/// Builds the APK and returns its path.
///
/// # Errors
///
/// When the stack cannot be prepared or Gradle fails.
pub(crate) fn build(workspace: &Path, options: &StackOptions) -> Result<PathBuf> {
    let layout = Layout::new(options.resolve_dir()?);
    if !layout.app_env().is_file() || options.fresh {
        stack::prepare(workspace, options)?;
    }
    let variables = stack::read_app_env(&layout)?;
    let mut command = Command::new(workspace.join("android/gradlew"));
    command
        .current_dir(workspace.join("android"))
        .arg(":app:assembleFossDebug");
    for (name, value) in &variables {
        command.env(name, value.as_str());
    }
    let names: Vec<&str> = variables
        .iter()
        .map(|(name, _value)| name.as_str())
        .collect();
    writeln!(
        io::stdout(),
        "local-app: ./gradlew :app:assembleFossDebug with {} from {}",
        names.join(", "),
        layout.app_env().display()
    )?;
    if !command.status()?.success() {
        bail!("the foss debug build failed");
    }
    Ok(workspace.join(APK))
}
