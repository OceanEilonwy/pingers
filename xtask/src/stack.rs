// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask local-stack`: the tier-1 local stack (docs/deploy-nodo.md, "Local testing")
//! in one command, with fresh keys, outside the repository.
//!
//! ```text
//! cargo xtask local-stack [--dir <dir>] [--packs <dir>] [--ntfy <binary>] [--caddy <binary>]
//!                         [--fresh] [--prepare-only] [--second-server]
//! ```
//!
//! It builds the four service binaries (release), and on first use in `<dir>` generates every
//! key with the services' own `keygen` subcommands and writes the configurations, derived from
//! `packaging/nodo/dev/` so they keep the nodo templates' shape. Later runs reuse them, so an
//! app built against the stack keeps matching its pins; `--fresh` starts over. Then it runs,
//! until Ctrl-C:
//!
//! | Process | Listens on | Reached by the emulator as |
//! |---|---|---|
//! | pingers-server (API, gateway) | 127.0.0.1:9470, :9471 | `https://10.0.2.2:9443` through the TLS front |
//! | pingers-push-relay | 127.0.0.1:9472 | `https://10.0.2.2:9445` |
//! | pingers-ohttp-relay | 127.0.0.1:9473 | `https://10.0.2.2:9444/ohttp` |
//! | pingers-map-host | 127.0.0.1:9474 | `https://10.0.2.2:9446` |
//! | ntfy (UnifiedPush) | 127.0.0.1:8099 | `http://127.0.0.1:8099` through `adb reverse` |
//! | TLS front | 127.0.0.1:9443 to 9446 | |
//!
//! The TLS front is Caddy with `packaging/nodo/dev/caddy.json` when `caddy` is installed (or
//! `--caddy`), and otherwise `cargo xtask tls-front`, which serves the same file's routes
//! (see [`crate::tls_front`]). Its certificate comes from the dev CA (`cargo xtask dev-ca`'s
//! code), whose leaf covers `localhost`, `127.0.0.1` and `10.0.2.2`, the emulator's name for
//! this machine: the app reaches every service by that address, so no hosts file is needed.
//!
//! The app's build variables (hosts, `cargo xtask pins` output, the CA, the registration code)
//! are written to `<dir>/app.env` for `cargo xtask local-app`. `<dir>` defaults to
//! `$PINGERS_LOCAL_STACK`, else `$XDG_STATE_HOME/pingers/local-stack` (or
//! `~/.local/state/pingers/local-stack`). `--packs` (or `$PINGERS_TEST_PACKS`) is a directory
//! of map packs (`pingers-tiles cut` output) copied into the map host's directory once;
//! without it the map host serves an empty catalog. ntfy is `--ntfy`, `$PINGERS_NTFY` or
//! `ntfy` on the `PATH`; without it there is no UnifiedPush.
//!
//! # `--second-server`
//!
//! For Parties on different servers, a second pingers-server ("server B") runs too, with its
//! own fresh keys (`config/second-server.toml`), databases (`data/second-server/`) and ports,
//! and its own pingers-ohttp-relay (`config/second-relay.toml`) forwarding to B's gateway:
//!
//! | Process | Listens on | Reached by the emulator as |
//! |---|---|---|
//! | pingers-server B (API, gateway) | 127.0.0.1:9475, :9476 | `https://10.0.2.2:9447` |
//! | pingers-ohttp-relay B | 127.0.0.1:9477 | `https://10.0.2.2:9448/ohttp` |
//!
//! Both are behind the same TLS front (the stack's `caddy.json` gains two sites copied from
//! A's, so Caddy and `cargo xtask tls-front` serve them alike) and the same dev CA. B
//! advertises its own relay in `/v1/params`, the same push relay and map host as A, and the
//! operator [`SECOND_OPERATOR`]. The push relay lists B as a second allowed server with a
//! credential of its own. B requires a registration code of its own, so an app can exercise
//! "code required" and "code refused"; A is unchanged. B's origin, relay, pins and code are
//! written to `<dir>/second-server.env`, in `app.env`'s names. `--fresh` regenerates B too.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, anyhow, bail};
use toml::{Table, Value};
use zeroize::Zeroizing;

use crate::Outcome;
use crate::supervise::{Ending, Service, StopSignal, Supervisor, answers};

/// The emulator's address for this machine.
pub(crate) const HOST_FROM_EMULATOR: &str = "10.0.2.2";

/// The TLS front's ports, as `packaging/nodo/dev/caddy.json` has them.
const FRONT_SERVER: u16 = 9443;
const FRONT_OHTTP: u16 = 9444;
const FRONT_PUSH: u16 = 9445;
const FRONT_MAPS: u16 = 9446;
/// Server B's TLS front ports (`--second-server`): its API and its OHTTP relay.
const FRONT_SECOND_SERVER: u16 = 9447;
const FRONT_SECOND_OHTTP: u16 = 9448;
/// Server B's own ports on loopback: API, gateway, OHTTP relay.
const SECOND_SERVER_PORT: u16 = 9475;
const SECOND_GATEWAY_PORT: u16 = 9476;
const SECOND_RELAY_PORT: u16 = 9477;
/// Who runs server B, as its `/v1/params` publishes it.
pub(crate) const SECOND_OPERATOR: &str = "Second server (local)";
/// Server B's name in the push relay's `[[servers]]`.
const SECOND_PUSH_SERVER_ID: &str = "local-second";
/// ntfy's port, on loopback here and (through `adb reverse`) on each emulator.
pub(crate) const NTFY_PORT: u16 = 8099;

/// The four service binaries, in start order.
const SERVICE_PACKAGES: [&str; 4] = [
    "pingers-push-relay",
    "pingers-server",
    "pingers-ohttp-relay",
    "pingers-map-host",
];

