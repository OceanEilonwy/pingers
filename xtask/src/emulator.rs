// SPDX-License-Identifier: AGPL-3.0-only
//! Emulators for the scenario, under the machine's rules: at most two, each held through a
//! lease file locked with flock(2) (`<leases>/.emulator-lease-1` and `-2`, the files other
//! tools on this machine lock too), always started with `-gpu host -accel on`, checked to
//! render on the host GPU rather than `SwiftShader`, and shut down (`adb emu kill`) when
//! dropped. Only emulators this process started are ever stopped.
//!
//! Each runs an AVD with `-read-only -no-snapshot`, so two can share one AVD, a run starts
//! from a cold boot, and nothing a run does is kept.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use crate::stack::environment;
use crate::supervise::answers;
use crate::wait::{Deadline, pause};

/// How long an emulator has to boot.
const BOOT_TIMEOUT: Duration = Duration::from_secs(300);
/// How long to wait for both leases before giving up.
const LEASE_TIMEOUT: Duration = Duration::from_mins(30);
/// The first console port tried; ports are even, each with the next odd one for adb.
const FIRST_PORT: u16 = 5600;
const LAST_PORT: u16 = 5680;

/// The Android SDK tools the scenario uses.
#[derive(Clone, Debug)]
pub(crate) struct Sdk {
    /// `emulator/emulator`.
    pub(crate) emulator: PathBuf,
    /// `platform-tools/adb`.
    pub(crate) adb: PathBuf,
}

impl Sdk {
    /// The SDK named by `ANDROID_HOME` or `ANDROID_SDK_ROOT`.
    ///
    /// # Errors
    ///
    /// When neither is set or the tools are missing.
    pub(crate) fn find() -> Result<Self> {
        let root = environment("ANDROID_HOME")
            .or_else(|| environment("ANDROID_SDK_ROOT"))
            .map(PathBuf::from)
            .context("set ANDROID_HOME to the Android SDK")?;
        let sdk = Self {
            emulator: root.join("emulator/emulator"),
            adb: root.join("platform-tools/adb"),
        };
        for tool in [&sdk.emulator, &sdk.adb] {
            if !tool.is_file() {
                bail!("{} is missing", tool.display());
            }
        }
        Ok(sdk)
    }
}

/// One held lease: the lock lasts as long as the open file.
#[derive(Debug)]
pub(crate) struct Lease {
    _file: File,
    /// Which lease file.
    pub(crate) path: PathBuf,
}

/// Takes both leases, waiting while another tool holds either.
///
/// # Errors
///
/// When a lease file cannot be opened, or both are not free within half an hour.
pub(crate) fn take_leases(dir: &Path) -> Result<[Lease; 2]> {
    let deadline = Deadline::after(LEASE_TIMEOUT);
    let mut told = false;
    loop {
        let first = try_lease(&dir.join(".emulator-lease-1"))?;
        let second = try_lease(&dir.join(".emulator-lease-2"))?;
        if let (Some(first), Some(second)) = (first, second) {
            return Ok([first, second]);
        }
        if deadline.passed() {
            bail!(
                "the emulator leases in {} stayed taken for {} min",
                dir.display(),
                LEASE_TIMEOUT.as_secs().div_euclid(60)
            );
        }
        if !told {
            writeln!(
                io::stdout(),
                "e2e: waiting for both emulator leases in {}",
                dir.display()
            )?;
            told = true;
        }
        pause(Duration::from_secs(15));
    }
}

fn try_lease(path: &Path) -> Result<Option<Lease>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| path.display().to_string())?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Lease {
            _file: file,
            path: path.to_path_buf(),
        })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// What to start.
#[derive(Debug)]
pub(crate) struct EmulatorSpec<'spec> {
    /// The SDK tools.
    pub(crate) sdk: &'spec Sdk,
    /// The AVD.
    pub(crate) avd: &'spec str,
    /// The lease this emulator runs under.
    pub(crate) lease: Lease,
    /// The emulator's own output.
    pub(crate) log: PathBuf,
}

/// A running emulator this process started.
#[derive(Debug)]
pub(crate) struct Emulator {
    /// `emulator-<port>`.
    pub(crate) serial: String,
    adb: PathBuf,
    child: Child,
    lease: Lease,
}

