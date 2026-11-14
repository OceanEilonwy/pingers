// SPDX-License-Identifier: AGPL-3.0-only
//! Runs the local stack's processes: starts each in its own process group with its output in
//! a log file, waits until its port answers, notices when one exits, and on Ctrl-C (SIGINT) or
//! SIGTERM stops them all, newest first, with SIGTERM and then SIGKILL after a grace period.
//!
//! The pids are written to a file in the stack's run directory. A stack whose supervisor was
//! killed outright leaves its children running; the next start stops exactly those (a pid
//! counts only if its command line still names the stack directory), never anything else.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use crate::wait::{Deadline, pause};

/// How long a service has to open its port.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a stopped service has to exit before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// How often the supervisor looks at its children.
const POLL: Duration = Duration::from_millis(300);

/// One process of the stack.
#[derive(Debug)]
pub(crate) struct Service {
    /// A short name for logs and messages.
    pub(crate) name: &'static str,
    /// The executable.
    pub(crate) program: PathBuf,
    /// Its arguments.
    pub(crate) arguments: Vec<OsString>,
    /// The ports that answer once it is ready (none: ready at once).
    pub(crate) ready: Vec<SocketAddr>,
}

/// Set once Ctrl-C or SIGTERM arrives.
#[derive(Clone, Debug)]
pub(crate) struct StopSignal {
    received: Arc<AtomicBool>,
}

impl StopSignal {
    /// Starts watching SIGINT and SIGTERM on a thread of its own, for the rest of the process.
    ///
    /// # Errors
    ///
    /// When the runtime or a signal handler cannot be set up.
    pub(crate) fn install() -> Result<Self> {
        let received = Arc::new(AtomicBool::new(false));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .context("signal runtime")?;
        let (mut interrupt, mut terminate) = runtime.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            Ok::<_, io::Error>((
                signal(SignalKind::interrupt())?,
                signal(SignalKind::terminate())?,
            ))
        })?;
        let flag = Arc::clone(&received);
        std::thread::Builder::new()
            .name("stop-signal".into())
            .spawn(move || {
                runtime.block_on(async {
                    tokio::select! {
                        _ = interrupt.recv() => {}
                        _ = terminate.recv() => {}
                    }
                });
                flag.store(true, Ordering::SeqCst);
            })
            .context("signal thread")?;
        Ok(Self { received })
    }

    /// Whether a stop was asked for.
    pub(crate) fn received(&self) -> bool {
        self.received.load(Ordering::SeqCst)
    }
}

/// Why [`Supervisor::watch`] returned.
#[derive(Debug)]
pub(crate) enum Ending {
    /// Ctrl-C or SIGTERM.
    Stopped,
    /// A service exited by itself.
    Exited {
        /// Which.
        name: &'static str,
        /// How.
        status: ExitStatus,
    },
}

struct Running {
    name: &'static str,
    child: Child,
}

/// The running services, stopped when this is dropped.
pub(crate) struct Supervisor {
    log_dir: PathBuf,
    pid_file: PathBuf,
    marker: OsString,
    running: Vec<Running>,
}

impl Supervisor {
    /// A supervisor writing logs to `log_dir` and pids to `run_dir/stack.pids`. Leftover
    /// processes of an earlier supervisor of the same stack (`marker`, a path every one of
    /// their command lines contains) are stopped first.
    ///
    /// # Errors
    ///
    /// When the directories cannot be made.
    pub(crate) fn new(run_dir: &Path, log_dir: &Path, marker: &Path) -> Result<Self> {
        fs::create_dir_all(run_dir)?;
        fs::create_dir_all(log_dir)?;
        let supervisor = Self {
            log_dir: log_dir.to_path_buf(),
            pid_file: run_dir.join("stack.pids"),
            marker: marker.as_os_str().to_owned(),
            running: Vec::new(),
        };
        supervisor.stop_leftovers()?;
        Ok(supervisor)
    }