/// What the command line asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StackOptions {
    /// Where everything is written; see [`StackOptions::resolve`] for the default.
    pub(crate) dir: Option<PathBuf>,
    /// Map packs to serve.
    pub(crate) packs: Option<PathBuf>,
    /// The ntfy server binary.
    pub(crate) ntfy: Option<PathBuf>,
    /// The Caddy binary.
    pub(crate) caddy: Option<PathBuf>,
    /// Throw away the keys and data of an earlier run.
    pub(crate) fresh: bool,
    /// Write keys and configurations, print the pins, and stop there.
    pub(crate) prepare_only: bool,
    /// Also run server B with its own OHTTP relay.
    pub(crate) second_server: bool,
}

impl StackOptions {
    /// Parses `local-stack`'s options; `None` for anything not understood.
    pub(crate) fn parse(options: &[String]) -> Option<Self> {
        let mut parsed = Self::default();
        let mut options = options.iter();
        while let Some(option) = options.next() {
            if !parsed.take(option, &mut options)? {
                return None;
            }
        }
        Some(parsed)
    }

    /// Takes one option shared by every local command, and its value from `rest`. `Some(false)`
    /// when `option` is not one of them, `None` when its value is missing.
    pub(crate) fn take<'option>(
        &mut self,
        option: &str,
        rest: &mut impl Iterator<Item = &'option String>,
    ) -> Option<bool> {
        match option {
            "--dir" => self.dir = Some(PathBuf::from(rest.next()?)),
            "--packs" => self.packs = Some(PathBuf::from(rest.next()?)),
            "--ntfy" => self.ntfy = Some(PathBuf::from(rest.next()?)),
            "--caddy" => self.caddy = Some(PathBuf::from(rest.next()?)),
            "--fresh" => self.fresh = true,
            "--prepare-only" => self.prepare_only = true,
            "--second-server" => self.second_server = true,
            _ => return Some(false),
        }
        Some(true)
    }

    /// The stack directory: `--dir`, `$PINGERS_LOCAL_STACK`, or the user's state directory.
    ///
    /// # Errors
    ///
    /// When none of them is set (no `HOME`).
    pub(crate) fn resolve_dir(&self) -> Result<PathBuf> {
        if let Some(dir) = &self.dir {
            return absolute(dir);
        }
        if let Some(dir) = environment("PINGERS_LOCAL_STACK") {
            return absolute(Path::new(&dir));
        }
        let state = environment("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| environment("HOME").map(|home| Path::new(&home).join(".local/state")))
            .context("set --dir or PINGERS_LOCAL_STACK (no HOME)")?;
        Ok(state.join("pingers").join("local-stack"))
    }
}

/// The stack directory's layout.
#[derive(Clone, Debug)]
pub(crate) struct Layout {
    root: PathBuf,
}

impl Layout {
    /// The layout under `root`.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The stack directory itself.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    fn config(&self) -> PathBuf {
        self.root.join("config")
    }
    fn ca(&self) -> PathBuf {
        self.root.join("ca")
    }
    fn data(&self) -> PathBuf {
        self.root.join("data")
    }
    fn maps(&self) -> PathBuf {
        self.root.join("maps")
    }
    /// The services' log files.
    pub(crate) fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    fn run(&self) -> PathBuf {
        self.root.join("run")
    }
    /// The app's build variables, one `NAME=value` per line.
    pub(crate) fn app_env(&self) -> PathBuf {
        self.root.join("app.env")
    }
    /// The emulator scenario's screenshots, logcat and report.
    pub(crate) fn e2e_out(&self) -> PathBuf {
        self.root.join("e2e-out")
    }
    fn server_config(&self) -> PathBuf {
        self.config().join("server.toml")
    }
    fn push_relay_config(&self) -> PathBuf {
        self.config().join("push-relay.toml")
    }
    fn relay_config(&self) -> PathBuf {
        self.config().join("relay.toml")
    }
    fn map_host_config(&self) -> PathBuf {
        self.config().join("map-host.toml")
    }
    fn caddy_config(&self) -> PathBuf {
        self.config().join("caddy.json")
    }
    fn ntfy_config(&self) -> PathBuf {
        self.config().join("ntfy.yml")
    }
    fn second_server_config(&self) -> PathBuf {
        self.config().join("second-server.toml")
    }
    fn second_relay_config(&self) -> PathBuf {
        self.config().join("second-relay.toml")
    }
    fn second_data(&self) -> PathBuf {
        self.data().join("second-server")
    }
    /// Server B's values, one `NAME=value` per line, in `app.env`'s names.
    pub(crate) fn second_server_env(&self) -> PathBuf {
        self.root.join("second-server.env")
    }
    /// The dev CA's certificate, for `PINGERS_EXTRA_CA`.
    pub(crate) fn ca_pem(&self) -> PathBuf {
        self.ca().join("ca.pem")
    }
}

/// The stack, prepared: binaries built, keys and configurations written.
#[derive(Debug)]
pub(crate) struct Prepared {
    /// Where it lives.
    pub(crate) layout: Layout,
    binaries: PathBuf,
    ntfy: Option<PathBuf>,
    caddy: Option<PathBuf>,
    second_server: bool,
}

/// `cargo xtask local-stack`.
///
/// # Errors
///
/// When the stack cannot be prepared or a service cannot start.
pub(crate) fn run(workspace: &Path, options: &StackOptions) -> Result<Outcome> {
    let prepared = prepare(workspace, options)?;
    if options.prepare_only {
        return Ok(Outcome::Passed);
    }
    let stop = StopSignal::install()?;
    let mut supervisor = start(&prepared)?;
    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "local-stack: up. Logs in {}. Build the app with `cargo xtask local-app`. Ctrl-C stops everything.",
        prepared.layout.logs().display()
    )?;
    drop(stdout);
    let ending = supervisor.watch(&stop)?;
    supervisor.stop_all();
    match ending {
        Ending::Stopped => {
            writeln!(io::stdout(), "local-stack: stopped")?;
            Ok(Outcome::Passed)
        }
        Ending::Exited { name, status } => {
            writeln!(
                io::stderr(),
                "local-stack: {name} exited ({status}); see {}",
                prepared.layout.logs().join(format!("{name}.log")).display()
            )?;
            Ok(Outcome::Failed)
        }
    }
}

/// Whether the TLS front of a stack already answers on this machine.
pub(crate) fn front_is_up() -> bool {
    [FRONT_SERVER, FRONT_OHTTP, FRONT_PUSH, FRONT_MAPS]
        .into_iter()
        .all(|port| answers(loopback(port)))
}