impl Emulator {
    /// Starts an emulator and waits for it to finish booting.
    ///
    /// # Errors
    ///
    /// When no console port is free, it cannot start, or it does not boot in time.
    pub(crate) fn start(spec: EmulatorSpec<'_>, taken: &[u16]) -> Result<Self> {
        let port = free_port(spec.sdk, taken)?;
        let serial = format!("emulator-{port}");
        let log = File::create(&spec.log).with_context(|| spec.log.display().to_string())?;
        writeln!(
            io::stdout(),
            "e2e: starting {} as {serial} under {} (-gpu host -accel on)",
            spec.avd,
            spec.lease.path.display()
        )?;
        let child = Command::new(&spec.sdk.emulator)
            .args(["-avd", spec.avd, "-port", &port.to_string()])
            .args(["-read-only", "-no-snapshot", "-no-boot-anim", "-no-audio"])
            .args(["-no-window", "-gpu", "host", "-accel", "on"])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("cannot start the emulator")?;
        let mut emulator = Self {
            serial,
            adb: spec.sdk.adb.clone(),
            child,
            lease: spec.lease,
        };
        emulator.wait_for_boot(&spec.log)?;
        Ok(emulator)
    }

    /// The console port, from the serial.
    pub(crate) fn port(&self) -> Option<u16> {
        self.serial.strip_prefix("emulator-")?.parse().ok()
    }

    fn wait_for_boot(&mut self, log: &Path) -> Result<()> {
        let deadline = Deadline::after(BOOT_TIMEOUT);
        loop {
            if let Some(status) = self.child.try_wait()? {
                bail!(
                    "{} exited while booting ({status}); see {}",
                    self.serial,
                    log.display()
                );
            }
            let booted = Command::new(&self.adb)
                .args(["-s", &self.serial, "shell", "getprop", "sys.boot_completed"])
                .stderr(Stdio::null())
                .output()
                .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "1");
            if booted {
                writeln!(io::stdout(), "e2e: {} booted", self.serial)?;
                return Ok(());
            }
            if deadline.passed() {
                bail!(
                    "{} did not boot within {} s; see {}",
                    self.serial,
                    BOOT_TIMEOUT.as_secs(),
                    log.display()
                );
            }
            pause(Duration::from_secs(3));
        }
    }

    /// The `GLES` lines of `dumpsys SurfaceFlinger`, checked to name an AMD GPU and not
    /// `SwiftShader`.
    ///
    /// # Errors
    ///
    /// When dumpsys fails, or the renderer is not the host's AMD GPU.
    pub(crate) fn check_gpu(&self) -> Result<String> {
        let output = Command::new(&self.adb)
            .args(["-s", &self.serial, "shell", "dumpsys", "SurfaceFlinger"])
            .output()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = text.lines().filter(|line| line.contains("GLES")).collect();
        let joined = lines.join("\n");
        let upper = joined.to_ascii_uppercase();
        if upper.contains("SWIFTSHADER") {
            bail!(
                "{} renders with SwiftShader, not the host GPU:\n{joined}",
                self.serial
            );
        }
        if !(upper.contains("AMD") || upper.contains("RADEON")) {
            bail!("{} does not report the AMD GPU:\n{joined}", self.serial);
        }
        Ok(joined)
    }

    /// Stops the emulator and waits for it to exit.
    pub(crate) fn shutdown(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ignored = Command::new(&self.adb)
            .args(["-s", &self.serial, "emu", "kill"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Deadline::after(Duration::from_secs(60));
        while matches!(self.child.try_wait(), Ok(None)) && !deadline.passed() {
            pause(Duration::from_millis(500));
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ignored = self.child.kill();
            let _ignored = self.child.wait();
        }
        let _ignored = writeln!(
            io::stdout(),
            "e2e: {} shut down; {} released",
            self.serial,
            self.lease.path.display()
        );
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// An even console port whose pair is free and not a device adb already knows.
fn free_port(sdk: &Sdk, taken: &[u16]) -> Result<u16> {
    let devices = Command::new(&sdk.adb).arg("devices").output()?;
    let devices = String::from_utf8_lossy(&devices.stdout);
    let mut port = FIRST_PORT;
    while port <= LAST_PORT {
        let next = port.saturating_add(1);
        let busy = taken.contains(&port)
            || devices.contains(&format!("emulator-{port}"))
            || answers(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
            || answers(std::net::SocketAddr::from(([127, 0, 0, 1], next)));
        if !busy {
            return Ok(port);
        }
        port = port.saturating_add(2);
    }
    bail!("no free emulator console port between {FIRST_PORT} and {LAST_PORT}")
}