    /// Starts `service` and waits until its ports answer.
    ///
    /// # Errors
    ///
    /// When a port is already taken, the process cannot start, or it exits or stays silent
    /// before it is ready. Its log file is named.
    pub(crate) fn start(&mut self, service: &Service) -> Result<()> {
        for address in &service.ready {
            if answers(*address) {
                bail!(
                    "{}: {address} is already in use (another local stack, or a service of the same port)",
                    service.name
                );
            }
        }
        let log_path = self.log_dir.join(format!("{}.log", service.name));
        let log = File::create(&log_path).with_context(|| log_path.display().to_string())?;
        let child = Command::new(&service.program)
            .args(&service.arguments)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            // Its own process group: Ctrl-C reaches only the supervisor, which then stops the
            // services in order.
            .process_group(0)
            .spawn()
            .with_context(|| {
                format!(
                    "{}: cannot start {}",
                    service.name,
                    service.program.display()
                )
            })?;
        self.running.push(Running {
            name: service.name,
            child,
        });
        self.write_pids()?;
        let deadline = Deadline::after(READY_TIMEOUT);
        loop {
            if let Some(running) = self.running.last_mut()
                && let Some(status) = running.child.try_wait()?
            {
                bail!(
                    "{} exited during start ({status}); see {}",
                    service.name,
                    log_path.display()
                );
            }
            if service.ready.iter().all(|address| answers(*address)) {
                writeln!(io::stdout(), "local-stack: {} ready", service.name)?;
                return Ok(());
            }
            if deadline.passed() {
                bail!(
                    "{} did not open {:?} within {} s; see {}",
                    service.name,
                    service.ready,
                    READY_TIMEOUT.as_secs(),
                    log_path.display()
                );
            }
            pause(POLL);
        }
    }

    /// Waits until a stop is asked for or a service exits.
    ///
    /// # Errors
    ///
    /// When a child's state cannot be read.
    pub(crate) fn watch(&mut self, stop: &StopSignal) -> Result<Ending> {
        loop {
            if stop.received() {
                return Ok(Ending::Stopped);
            }
            for running in &mut self.running {
                if let Some(status) = running.child.try_wait()? {
                    return Ok(Ending::Exited {
                        name: running.name,
                        status,
                    });
                }
            }
            pause(POLL);
        }
    }

    /// Stops every service, newest first.
    pub(crate) fn stop_all(&mut self) {
        while let Some(mut running) = self.running.pop() {
            stop_child(running.name, &mut running.child);
        }
        let _ignored = fs::remove_file(&self.pid_file);
    }

    fn write_pids(&self) -> Result<()> {
        let mut text = String::new();
        for running in &self.running {
            let _infallible = writeln!(text, "{} {}", running.name, running.child.id());
        }
        fs::write(&self.pid_file, text)?;
        Ok(())
    }

    fn stop_leftovers(&self) -> Result<()> {
        let Ok(text) = fs::read_to_string(&self.pid_file) else {
            return Ok(());
        };
        for line in text.lines().rev() {
            let Some((name, pid)) = line.split_once(' ') else {
                continue;
            };
            let Ok(pid) = pid.trim().parse::<u32>() else {
                continue;
            };
            if !command_line_contains(pid, &self.marker) {
                continue;
            }
            writeln!(
                io::stdout(),
                "local-stack: stopping {name} (pid {pid}) left by an earlier run"
            )?;
            signal(pid, "TERM");
            let deadline = Deadline::after(STOP_GRACE);
            while command_line_contains(pid, &self.marker) && !deadline.passed() {
                pause(POLL);
            }
            if command_line_contains(pid, &self.marker) {
                signal(pid, "KILL");
            }
        }
        fs::remove_file(&self.pid_file)?;
        Ok(())
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// Whether something accepts TCP connections at `address`.
pub(crate) fn answers(address: SocketAddr) -> bool {
    TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok()
}

fn stop_child(name: &str, child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    signal(child.id(), "TERM");
    let deadline = Deadline::after(STOP_GRACE);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let _ignored = writeln!(io::stdout(), "local-stack: {name} stopped");
                return;
            }
            Ok(None) if !deadline.passed() => pause(POLL),
            Ok(None) | Err(_) => break,
        }
    }
    let _ignored = writeln!(
        io::stdout(),
        "local-stack: {name} did not stop within {} s; killing it",
        STOP_GRACE.as_secs()
    );
    let _ignored = child.kill();
    let _ignored = child.wait();
}

/// Sends a signal to one process with kill(1): only ever a pid this stack started.
fn signal(pid: u32, name: &str) {
    let _ignored = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn command_line_contains(pid: u32, marker: &std::ffi::OsStr) -> bool {
    let Ok(command_line) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let marker = marker.as_encoded_bytes();
    !marker.is_empty()
        && command_line
            .windows(marker.len())
            .any(|window| window == marker)
}