/// Builds the binaries and writes every key, configuration and the app's variables.
///
/// # Errors
///
/// When a build, a keygen or a file write fails.
pub(crate) fn prepare(workspace: &Path, options: &StackOptions) -> Result<Prepared> {
    let layout = Layout::new(options.resolve_dir()?);
    if is_inside(layout.root(), workspace) {
        bail!(
            "{} is inside the repository; the stack's keys must live elsewhere",
            layout.root().display()
        );
    }
    if options.fresh {
        for dir in [layout.config(), layout.ca(), layout.data(), layout.run()] {
            if dir.exists() {
                fs::remove_dir_all(&dir)?;
            }
        }
        let _ignored = fs::remove_file(layout.app_env());
        let _ignored = fs::remove_file(layout.second_server_env());
    }
    for dir in [
        layout.config(),
        layout.data(),
        layout.maps(),
        layout.logs(),
        layout.run(),
    ] {
        fs::create_dir_all(&dir)?;
    }
    fs::set_permissions(layout.config(), fs::Permissions::from_mode(0o700))?;

    let binaries = build_services(workspace)?;
    let mut stdout = io::stdout().lock();

    if !layout.ca_pem().is_file() {
        let names = crate::nodo::write_dev_ca(&layout.ca(), &[])?;
        writeln!(
            stdout,
            "local-stack: dev CA in {} for {}",
            layout.ca().display(),
            names.join(", ")
        )?;
    }

    let dev = workspace.join("packaging/nodo/dev");
    let registration_code = write_keys(&dev, &binaries, &layout)?;
    fs::copy(dev.join("relay.toml"), layout.relay_config())?;
    fs::write(layout.map_host_config(), map_host_config(&dev, &layout)?)?;
    let mut caddy_text = caddy_config(&dev, &layout)?;
    if options.second_server {
        caddy_text = with_second_server_sites(&caddy_text)?;
    }
    fs::write(layout.caddy_config(), caddy_text)?;
    fs::write(layout.ntfy_config(), ntfy_config(&layout))?;
    copy_packs(options, &layout)?;

    let pins = crate::pins::run(&layout.server_config(), &layout.push_relay_config())?;
    writeln!(
        stdout,
        "local-stack: cargo xtask pins {} {}\n{pins}",
        layout.server_config().display(),
        layout.push_relay_config().display()
    )?;
    let app_env = app_env(&layout, &pins, &registration_code);
    write_secret_file(&layout.app_env(), &app_env)?;
    writeln!(
        stdout,
        "local-stack: app build variables in {}",
        layout.app_env().display()
    )?;
    drop(stdout);
    if options.second_server {
        prepare_second_server(&dev, &binaries, &layout)?;
    }

    let ntfy = options
        .ntfy
        .clone()
        .or_else(|| environment("PINGERS_NTFY").map(PathBuf::from))
        .or_else(|| on_path("ntfy"));
    let caddy = options.caddy.clone().or_else(|| on_path("caddy"));
    Ok(Prepared {
        layout,
        binaries,
        ntfy,
        caddy,
        second_server: options.second_server,
    })
}

/// Writes server B's configuration and relay, adds B to the push relay's servers, and writes
/// `second-server.env`. Keys of an earlier run are reused while B's configuration and its push
/// relay entry are both there.
fn prepare_second_server(dev: &Path, binaries: &Path, layout: &Layout) -> Result<()> {
    fs::create_dir_all(layout.second_data())?;
    let push_relay = Zeroizing::new(
        fs::read_to_string(layout.push_relay_config())
            .with_context(|| layout.push_relay_config().display().to_string())?,
    );
    let code = if layout.second_server_config().is_file() && has_second_server(&push_relay)? {
        writeln!(
            io::stdout(),
            "local-stack: reusing server B's keys in {}",
            layout.second_server_config().display()
        )?;
        read_registration_code(&layout.second_server_config())?
    } else {
        let credential = keygen(binaries, "pingers-push-relay", &["keygen"])?;
        write_secret_file(
            &layout.push_relay_config(),
            &with_second_server(&push_relay, &credential)?,
        )?;
        let push_pin = crate::pins::push_relay_pins(&push_relay)?
            .first()
            .map(pingers_proto::KeyConfigHash::to_pin)
            .context("the push relay has no key")?;
        let code = keygen(binaries, "pingers-push-relay", &["keygen"])?;
        let server = server_config(
            dev,
            &ServerSecrets {
                binaries,
                push_pin: &push_pin,
                credential: &credential,
                registration_code: &code,
            },
            &layout.second_data(),
            Shape::Second,
        )?;
        write_secret_file(&layout.second_server_config(), &server)?;
        writeln!(
            io::stdout(),
            "local-stack: new keys for server B in {}",
            layout.second_server_config().display()
        )?;
        code
    };
    fs::write(layout.second_relay_config(), second_relay_config(dev)?)?;
    let pins = crate::pins::run(&layout.second_server_config(), &layout.push_relay_config())?;
    write_secret_file(
        &layout.second_server_env(),
        &second_server_env(layout, &pins, &code),
    )?;
    writeln!(
        io::stdout(),
        "local-stack: server B ({SECOND_OPERATOR}) at https://{HOST_FROM_EMULATOR}:{FRONT_SECOND_SERVER}, \
         relay https://{HOST_FROM_EMULATOR}:{FRONT_SECOND_OHTTP}/ohttp; its values in {}",
        layout.second_server_env().display()
    )?;
    Ok(())
}

/// Writes the push relay and server configurations with fresh keys, unless an earlier run
/// did, and returns the server's registration code.
fn write_keys(dev: &Path, binaries: &Path, layout: &Layout) -> Result<Zeroizing<String>> {
    if layout.server_config().is_file() && layout.push_relay_config().is_file() {
        writeln!(
            io::stdout(),
            "local-stack: reusing the keys in {} (--fresh makes new ones)",
            layout.config().display()
        )?;
        return read_registration_code(&layout.server_config());
    }
    let push_relay = push_relay_config(dev, binaries)?;
    write_secret_file(&layout.push_relay_config(), &push_relay)?;
    let push_pin = crate::pins::push_relay_pins(&push_relay)?
        .first()
        .map(pingers_proto::KeyConfigHash::to_pin)
        .context("the push relay has no key")?;
    let credential = relay_credential(&push_relay)?;
    let code = keygen(binaries, "pingers-push-relay", &["keygen"])?;
    let server = server_config(
        dev,
        &ServerSecrets {
            binaries,
            push_pin: &push_pin,
            credential: &credential,
            registration_code: &code,
        },
        &layout.data(),
        Shape::First,
    )?;
    write_secret_file(&layout.server_config(), &server)?;
    writeln!(
        io::stdout(),
        "local-stack: new keys in {}",
        layout.config().display()
    )?;
    Ok(code)
}

/// Starts every process of a prepared stack.
///
/// # Errors
///
/// When one cannot start; those already started are stopped when the supervisor drops.
pub(crate) fn start(prepared: &Prepared) -> Result<Supervisor> {
    let layout = &prepared.layout;
    let mut supervisor = Supervisor::new(&layout.run(), &layout.logs(), layout.root())?;
    let mut stdout = io::stdout().lock();
    if let Some(ntfy) = &prepared.ntfy {
        drop(stdout);
        supervisor.start(&Service {
            name: "ntfy",
            program: ntfy.clone(),
            arguments: os_strings([
                OsStr::new("serve"),
                OsStr::new("--config"),
                layout.ntfy_config().as_os_str(),
            ]),
            ready: vec![loopback(NTFY_PORT)],
        })?;
    } else {
        writeln!(
            stdout,
            "local-stack: no ntfy (--ntfy, PINGERS_NTFY or PATH); running without UnifiedPush"
        )?;
        drop(stdout);
    }
    let services = [
        (
            "pingers-push-relay",
            layout.push_relay_config(),
            vec![loopback(9472)],
        ),
        (
            "pingers-server",
            layout.server_config(),
            vec![loopback(9470), loopback(9471)],
        ),
        (
            "pingers-ohttp-relay",
            layout.relay_config(),
            vec![loopback(9473)],
        ),
        (
            "pingers-map-host",
            layout.map_host_config(),
            vec![loopback(9474)],
        ),
    ];
    for (name, config, ready) in services {
        supervisor.start(&Service {
            name,
            program: prepared.binaries.join(name),
            arguments: vec![config.into_os_string()],
            ready,
        })?;
    }
    let mut front_ports = vec![FRONT_SERVER, FRONT_OHTTP, FRONT_PUSH, FRONT_MAPS];
    if prepared.second_server {
        start_second_server(prepared, &mut supervisor)?;
        front_ports.extend([FRONT_SECOND_SERVER, FRONT_SECOND_OHTTP]);
    }
    let front_ready = front_ports.into_iter().map(loopback).collect::<Vec<_>>();
    if let Some(caddy) = &prepared.caddy {
        supervisor.start(&Service {
            name: "caddy",
            program: caddy.clone(),
            arguments: os_strings([
                OsStr::new("run"),
                OsStr::new("--config"),
                layout.caddy_config().as_os_str(),
            ]),
            ready: front_ready,
        })?;
    } else {
        writeln!(
            io::stdout(),
            "local-stack: Caddy is not installed; using `cargo xtask tls-front` (the same caddy.json routes, h2 and HTTP/1.1 in, h2c streamed to the server and relay)"
        )?;
        supervisor.start(&Service {
            name: "tls-front",
            program: std::env::current_exe().context("the xtask binary")?,
            arguments: os_strings([OsStr::new("tls-front"), layout.caddy_config().as_os_str()]),
            ready: front_ready,
        })?;
    }
    Ok(supervisor)
}

/// Starts server B and its OHTTP relay.
fn start_second_server(prepared: &Prepared, supervisor: &mut Supervisor) -> Result<()> {
    let layout = &prepared.layout;
    let second = [
        (
            "pingers-server-second",
            "pingers-server",
            layout.second_server_config(),
            vec![loopback(SECOND_SERVER_PORT), loopback(SECOND_GATEWAY_PORT)],
        ),
        (
            "pingers-ohttp-relay-second",
            "pingers-ohttp-relay",
            layout.second_relay_config(),
            vec![loopback(SECOND_RELAY_PORT)],
        ),
    ];
    for (name, binary, config, ready) in second {
        supervisor.start(&Service {
            name,
            program: prepared.binaries.join(binary),
            arguments: vec![config.into_os_string()],
            ready,
        })?;
    }
    Ok(())
}

/// The app's build variables, as `NAME=value` lines (also the format of `app.env`).
fn app_env(layout: &Layout, pins: &str, registration_code: &str) -> Zeroizing<String> {
    let host = HOST_FROM_EMULATOR;
    let mut text = Zeroizing::new(format!(
        "# Written by `cargo xtask local-stack`: the app's build variables for this local stack.\n\
         PINGERS_SERVER_HOST=https://{host}:{FRONT_SERVER}\n\
         PINGERS_OHTTP_RELAY=https://{host}:{FRONT_OHTTP}/ohttp\n\
         PINGERS_PUSH_RELAY=https://{host}:{FRONT_PUSH}\n\
         PINGERS_MAP_HOST=https://{host}:{FRONT_MAPS}\n\
         PINGERS_EXTRA_CA={}\n",
        layout.ca_pem().display()
    ));
    text.push_str(pins);
    text.push_str("PINGERS_DEV_REGISTRATION_CODE=");
    text.push_str(registration_code);
    text.push('\n');
    text
}

/// Reads `app.env`.
///
/// # Errors
///
/// When the file cannot be read or a line is not `NAME=value`.
pub(crate) fn read_app_env(layout: &Layout) -> Result<Vec<(String, Zeroizing<String>)>> {
    let path = layout.app_env();
    let text = Zeroizing::new(
        fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?,
    );
    let mut variables = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, value) = line
            .split_once('=')
            .with_context(|| format!("{}: a line is not NAME=value", path.display()))?;
        variables.push((name.to_owned(), Zeroizing::new(value.to_owned())));
    }
    Ok(variables)
}

fn build_services(workspace: &Path) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(cargo);
    command
        .current_dir(workspace)
        .args(["build", "--release", "--quiet"]);
    for package in SERVICE_PACKAGES {
        command.args(["-p", package]);
    }
    writeln!(
        io::stdout(),
        "local-stack: building {}",
        SERVICE_PACKAGES.join(", ")
    )?;
    if !command.status()?.success() {
        bail!("building the services failed");
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| workspace.join("target"), PathBuf::from);
    Ok(absolute(&target)?.join("release"))
}

/// Runs a service binary's keygen subcommand and returns the key it printed.
fn keygen(binaries: &Path, binary: &str, arguments: &[&str]) -> Result<Zeroizing<String>> {
    let output = Command::new(binaries.join(binary))
        .args(arguments)
        .output()
        .with_context(|| format!("{binary} {}", arguments.join(" ")))?;
    if !output.status.success() {
        bail!("{binary} {} failed", arguments.join(" "));
    }
    let printed = Zeroizing::new(
        String::from_utf8(output.stdout).map_err(|_error| anyhow!("{binary} printed non-UTF-8"))?,
    );
    let key = Zeroizing::new(printed.trim().to_owned());
    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!(
            "{binary} {} did not print 64 hex digits",
            arguments.join(" ")
        );
    }
    Ok(key)
}

fn parse_template(path: &Path) -> Result<Table> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    text.parse::<Table>()
        .with_context(|| format!("{} is not TOML", path.display()))
}

fn table_mut<'table>(table: &'table mut Table, key: &str) -> Result<&'table mut Table> {
    table
        .get_mut(key)
        .and_then(Value::as_table_mut)
        .with_context(|| format!("the dev template has no [{key}]"))
}

fn entries_mut<'table>(table: &'table mut Table, key: &str) -> Result<&'table mut Vec<Value>> {
    table
        .get_mut(key)
        .and_then(Value::as_array_mut)
        .with_context(|| format!("the dev template has no [[{key}]]"))
}

fn set_secret(entry: &mut Value, field: &str, secret: &str) -> Result<()> {
    entry
        .as_table_mut()
        .context("an entry is not a table")?
        .insert(field.to_owned(), Value::String(secret.to_owned()));
    Ok(())
}

/// The dev push relay configuration with fresh keys, hash key and server credential.
fn push_relay_config(dev: &Path, binaries: &Path) -> Result<Zeroizing<String>> {
    let mut table = parse_template(&dev.join("push-relay.toml"))?;
    let hash_key = keygen(binaries, "pingers-push-relay", &["keygen"])?;
    table.insert("hash_key".to_owned(), Value::String(hash_key.to_string()));
    for key in entries_mut(&mut table, "keys")? {
        let private_key = keygen(binaries, "pingers-push-relay", &["keygen"])?;
        set_secret(key, "private_key", &private_key)?;
    }
    let credential = keygen(binaries, "pingers-push-relay", &["keygen"])?;
    for server in entries_mut(&mut table, "servers")? {
        set_secret(server, "credential", &credential)?;
    }
    Ok(with_header(&table))
}

/// The server credential the push relay configuration issues.
fn relay_credential(push_relay: &str) -> Result<Zeroizing<String>> {
    let table = Zeroizing::new(push_relay.to_owned()).parse::<Table>()?;
    table
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(|server| server.get("credential"))
        .and_then(Value::as_str)
        .map(|credential| Zeroizing::new(credential.to_owned()))
        .context("the push relay configuration has no server credential")
}

struct ServerSecrets<'values> {
    binaries: &'values Path,
    push_pin: &'values str,
    credential: &'values str,
    registration_code: &'values str,
}

/// Which of the stack's servers a configuration is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Server A: the dev template's addresses as they are.
    First,
    /// Server B: its own ports, origin, relay and operator ([`make_second_server`]).
    Second,
}

/// The dev server configuration with fresh gateway keys, the push relay's credential and
/// pin, a registration code, and its databases in `data`.
fn server_config(
    dev: &Path,
    secrets: &ServerSecrets<'_>,
    data: &Path,
    shape: Shape,
) -> Result<Zeroizing<String>> {
    let mut table = parse_template(&dev.join("server.toml"))?;
    if shape == Shape::Second {
        make_second_server(&mut table)?;
    }
    for (field, file) in [
        ("delivery_db", "groups.sqlite"),
        ("quota_db", "quota.sqlite"),
        ("mailbox_db", "mailboxes.sqlite"),
    ] {
        table.insert(
            field.to_owned(),
            Value::String(data.join(file).display().to_string()),
        );
    }
    table.insert(
        "registration_codes".to_owned(),
        Value::Array(vec![Value::String(secrets.registration_code.to_owned())]),
    );
    for key in entries_mut(table_mut(&mut table, "ohttp")?, "keys")? {
        let secret = keygen(secrets.binaries, "pingers-server", &["keygen", "ohttp"])?;
        set_secret(key, "secret", &secret)?;
    }
    for relay in entries_mut(&mut table, "push_relays")? {
        set_secret(relay, "credential", secrets.credential)?;
        set_secret(relay, "key_config_hash", secrets.push_pin)?;
    }
    Ok(with_header(&table))
}

/// Turns the dev server template into server B's: its own listen and gateway ports, origin,
/// advertised OHTTP relay and operator. The push relay and map host stay A's.
fn make_second_server(table: &mut Table) -> Result<()> {
    let host = HOST_FROM_EMULATOR;
    for (field, value) in [
        ("listen", format!("127.0.0.1:{SECOND_SERVER_PORT}")),
        ("server_id", format!("https://{host}:{FRONT_SECOND_SERVER}")),
        ("server_operator", SECOND_OPERATOR.to_owned()),
    ] {
        table.insert(field.to_owned(), Value::String(value));
    }
    table.insert(
        "ohttp_relay_urls".to_owned(),
        Value::Array(vec![Value::String(format!(
            "https://{host}:{FRONT_SECOND_OHTTP}/ohttp"
        ))]),
    );
    table_mut(table, "ohttp")?.insert(
        "listen".to_owned(),
        Value::String(format!("127.0.0.1:{SECOND_GATEWAY_PORT}")),
    );
    Ok(())
}

/// Server B's OHTTP relay: the dev relay template forwarding to B's gateway.
fn second_relay_config(dev: &Path) -> Result<String> {
    let mut table = parse_template(&dev.join("relay.toml"))?;
    for (field, value) in [
        ("listen", format!("127.0.0.1:{SECOND_RELAY_PORT}")),
        (
            "gateway",
            format!("http://127.0.0.1:{SECOND_GATEWAY_PORT}/ohttp"),
        ),
        (
            "gateway_key_config",
            format!("http://127.0.0.1:{SECOND_GATEWAY_PORT}/v1/ohttp/config"),
        ),
    ] {
        if !table.contains_key(field) {
            bail!("packaging/nodo/dev/relay.toml no longer has {field}");
        }
        table.insert(field.to_owned(), Value::String(value));
    }
    Ok(with_header(&table).to_string())
}

/// Whether a push relay configuration lists server B.
fn has_second_server(push_relay: &str) -> Result<bool> {
    let table = Zeroizing::new(push_relay.to_owned()).parse::<Table>()?;
    Ok(table
        .get("servers")
        .and_then(Value::as_array)
        .is_some_and(|servers| servers.iter().any(is_second_server)))
}

fn is_second_server(server: &Value) -> bool {
    server.get("server_id").and_then(Value::as_str) == Some(SECOND_PUSH_SERVER_ID)
}

/// The push relay configuration with server B allowed, with `credential` (replacing an earlier
/// entry for B); the other servers and keys are kept.
fn with_second_server(push_relay: &str, credential: &str) -> Result<Zeroizing<String>> {
    let mut table = Zeroizing::new(push_relay.to_owned()).parse::<Table>()?;
    let servers = entries_mut(&mut table, "servers")?;
    let limit = servers
        .first()
        .and_then(|first| first.get("pushes_per_minute"))
        .cloned()
        .context("the push relay's first server has no pushes_per_minute")?;
    servers.retain(|server| !is_second_server(server));
    let mut entry = Table::new();
    entry.insert(
        "server_id".to_owned(),
        Value::String(SECOND_PUSH_SERVER_ID.to_owned()),
    );
    entry.insert(
        "credential".to_owned(),
        Value::String(credential.to_owned()),
    );
    entry.insert("pushes_per_minute".to_owned(), limit);
    servers.push(Value::Table(entry));
    Ok(with_header(&table))
}

/// Server B's values for an app or a test, as `NAME=value` lines in `app.env`'s names.
fn second_server_env(layout: &Layout, pins: &str, registration_code: &str) -> Zeroizing<String> {
    let host = HOST_FROM_EMULATOR;
    let mut text = Zeroizing::new(format!(
        "# Written by `cargo xtask local-stack --second-server`: server B of this local stack.\n\
         PINGERS_SERVER_HOST=https://{host}:{FRONT_SECOND_SERVER}\n\
         PINGERS_OHTTP_RELAY=https://{host}:{FRONT_SECOND_OHTTP}/ohttp\n\
         PINGERS_SERVER_OPERATOR={SECOND_OPERATOR}\n\
         PINGERS_PUSH_RELAY=https://{host}:{FRONT_PUSH}\n\
         PINGERS_MAP_HOST=https://{host}:{FRONT_MAPS}\n\
         PINGERS_EXTRA_CA={}\n",
        layout.ca_pem().display()
    ));
    text.push_str(pins);
    text.push_str("PINGERS_DEV_REGISTRATION_CODE=");
    text.push_str(registration_code);
    text.push('\n');
    text
}

/// A Caddy JSON configuration of the dev shape with server B's two sites added: copies of
/// A's `server` and `ohttp` sites on B's front ports, forwarding to B's API and relay.
///
/// # Errors
///
/// When the configuration is not JSON or lacks either site or its upstream.
pub(crate) fn with_second_server_sites(caddy: &str) -> Result<String> {
    let mut config: serde_json::Value = serde_json::from_str(caddy)?;
    let servers = config
        .pointer_mut("/apps/http/servers")
        .and_then(serde_json::Value::as_object_mut)
        .context("caddy.json has no apps.http.servers")?;
    for (from, to, front, upstream) in [
        (
            "server",
            "second-server",
            FRONT_SECOND_SERVER,
            SECOND_SERVER_PORT,
        ),
        (
            "ohttp",
            "second-ohttp",
            FRONT_SECOND_OHTTP,
            SECOND_RELAY_PORT,
        ),
    ] {
        let mut site = servers
            .get(from)
            .cloned()
            .with_context(|| format!("caddy.json has no {from} site"))?;
        let listen = site.get_mut("listen").context("a site has no listen")?;
        *listen = serde_json::json!([format!("127.0.0.1:{front}")]);
        let mut dials = 0_usize;
        for route in site
            .get_mut("routes")
            .and_then(serde_json::Value::as_array_mut)
            .with_context(|| format!("the {from} site has no routes"))?
        {
            for handler in route
                .get_mut("handle")
                .and_then(serde_json::Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                for target in handler
                    .get_mut("upstreams")
                    .and_then(serde_json::Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    if let Some(dial) = target.get_mut("dial") {
                        *dial = serde_json::Value::String(format!("127.0.0.1:{upstream}"));
                        dials = dials.saturating_add(1);
                    }
                }
            }
        }
        if dials == 0 {
            bail!("the {from} site forwards nowhere");
        }
        servers.insert(to.to_owned(), site);
    }
    Ok(serde_json::to_string_pretty(&config)?)
}

fn read_registration_code(server: &Path) -> Result<Zeroizing<String>> {
    let text = Zeroizing::new(fs::read_to_string(server)?);
    let table = text.parse::<Table>()?;
    table
        .get("registration_codes")
        .and_then(Value::as_array)
        .and_then(|codes| codes.first())
        .and_then(Value::as_str)
        .map(|code| Zeroizing::new(code.to_owned()))
        .with_context(|| format!("{} has no registration code", server.display()))
}

fn map_host_config(dev: &Path, layout: &Layout) -> Result<String> {
    let mut table = parse_template(&dev.join("map-host.toml"))?;
    table.insert(
        "directory".to_owned(),
        Value::String(layout.maps().display().to_string()),
    );
    // Every pack a test may copy in, whatever its region.
    table.insert("hosted".to_owned(), Value::String("oceania".to_owned()));
    Ok(with_header(&table).to_string())
}

/// `packaging/nodo/dev/caddy.json` with the certificate paths pointed at the stack's CA.
fn caddy_config(dev: &Path, layout: &Layout) -> Result<String> {
    let template = fs::read_to_string(dev.join("caddy.json"))?;
    let mut text = template.clone();
    for (from, to) in [
        ("target/dev-ca/leaf.pem", layout.ca().join("leaf.pem")),
        ("target/dev-ca/leaf.key", layout.ca().join("leaf.key")),
    ] {
        if !text.contains(from) {
            bail!("packaging/nodo/dev/caddy.json no longer names {from}");
        }
        text = text.replace(from, &to.display().to_string());
    }
    Ok(text)
}

fn ntfy_config(layout: &Layout) -> String {
    format!(
        "# Written by `cargo xtask local-stack`: a local ntfy for UnifiedPush. Emulators reach it\n\
         # at the same address through `adb reverse tcp:{NTFY_PORT} tcp:{NTFY_PORT}`.\n\
         base-url: \"http://127.0.0.1:{NTFY_PORT}\"\n\
         listen-http: \"127.0.0.1:{NTFY_PORT}\"\n\
         cache-file: \"{}\"\n\
         auth-default-access: \"read-write\"\n",
        layout.data().join("ntfy-cache.db").display()
    )
}

fn copy_packs(options: &StackOptions, layout: &Layout) -> Result<()> {
    let Some(packs) = options
        .packs
        .clone()
        .or_else(|| environment("PINGERS_TEST_PACKS").map(PathBuf::from))
    else {
        return Ok(());
    };
    if fs::read_dir(layout.maps())?.next().is_some() {
        return Ok(());
    }
    let mut copied = 0_usize;
    for entry in fs::read_dir(&packs).with_context(|| packs.display().to_string())? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::copy(entry.path(), layout.maps().join(entry.file_name()))?;
            copied = copied.saturating_add(1);
        }
    }
    writeln!(
        io::stdout(),
        "local-stack: copied {copied} map pack files from {}",
        packs.display()
    )?;
    Ok(())
}

fn with_header(table: &Table) -> Zeroizing<String> {
    let mut text = Zeroizing::new(String::from(
        "# Written by `cargo xtask local-stack` from packaging/nodo/dev, with fresh keys.\n\
         # A local test stack only: never a deployment.\n",
    ));
    text.push_str(&table.to_string());
    text
}

fn write_secret_file(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn os_strings<const N: usize>(arguments: [&OsStr; N]) -> Vec<OsString> {
    arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect()
}

/// A non-empty environment variable.
pub(crate) fn environment(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// `program` found in a `PATH` directory.
pub(crate) fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).with_context(|| path.display().to_string())
}

fn is_inside(path: &Path, dir: &Path) -> bool {
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut probe = path.to_path_buf();
    // The stack directory may not exist yet: compare its nearest existing ancestor.
    while !probe.exists() {
        if !probe.pop() {
            break;
        }
    }
    canonical(&probe).starts_with(canonical(dir))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn strings(options: &[&str]) -> Vec<String> {
        options.iter().map(|option| (*option).to_owned()).collect()
    }

    #[test]
    fn options_take_paths_and_flags() {
        assert_eq!(
            StackOptions::parse(&strings(&["--dir", "/s", "--fresh", "--packs", "/p"])),
            Some(StackOptions {
                dir: Some(PathBuf::from("/s")),
                packs: Some(PathBuf::from("/p")),
                fresh: true,
                ..StackOptions::default()
            })
        );
        assert_eq!(StackOptions::parse(&strings(&["--dir"])), None);
        assert_eq!(StackOptions::parse(&strings(&["--other"])), None);
    }

    #[test]
    fn the_dev_templates_fill_in() -> Result<()> {
        let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../packaging/nodo/dev");
        let layout = Layout::new(PathBuf::from("/stack"));
        let caddy = caddy_config(&dev, &layout)?;
        assert!(caddy.contains("/stack/ca/leaf.pem"));
        assert!(!caddy.contains("target/dev-ca"));
        let maps = map_host_config(&dev, &layout)?;
        assert!(maps.contains("directory = \"/stack/maps\""));
        Ok(())
    }

    #[test]
    fn app_env_lists_every_build_variable() {
        let layout = Layout::new(PathBuf::from("/stack"));
        let text = app_env(
            &layout,
            "PINGERS_GATEWAY_KEY_PINS=a,b\nPINGERS_PUSH_RELAY_KEY_PINS=c,d\n",
            "code",
        );
        for name in [
            "PINGERS_SERVER_HOST=https://10.0.2.2:9443",
            "PINGERS_OHTTP_RELAY=https://10.0.2.2:9444/ohttp",
            "PINGERS_PUSH_RELAY=https://10.0.2.2:9445",
            "PINGERS_MAP_HOST=https://10.0.2.2:9446",
            "PINGERS_EXTRA_CA=/stack/ca/ca.pem",
            "PINGERS_GATEWAY_KEY_PINS=a,b",
            "PINGERS_DEV_REGISTRATION_CODE=code",
        ] {
            assert!(text.contains(name), "{name}");
        }
    }

    fn dev() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../packaging/nodo/dev")
    }

    fn text<'table>(table: &'table Table, field: &str) -> Option<&'table str> {
        table.get(field).and_then(Value::as_str)
    }

    #[test]
    fn the_second_server_flag_is_an_option() {
        assert_eq!(
            StackOptions::parse(&strings(&["--second-server", "--fresh"])),
            Some(StackOptions {
                second_server: true,
                fresh: true,
                ..StackOptions::default()
            })
        );
    }

    #[test]
    fn server_b_has_its_own_ports_origin_relay_and_operator() -> Result<()> {
        let template = parse_template(&dev().join("server.toml"))?;
        let mut table = template.clone();
        make_second_server(&mut table)?;
        assert_eq!(text(&table, "listen"), Some("127.0.0.1:9475"));
        assert_eq!(text(&table, "server_id"), Some("https://10.0.2.2:9447"));
        assert_eq!(text(&table, "server_operator"), Some(SECOND_OPERATOR));
        assert_eq!(
            table.get("ohttp_relay_urls"),
            Some(&Value::Array(vec![Value::String(
                "https://10.0.2.2:9448/ohttp".to_owned()
            )]))
        );
        assert_eq!(
            table
                .get("ohttp")
                .and_then(|ohttp| ohttp.get("listen"))
                .and_then(Value::as_str),
            Some("127.0.0.1:9476")
        );
        // The publisher's push relay and map host are A's.
        for field in ["map_host", "push_relays", "registration_codes"] {
            assert_eq!(table.get(field), template.get(field), "{field}");
        }
        // A's ports are not B's.
        assert_ne!(text(&template, "listen"), text(&table, "listen"));
        Ok(())
    }

    #[test]
    fn server_b_relay_forwards_to_b_gateway() -> Result<()> {
        let table = second_relay_config(&dev())?.parse::<Table>()?;
        assert_eq!(text(&table, "listen"), Some("127.0.0.1:9477"));
        assert_eq!(text(&table, "gateway"), Some("http://127.0.0.1:9476/ohttp"));
        assert_eq!(
            text(&table, "gateway_key_config"),
            Some("http://127.0.0.1:9476/v1/ohttp/config")
        );
        Ok(())
    }

    #[test]
    fn the_push_relay_allows_server_b_with_its_own_credential() -> Result<()> {
        let template = fs::read_to_string(dev().join("push-relay.toml"))?;
        assert!(!has_second_server(&template)?);
        let once = with_second_server(&template, "first-credential")?;
        assert!(has_second_server(&once)?);
        // Again: B's entry is replaced, not repeated; A's stays first.
        let twice = with_second_server(&once, "second-credential")?;
        let table = twice.parse::<Table>()?;
        let servers = table
            .get("servers")
            .and_then(Value::as_array)
            .context("servers")?;
        let ids: Vec<_> = servers
            .iter()
            .filter_map(|server| server.get("server_id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["local", SECOND_PUSH_SERVER_ID]);
        assert_eq!(
            servers
                .get(1)
                .and_then(|server| server.get("credential"))
                .and_then(Value::as_str),
            Some("second-credential")
        );
        // A's credential (the first server's) is unchanged.
        assert_eq!(*relay_credential(&twice)?, *relay_credential(&template)?);
        // The keys are kept.
        assert_eq!(
            crate::pins::push_relay_pins(&twice)?,
            crate::pins::push_relay_pins(&template)?
        );
        Ok(())
    }

    #[test]
    fn second_server_env_lists_b_values() {
        let layout = Layout::new(PathBuf::from("/stack"));
        let text = second_server_env(
            &layout,
            "PINGERS_GATEWAY_KEY_PINS=a,b\nPINGERS_PUSH_RELAY_KEY_PINS=c,d\n",
            "b-code",
        );
        for line in [
            "PINGERS_SERVER_HOST=https://10.0.2.2:9447",
            "PINGERS_OHTTP_RELAY=https://10.0.2.2:9448/ohttp",
            "PINGERS_SERVER_OPERATOR=Second server (local)",
            "PINGERS_PUSH_RELAY=https://10.0.2.2:9445",
            "PINGERS_MAP_HOST=https://10.0.2.2:9446",
            "PINGERS_EXTRA_CA=/stack/ca/ca.pem",
            "PINGERS_GATEWAY_KEY_PINS=a,b",
            "PINGERS_PUSH_RELAY_KEY_PINS=c,d",
            "PINGERS_DEV_REGISTRATION_CODE=b-code",
        ] {
            assert!(text.lines().any(|candidate| candidate == line), "{line}");
        }
    }

    #[test]
    fn the_front_gains_server_b_sites_and_keeps_a() -> Result<()> {
        let layout = Layout::new(PathBuf::from("/stack"));
        let plain = caddy_config(&dev(), &layout)?;
        let with_b: serde_json::Value = serde_json::from_str(&with_second_server_sites(&plain)?)?;
        let plain: serde_json::Value = serde_json::from_str(&plain)?;
        for site in ["server", "ohttp", "push", "maps"] {
            let pointer = format!("/apps/http/servers/{site}");
            assert_eq!(with_b.pointer(&pointer), plain.pointer(&pointer), "{site}");
        }
        for (site, listen, dial) in [
            ("second-server", "127.0.0.1:9447", "127.0.0.1:9475"),
            ("second-ohttp", "127.0.0.1:9448", "127.0.0.1:9477"),
        ] {
            let base = format!("/apps/http/servers/{site}");
            assert_eq!(
                with_b
                    .pointer(&format!("{base}/listen/0"))
                    .and_then(serde_json::Value::as_str),
                Some(listen)
            );
            let routes = with_b
                .pointer(&format!("{base}/routes"))
                .and_then(serde_json::Value::as_array)
                .context("routes")?;
            let dials: Vec<_> = routes
                .iter()
                .filter_map(|route| route.pointer("/handle/0/upstreams/0/dial"))
                .filter_map(serde_json::Value::as_str)
                .collect();
            assert_eq!(dials, vec![dial], "{site}");
        }
        // B's server hides its gateway and metrics like A's.
        assert_eq!(
            with_b.pointer("/apps/http/servers/second-server/routes/0"),
            plain.pointer("/apps/http/servers/server/routes/0")
        );
        Ok(())
    }

    #[test]
    fn a_directory_inside_the_repository_is_refused() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(is_inside(&workspace.join("target/stack"), workspace));
        assert!(!is_inside(Path::new("/nonexistent-stack"), workspace));
    }
}
