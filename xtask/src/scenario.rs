// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask e2e-emulators`: the two-emulator scenario against the local stack.
//!
//! ```text
//! cargo xtask e2e-emulators [<local-stack options>] [--avd <name>] [--leases <dir>]
//!                           [--apk <file>] [--ntfy-apk <file>] [--flavour foss|gms]
//!                           [--cross-server]
//! ```
//!
//! With `--second-server --cross-server` phone A's app is built with `second-server.env`, so its
//! built-in server is server B, and phone B's with `app.env` (server A). A creates a Party (on
//! B) and shares its link; B opens it, records server B with the pins the link carries, registers
//! there with the link's registration code and asks to join; server B's metrics must show it
//! answered. Then the whole scenario below runs with the Party on server B, and B's Party
//! settings must name server B's operator and say its keys came from the invite link.
//!
//! It uses a running local stack, or starts one (and stops it at the end); builds the debug
//! APK (foss, or gms with `--flavour gms`) against it unless `--apk` names one; takes both
//! emulator leases (`--leases`,
//! `$PINGERS_EMULATOR_LEASES`, else the stack directory) and boots two emulators of `--avd`
//! (`$PINGERS_E2E_AVD`, default `Pixel_9_Pro`) with `-gpu host -accel on`, checking that both
//! render on the AMD GPU. If an ntfy app is installed, or `--ntfy-apk` (`$PINGERS_NTFY_APK`)
//! provides one, its default server is pointed at the stack's ntfy (through `adb reverse`)
//! before Pingers registers for UnifiedPush; otherwise ntfy is removed so nothing registers
//! with a public server.
//!
//! Then, on phone A (who invites) and phone B (who joins), driven from here with adb and
//! UI Automator, step by step:
//!
//! 1. A creates a Party and shares its invite link (https://<server>/j#v1.…); B opens it as a
//!    tap would (a VIEW intent naming no app).
//! 2. Flow A (design canvas `JoinA1` to `JoinA3`, `InviteApprove`): B asks to join under a name and
//!    reads its safety code; A sees the request with a code; the codes must match; A lets B
//!    in; B opens the Party. Then B opens the link in Chrome, whose landing page (`GET /j`)
//!    hands it to Pingers with "Open in Pingers".
//! 3. A creates a Spot; B sees it.
//! 4. Both "walk in" (`adb emu geo fix`, repeated so the entry rule's two fixes 30 s apart
//!    come in), a Meetup forms and both open it.
//! 5. A sends a chat message, which B receives.
//! 6. B asks to see A's exact location, A allows, and B's compass points at A.
//! 7. Both walk out and the Meetup ends on both phones.
//!
//! Pushes are checked where they arrive (`dumpsys notification`): A is notified of B's join
//! request, both of the Meetup, and B, sent to the launcher first, of A's chat message.
//! With `--flavour gms` against a stack started with `--fcm-service-account`, one push also
//! goes from the local push relay through FCM to a phone ([`crate::fcm_push`]), and its core
//! must take it.
//!
//! Each step names what it waits for. The first failure stops the run and says which step
//! failed and why; screenshots, UI dumps and logcat of both phones are saved in
//! `<stack dir>/e2e-out/`, with `report.txt`. The emulators are shut down and the leases
//! released however the run ends.
//!
//! The Spot editor opens on the map's default centre (0°, 0°) until it centres on the phone,
//! so the scenario stands A there before the Spot is made and walks everyone in around it.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use crate::Outcome;
use crate::device::{Device, Find, Position, Text, safety_code};
use crate::emulator::{Emulator, EmulatorSpec, Sdk, take_leases};
use crate::local_app::Flavour;
use crate::stack::{self, Layout, NTFY_PORT, StackOptions, environment};
use crate::supervise::Supervisor;
use crate::wait::{Deadline, pause};

/// The main activity, after the application id.
const ACTIVITY: &str = "org.pingers.app.MainActivity";
/// The host of the stack's server as the emulators reach it (its invite links' host).
const STACK_HOST: &str = "10.0.2.2";
/// The browser on the Play images.
const CHROME: &str = "com.android.chrome";
/// The ntfy app, the UnifiedPush distributor on the emulators.
const NTFY_PACKAGE: &str = "io.heckel.ntfy";

const PARTY: &str = "Climbing crew";
const JOINER: &str = "Ben";
const SPOT: &str = "Crag";
const MESSAGE: &str = "Hello from A";

/// Where the Spot is made (see the module comment).
const SPOT_CENTRE: Position = Position {
    latitude: 0.0,
    longitude: 0.0,
};
/// About 25 m from the centre, inside the default 150 m circle, about 45 m apart.
const A_INSIDE: Position = Position {
    latitude: 0.0002,
    longitude: -0.0001,
};
const B_INSIDE: Position = Position {
    latitude: -0.0001,
    longitude: 0.0002,
};
/// About 4.7 km away.
const AWAY: Position = Position {
    latitude: 0.03,
    longitude: 0.03,
};

/// Waits for things that happen on the phone alone.
const UI: Duration = Duration::from_secs(20);
/// Waits for things that need the server (sync, a message).
const NETWORK: Duration = Duration::from_secs(120);
/// Waits for presence (the entry rule, wakes, the knock) and for the Meetup to end.
const PRESENCE: Duration = Duration::from_secs(360);

/// What the command line asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScenarioOptions {
    stack: StackOptions,
    avd: Option<String>,
    leases: Option<PathBuf>,
    apk: Option<PathBuf>,
    ntfy_apk: Option<PathBuf>,
    flavour: Flavour,
    /// `--cross-server`: phone A's built-in server is server B (`--second-server`), phone B's
    /// is server A; B joins A's Party on server B by its invite, and the scenario runs there.
    cross_server: bool,
}

impl ScenarioOptions {
    /// Parses `e2e-emulators`' options; `None` for anything not understood.
    pub(crate) fn parse(options: &[String]) -> Option<Self> {
        let mut parsed = Self::default();
        let mut options = options.iter();
        while let Some(option) = options.next() {
            if parsed.stack.take(option, &mut options)? {
                continue;
            }
            match option.as_str() {
                "--avd" => parsed.avd = Some(options.next()?.clone()),
                "--leases" => parsed.leases = Some(PathBuf::from(options.next()?)),
                "--apk" => parsed.apk = Some(PathBuf::from(options.next()?)),
                "--ntfy-apk" => parsed.ntfy_apk = Some(PathBuf::from(options.next()?)),
                "--flavour" => parsed.flavour = Flavour::parse(options.next()?)?,
                "--cross-server" => parsed.cross_server = true,
                _ => return None,
            }
        }
        Some(parsed)
    }
}

/// How one step ended.
#[derive(Debug)]
enum StepOutcome {
    Passed(String),
    Failed(String),
}

#[derive(Debug)]
struct StepRecord {
    number: usize,
    name: &'static str,
    outcome: StepOutcome,
}

/// The run's record, written to `report.txt` as it goes.
struct Report {
    out: PathBuf,
    steps: Vec<StepRecord>,
    notes: Vec<String>,
}

impl Report {
    fn note(&mut self, note: String) {
        let _ignored = writeln!(io::stdout(), "e2e: {note}");
        self.notes.push(note);
        self.save();
    }

    fn record(&mut self, name: &'static str, outcome: StepOutcome) -> usize {
        let number = self.steps.len().saturating_add(1);
        let line = match &outcome {
            StepOutcome::Passed(detail) => format!("step {number:02} {name}: passed. {detail}"),
            StepOutcome::Failed(reason) => format!("step {number:02} {name}: FAILED. {reason}"),
        };
        let _ignored = writeln!(io::stdout(), "e2e: {line}");
        self.steps.push(StepRecord {
            number,
            name,
            outcome,
        });
        self.save();
        number
    }

    fn text(&self) -> String {
        let mut text =
            String::from("Pingers two-emulator scenario (cargo xtask e2e-emulators)\n\n");
        for note in &self.notes {
            let _infallible = writeln!(text, "- {note}");
        }
        text.push('\n');
        for step in &self.steps {
            let _infallible = match &step.outcome {
                StepOutcome::Passed(detail) => {
                    writeln!(
                        text,
                        "{:02} {:<28} passed  {detail}",
                        step.number, step.name
                    )
                }
                StepOutcome::Failed(reason) => {
                    writeln!(
                        text,
                        "{:02} {:<28} FAILED  {reason}",
                        step.number, step.name
                    )
                }
            };
        }
        let furthest = self
            .steps
            .iter()
            .rev()
            .find(|step| matches!(step.outcome, StepOutcome::Passed(_)));
        let _infallible = match (furthest, self.failure()) {
            (_, Some(failed)) => writeln!(
                text,
                "\nResult: failed at step {:02} {}. Furthest step passed: {}.",
                failed.number,
                failed.name,
                furthest.map_or_else(
                    || "none".to_owned(),
                    |step| format!("{:02} {}", step.number, step.name)
                )
            ),
            (Some(step), None) => {
                writeln!(
                    text,
                    "\nResult: passed, through step {:02} {}.",
                    step.number, step.name
                )
            }
            (None, None) => writeln!(text, "\nResult: nothing ran."),
        };
        text
    }

    fn failure(&self) -> Option<&StepRecord> {
        self.steps
            .iter()
            .find(|step| matches!(step.outcome, StepOutcome::Failed(_)))
    }

    fn save(&self) {
        let _ignored = fs::write(self.out.join("report.txt"), self.text());
    }
}

/// What the steps share.
struct Scene {
    out: PathBuf,
    /// The application id of the build under test.
    package: &'static str,
    /// The stack's push relay configuration when the build is gms, for the FCM step.
    gms_push_relay: Option<PathBuf>,
    a: Device,
    b: Device,
    link: Option<String>,
    code_b: Option<String>,
}

/// A failed step, already recorded with its evidence.
#[derive(Debug)]
struct Stopped;

/// Runs one step: records it, and on failure saves both phones' evidence.
fn step(
    report: &mut Report,
    scene: &mut Scene,
    name: &'static str,
    body: impl FnOnce(&mut Scene) -> Result<String>,
) -> Result<(), Stopped> {
    match body(scene) {
        Ok(detail) => {
            let number = report.steps.len().saturating_add(1);
            for device in [&scene.a, &scene.b] {
                device.screenshot(&scene.out, &format!("{number:02}-{name}"));
            }
            report.record(name, StepOutcome::Passed(detail));
            Ok(())
        }
        Err(error) => {
            let number = report.steps.len().saturating_add(1);
            let stem = format!("{number:02}-{name}-FAILED");
            let mut saved = Vec::new();
            for device in [&scene.a, &scene.b] {
                saved.extend(device.evidence(&scene.out, &stem));
            }
            report.record(name, StepOutcome::Failed(format!("{error:#}")));
            report.note(format!(
                "evidence of step {number:02}: {}",
                saved
                    .iter()
                    .filter_map(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            Err(Stopped)
        }
    }
}

/// `cargo xtask e2e-emulators`.
///
/// # Errors
///
/// When the output directory cannot be written; every other problem is a failed step.
pub(crate) fn run(workspace: &Path, options: &ScenarioOptions) -> Result<Outcome> {
    let layout = Layout::new(options.stack.resolve_dir()?);
    let out = layout.e2e_out();
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(&out)?;
    let mut report = Report {
        out: out.clone(),
        steps: Vec::new(),
        notes: Vec::new(),
    };
    let outcome = match setup_and_play(workspace, options, &layout, &mut report) {
        Ok(()) => Outcome::Passed,
        Err(_stopped) => Outcome::Failed,
    };
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "\n{}", report.text())?;
    writeln!(stdout, "e2e: artifacts in {}", out.display())?;
    Ok(if report.failure().is_some() {
        Outcome::Failed
    } else {
        outcome
    })
}

/// Everything that holds resources (the stack, the emulators) lives in this frame, so it is
/// all released, emulators first, however the scenario ends.
#[expect(
    clippy::too_many_lines,
    reason = "setup reads top to bottom as the steps it reports"
)]
fn setup_and_play(
    workspace: &Path,
    options: &ScenarioOptions,
    layout: &Layout,
    report: &mut Report,
) -> Result<(), Stopped> {
    // The stack: one already running, or one of our own for this run.
    let mut own_stack: Option<Supervisor> = None;
    setup_step(report, "local stack up", || {
        if stack::front_is_up() {
            if options.cross_server && !stack::second_front_is_up() {
                bail!(
                    "a stack without server B already holds the ports: stop it, or run without --cross-server"
                );
            }
            return Ok("using the stack already running".to_owned());
        }
        let prepared = stack::prepare(workspace, &options.stack)?;
        own_stack = Some(stack::start(&prepared)?);
        Ok(format!(
            "started for this run; logs in {}",
            prepared.layout.logs().display()
        ))
    })?;

    let mut apk = PathBuf::new();
    setup_step(report, "app built for the stack", || {
        apk = match &options.apk {
            Some(apk) => apk.clone(),
            None => crate::local_app::build(workspace, &options.stack, options.flavour)?,
        };
        if !apk.is_file() {
            bail!("{} does not exist", apk.display());
        }
        Ok(apk.display().to_string())
    })?;
    // Phone A's app, built-in server B, when the run crosses servers; phone B keeps `apk`.
    let mut apk_a = apk.clone();
    if options.cross_server {
        setup_step(report, "app built for server B", || {
            if !options.stack.second_server {
                bail!("--cross-server needs --second-server");
            }
            // Gradle writes one path: keep the first build before the second replaces it.
            let kept = layout.e2e_out().join("app-built-in-a.apk");
            fs::copy(&apk, &kept)?;
            apk = kept;
            apk_a = crate::local_app::build_with(
                workspace,
                &options.stack,
                crate::local_app::AppBuild {
                    env: &layout.second_server_env(),
                    flavour: options.flavour,
                },
            )?;
            Ok(format!(
                "A: {} (built-in server B); B: {} (built-in server A)",
                apk_a.display(),
                apk.display()
            ))
        })?;
    }

    let mut emulators: Vec<Emulator> = Vec::new();
    let mut sdk = None;
    setup_step(report, "two emulators booted", || {
        let found = Sdk::find()?;
        let lease_dir = options
            .leases
            .clone()
            .or_else(|| environment("PINGERS_EMULATOR_LEASES").map(PathBuf::from))
            .unwrap_or_else(|| layout.root().to_path_buf());
        let avd = options
            .avd
            .clone()
            .or_else(|| environment("PINGERS_E2E_AVD").and_then(|avd| avd.into_string().ok()))
            .unwrap_or_else(|| "Pixel_9_Pro".to_owned());
        let leases = take_leases(&lease_dir)?;
        for (index, lease) in leases.into_iter().enumerate() {
            let taken: Vec<u16> = emulators.iter().filter_map(Emulator::port).collect();
            emulators.push(Emulator::start(
                EmulatorSpec {
                    sdk: &found,
                    avd: &avd,
                    lease,
                    log: layout.e2e_out().join(format!(
                        "emulator-{}.log",
                        if index == 0 { "A" } else { "B" }
                    )),
                },
                &taken,
            )?);
        }
        sdk = Some(found);
        Ok(format!(
            "{} from AVD {avd}, leases in {}",
            emulators
                .iter()
                .map(|emulator| emulator.serial.as_str())
                .collect::<Vec<_>>()
                .join(" and "),
            lease_dir.display()
        ))
    })?;

    let mut gpu = String::new();
    setup_step(report, "GPU is the host's AMD GPU", || {
        for emulator in &emulators {
            let lines = emulator.check_gpu()?;
            let _infallible = writeln!(gpu, "{}: {}", emulator.serial, lines.trim());
        }
        Ok(gpu.trim().replace('\n', " | "))
    })?;
    report.note(format!(
        "dumpsys SurfaceFlinger | grep GLES:\n{}",
        gpu.trim()
    ));

    let (Some(sdk), [first, second]) = (sdk, emulators.as_slice()) else {
        report.record(
            "two emulators booted",
            StepOutcome::Failed("fewer than two emulators".to_owned()),
        );
        return Err(Stopped);
    };
    let mut scene = Scene {
        out: layout.e2e_out(),
        package: options.flavour.package(),
        gms_push_relay: (options.flavour == Flavour::Gms).then(|| layout.push_relay_config()),
        a: Device::new("A", first.serial.clone(), sdk.adb.clone()),
        b: Device::new("B", second.serial.clone(), sdk.adb.clone()),
        link: None,
        code_b: None,
    };
    let ntfy_apk = options
        .ntfy_apk
        .clone()
        .or_else(|| environment("PINGERS_NTFY_APK").map(PathBuf::from));
    let apks = Apks {
        a: &apk_a,
        b: &apk,
        cross_server: options.cross_server,
    };
    let result = play(report, &mut scene, &apks, ntfy_apk.as_deref());
    // Emulators go first, then the stack.
    drop(emulators);
    drop(own_stack);
    result
}

/// A step before the phones exist: no evidence but the message.
fn setup_step(
    report: &mut Report,
    name: &'static str,
    body: impl FnOnce() -> Result<String>,
) -> Result<(), Stopped> {
    match body() {
        Ok(detail) => {
            report.record(name, StepOutcome::Passed(detail));
            Ok(())
        }
        Err(error) => {
            report.record(name, StepOutcome::Failed(format!("{error:#}")));
            Err(Stopped)
        }
    }
}

/// The app each phone gets.
struct Apks<'path> {
    a: &'path Path,
    b: &'path Path,
    /// Phone A's built-in server is server B: stop once B has joined there.
    cross_server: bool,
}

#[expect(
    clippy::too_many_lines,
    reason = "the scenario reads top to bottom as the steps it reports"
)]
fn play(
    report: &mut Report,
    scene: &mut Scene,
    apks: &Apks<'_>,
    ntfy_apk: Option<&Path>,
) -> Result<(), Stopped> {
    step(report, scene, "ntfy points at the stack", |scene| {
        let mut details = Vec::new();
        for device in [&scene.a, &scene.b] {
            details.push(format!(
                "{}: {}",
                device.label,
                setup_ntfy(device, ntfy_apk)?
            ));
        }
        Ok(details.join("; "))
    })?;

    step(report, scene, "Pingers installed", |scene| {
        install_pingers(&scene.a, apks.a, scene.package)?;
        install_pingers(&scene.b, apks.b, scene.package)?;
        Ok(format!(
            "{} installed, permissions granted, Home shown on both",
            scene.package
        ))
    })?;

    // 1. Create a Party and invite.
    step(report, scene, "A creates a Party", |scene| {
        let a = &scene.a;
        a.tap_when(
            &Find::tag("home_new_party").or(Text::Is("New party".into())),
            UI,
        )?;
        let field = a.tap_when(&Find::text(Text::Is("Party name".into())).field(), UI)?;
        a.type_text(PARTY)?;
        let create = Find::tag("new_party_create").enabled();
        if let Err(error) = a.tap_when(&create, UI) {
            bail!(
                "Create party never became enabled after typing the name into {:?}: {error:#}",
                field.label()
            );
        }
        a.wait_for(&Find::text(Text::Is("Invite people".into())), NETWORK)?;
        Ok(format!("{PARTY:?} created; Invite people shown"))
    })?;

    step(
        report,
        scene,
        "a UnifiedPush message wakes A's core",
        |scene| {
            let a = &scene.a;
            let endpoint = unifiedpush_endpoint(a, scene.package)?;
            a.shell("logcat -c")?;
            // A content-free push naming no handle of A's: the core wakes, reads nothing, shows
            // nothing. What a real one leads to is checked at the Meetup and chat steps.
            let sent = std::process::Command::new("curl")
                .args([
                    "-fsS",
                    "-X",
                    "POST",
                    "-d",
                    r#"{"v":1,"h":"0000000000000000"}"#,
                ])
                .arg(endpoint.replace("10.0.2.2", "127.0.0.1"))
                .output()
                .context("running curl")?;
            if !sent.status.success() {
                bail!(
                    "ntfy refused the push: {}",
                    String::from_utf8_lossy(&sent.stderr)
                );
            }
            let deadline = Deadline::after(NETWORK);
            loop {
                let log = a.shell("logcat -d -s PingersCore")?;
                if let Some(line) = log
                    .lines()
                    .find(|line| line.contains("onPush UNIFIED_PUSH"))
                {
                    return Ok(format!("A's logcat: {}", line.trim()));
                }
                if deadline.passed() {
                    bail!("the push never reached A's core (no onPush in logcat)");
                }
                pause(Duration::from_secs(1));
            }
        },
    )?;

    if scene.gms_push_relay.is_some() {
        step(
            report,
            scene,
            "an FCM message through the push relay wakes a core",
            fcm_wakes_the_core,
        )?;
    }

    step(report, scene, "A shares the invite link", |scene| {
        let a = &scene.a;
        // The first invite on a server registers its mailbox, and the first Party there fetched a
        // whole token batch: on an emulator's debug build that can take minutes, while the
        // "Making an invite link" animation keeps uiautomator from dumping the screen.
        a.tap_when(&Find::tag("invite_share").enabled(), PRESENCE)
            .context("the invite link was never made (Share invite link stayed disabled)")?;
        // The share sheet previews the text it shares: the link itself, https://<server>/j#v1.…
        let preview = a.wait_for(&Find::text(Text::Contains("/j#v1.".into())), UI)?;
        let link = preview
            .label()
            .split_whitespace()
            .find(|word| word.starts_with("https://") && word.contains("/j#v1."))
            .context("no https invite link in the share sheet's preview")?
            .to_owned();
        a.back()?;
        let length = link.len();
        scene.link = Some(link);
        Ok(format!(
            "https://…/j#v1.… ({length} characters) read from the share sheet"
        ))
    })?;

    let mut before = 0;
    if apks.cross_server {
        step(report, scene, "server B counted before B joins", |_scene| {
            before = server_b_answers()?;
            Ok(format!("{before} requests answered so far, all phone A's"))
        })?;
    }

    step(report, scene, "B opens the invite link", |scene| {
        let link = scene.link.clone().context("no link")?;
        tap_link(&scene.b, &link)?;
        Ok("opened as a tap would: a VIEW intent for the https link, no package named".to_owned())
    })?;

    // 2. Flow A.
    step(report, scene, "B asks to join (JoinA1)", |scene| {
        let b = &scene.b;
        let ask = Find::tag("join_ask").or(Text::Is("Ask to join".into()));
        let old_flow = Find::tag("accept_join").or(Text::Is("Codes match · Join".into()));
        let deadline = Deadline::after(NETWORK);
        loop {
            let screen = b.screen()?;
            if screen.find(&ask).is_some() {
                break;
            }
            if screen.find(&old_flow).is_some() {
                bail!(
                    "flow A is not built: B shows the old Accept invite screen (\"Codes match · Join\") instead of JoinA1 (\"You've been invited to a Party\", \"Ask to join\"); the screen shows: {}",
                    screen.summary()
                );
            }
            if deadline.passed() {
                bail!(
                    "no JoinA1 screen (\"Ask to join\") within {} s; the screen shows: {}",
                    NETWORK.as_secs(),
                    screen.summary()
                );
            }
            pause(Duration::from_secs(1));
        }
        b.tap_when(
            &Find::tag("join_name")
                .or(Text::Is("Your name in this Party".into()))
                .field(),
            UI,
        )?;
        b.type_text(JOINER)?;
        b.tap_when(&ask.enabled(), UI)?;
        Ok(format!("asked as {JOINER:?}"))
    })?;

    step(report, scene, "B reads its code (JoinA2)", |scene| {
        let node = scene
            .b
            .wait_for(&Find::tag("join_code").or(Text::SafetyCode), NETWORK)?;
        let code = safety_code(node.label())
            .with_context(|| format!("{:?} is not a 12-digit code", node.label()))?;
        scene.code_b = Some(code.clone());
        Ok(format!("B's code {code}"))
    })?;

    if apks.cross_server {
        server_b_registered(report, scene, before)?;
    }

    step(
        report,
        scene,
        "A sees the request (InviteApprove)",
        |scene| {
            let a = &scene.a;
            // Invite people lists the opened invite (it checks for requests while it shows);
            // Verify opens InviteApprove, "Ben asked to join".
            a.tap_when(
                &Find::tag("invite_verify").or(Text::Is("Verify".into())),
                NETWORK,
            )?;
            a.wait_for(&Find::text(Text::Is(format!("{JOINER} asked to join"))), UI)?;
            let node = a.wait_for(&Find::tag("approve_code").or(Text::SafetyCode), UI)?;
            let code = safety_code(node.label())
                .with_context(|| format!("{:?} is not a 12-digit code", node.label()))?;
            let expected = scene.code_b.clone().context("no code from B")?;
            if code != expected {
                bail!("codes differ: A shows {code}, B shows {expected}");
            }
            Ok(format!("codes match ({code})"))
        },
    )?;

    step(report, scene, "A is notified of the request", |scene| {
        let text = notification_shown(&scene.a, scene.package, "opened your invite", NETWORK)?;
        Ok(format!("A's notification: {text}"))
    })?;

    step(report, scene, "A lets B in", |scene| {
        scene.a.tap_when(
            &Find::tag("approve_let_in").or(Text::StartsWith("Codes match · Let".into())),
            UI,
        )?;
        Ok("tapped Codes match · Let in".to_owned())
    })?;

    step(report, scene, "B is in the Party (JoinA3)", |scene| {
        let b = &scene.b;
        b.tap_when(
            &Find::tag("joined_open").or(Text::Is(format!("Open {PARTY}"))),
            NETWORK,
        )?;
        b.wait_for(&Find::text(Text::Is(PARTY.into())), UI)?;
        Ok(format!("B opened {PARTY:?}"))
    })?;

    step(
        report,
        scene,
        "B's browser hands an invite to Pingers (landing page)",
        |scene| {
            let link = scene.link.clone().context("no link")?;
            through_landing_page(&scene.b, &link)
        },
    )?;

    step(report, scene, "both see each other in the Party", |scene| {
        open_party(&scene.a, scene.package)?;
        scene
            .a
            .wait_for(&Find::text(Text::Is(JOINER.into())), NETWORK)
            .context("A's Party does not list B")?;
        open_party(&scene.b, scene.package)?;
        Ok(format!("A's Party lists {JOINER}"))
    })?;

    if apks.cross_server {
        step(report, scene, "B's Party settings show server B", |scene| {
            let b = &scene.b;
            b.tap_when(&Find::tag("party_settings_open"), UI)?;
            let keys = Find::text(Text::Is("Server keys from your invite link".into()));
            let operator = Find::text(Text::Is(stack::SECOND_OPERATOR.into()));
            let screen = b.screen()?;
            if screen.find(&keys).is_none() || screen.find(&operator).is_none() {
                // The Operators card may sit below the fold.
                b.shell("input swipe 540 1800 540 900")?;
            }
            b.wait_for(&keys, UI)?;
            b.wait_for(&operator, UI)?;
            open_party(b, scene.package)?;
            Ok(format!(
                "run by {:?}, keys from the invite link",
                stack::SECOND_OPERATOR
            ))
        })?;
    }

    // 3. A Spot.
    step(report, scene, "A creates a Spot", |scene| {
        let a = &scene.a;
        a.geo_fix(SPOT_CENTRE)?;
        a.tap_when(
            &Find::tag("party_add_spot").or(Text::Is("Add spot".into())),
            UI,
        )?;
        a.tap_when(&Find::text(Text::Is("Spot name".into())).field(), UI)?;
        a.type_text(SPOT)?;
        a.tap_when(&Find::tag("spot_save").enabled(), UI)
            .context("Save spot never became enabled")?;
        a.wait_for(&Find::text(Text::Is(SPOT.into())), NETWORK)?;
        a.geo_fix(AWAY)?;
        Ok(format!("{SPOT:?} saved at the editor's centre"))
    })?;

    step(report, scene, "B sees the Spot", |scene| {
        scene
            .b
            .wait_for(&Find::text(Text::Is(SPOT.into())), NETWORK)?;
        Ok(format!("{SPOT:?} listed on B"))
    })?;

    // 4. Walk in.
    step(report, scene, "both enter; a Meetup forms", |scene| {
        let signal = Find::tag("alert_open")
            .or(Text::Is("Open meetup".into()))
            .or(Text::Contains(" here".into()));
        let deadline = Deadline::after(PRESENCE);
        let mut round = 0_u32;
        loop {
            // A fresh fix each round, a metre apart, so each counts as a new fix.
            let nudge = if round.is_multiple_of(2) {
                0.0
            } else {
                0.000_01
            };
            scene.a.geo_fix(Position {
                latitude: A_INSIDE.latitude + nudge,
                longitude: A_INSIDE.longitude,
            })?;
            scene.b.geo_fix(Position {
                latitude: B_INSIDE.latitude + nudge,
                longitude: B_INSIDE.longitude,
            })?;
            let on_a = scene.a.screen()?.find(&signal).is_some();
            let on_b = scene.b.screen()?.find(&signal).is_some();
            if on_a && on_b {
                return Ok(format!(
                    "both phones show the Meetup after {} rounds",
                    round.saturating_add(1)
                ));
            }
            if deadline.passed() {
                bail!(
                    "no Meetup within {} s of both phones being inside (A shows it: {on_a}, B shows it: {on_b}); A's screen: {}; B's screen: {}",
                    PRESENCE.as_secs(),
                    scene.a.screen()?.summary(),
                    scene.b.screen()?.summary()
                );
            }
            round = round.saturating_add(1);
            pause(Duration::from_secs(5));
        }
    })?;

    step(report, scene, "both are notified of the Meetup", |scene| {
        let mut seen = Vec::new();
        for device in [&scene.a, &scene.b] {
            let text = notification_shown(device, scene.package, "too. Say hi?", NETWORK)?;
            seen.push(format!("{}: {text}", device.label));
        }
        Ok(seen.join("; "))
    })?;

    step(report, scene, "both open the Meetup", |scene| {
        for device in [&scene.a, &scene.b] {
            let open = Find::tag("alert_open").or(Text::Is("Open meetup".into()));
            if device.screen()?.find(&open).is_some() {
                device.tap_when(&open, UI)?;
            }
            device.wait_for(&Find::text(Text::Contains("· 2 here".into())), NETWORK)?;
        }
        Ok("Meetup shows 2 here on both".to_owned())
    })?;

    // 5. Chat, with B out of the app so the message reaches it as a push and a notification.
    step(report, scene, "B leaves the app", |scene| {
        scene.b.shell("input keyevent KEYCODE_HOME")?;
        Ok("B is on the launcher".to_owned())
    })?;

    step(report, scene, "A sends a chat message", |scene| {
        let a = &scene.a;
        a.tap_when(&Find::tag("meetup_chat"), UI)?;
        a.tap_when(&Find::text(Text::Is("Message".into())).field(), UI)?;
        a.type_text(MESSAGE)?;
        a.tap_when(&Find::tag("chat_send"), UI)?;
        a.wait_for(&Find::text(Text::Is(MESSAGE.into())), UI)?;
        Ok(format!("sent {MESSAGE:?}"))
    })?;

    step(report, scene, "B is notified of it", |scene| {
        let text = notification_shown(&scene.b, scene.package, MESSAGE, NETWORK)?;
        Ok(format!("B's notification: {text}"))
    })?;

    step(report, scene, "B receives it", |scene| {
        let b = &scene.b;
        b.shell(&format!("am start -W -n {}/{ACTIVITY}", scene.package))?;
        b.tap_when(&Find::tag("meetup_chat"), UI)?;
        b.wait_for(&Find::text(Text::Is(MESSAGE.into())), NETWORK)?;
        b.back()?;
        scene.a.back()?;
        Ok(format!("{MESSAGE:?} shown on B"))
    })?;

    // 6. Location consent and the compass.
    step(
        report,
        scene,
        "B asks for A's location; A allows",
        |scene| {
            scene.b.tap_when(&Find::tag("meetup_ask_location"), UI)?;
            scene.a.tap_when(
                &Find::tag("location_allow").or(Text::Is("Allow".into())),
                NETWORK,
            )?;
            scene.b.wait_for(
                &Find::text(Text::Contains("sharing with you".into()))
                    .or(Text::Is("Sharing with you".into())),
                NETWORK,
            )?;
            Ok("B sees A sharing".to_owned())
        },
    )?;

    step(report, scene, "B's compass points at A", |scene| {
        let b = &scene.b;
        b.tap_when(&Find::tag("meetup_compass"), UI)?;
        let dial = b.wait_for(
            &Find::text(Text::StartsWith("Arrow pointing".into())),
            NETWORK,
        )?;
        b.back()?;
        Ok(format!("compass: {:?}", dial.description))
    })?;

    // 7. Walk out.
    step(report, scene, "both leave; the Meetup ends", |scene| {
        let ended = Find::tag("ended_done").or(Text::Is("Meetup ended".into()));
        let deadline = Deadline::after(PRESENCE);
        loop {
            scene.a.geo_fix(AWAY)?;
            scene.b.geo_fix(AWAY)?;
            let on_a = scene.a.screen()?.find(&ended).is_some();
            let on_b = scene.b.screen()?.find(&ended).is_some();
            if on_a && on_b {
                return Ok("Meetup ended on both".to_owned());
            }
            if deadline.passed() {
                bail!(
                    "the Meetup did not end within {} s of both leaving (A: {on_a}, B: {on_b})",
                    PRESENCE.as_secs()
                );
            }
            pause(Duration::from_secs(5));
        }
    })?;
    Ok(())
}

/// The gms flavour's FCM check (see [`crate::fcm_push`]): a content-free push naming no handle
/// goes from the local push relay through FCM to a phone, and that phone's core takes it.
/// Without FCM in the stack (no `--fcm-service-account`) the step says so and passes, since
/// FCM is opt-in.
///
/// Both emulators boot read-only from the same AVD, so they share one Play services identity.
/// FCM then keeps one registration for the pair and delivers to whichever phone holds the
/// identity's connection: the other phone's registration fails or its token answers
/// `UNREGISTERED` (the relay's 410), and a push for A's token can arrive at B. So A's token is
/// tried first and B's after a 410 or no token, and the push counts when either phone's core
/// takes it.
fn fcm_wakes_the_core(scene: &mut Scene) -> Result<String> {
    let config = scene.gms_push_relay.clone().context("not a gms build")?;
    if !crate::fcm_push::configured(&config)? {
        return Ok(
            "not run: the stack's push relay has no FCM (local-stack --fcm-service-account)"
                .to_owned(),
        );
    }
    let phones = [&scene.a, &scene.b];
    let mut skipped = Vec::new();
    for device in phones {
        let label = device.label;
        let Ok(token) = push_preference(device, scene.package, "fcm.token") else {
            skipped.push(format!("{label} has no FCM token"));
            continue;
        };
        for phone in phones {
            phone.shell("logcat -c")?;
        }
        match crate::fcm_push::send(&config, &token, [0; 8])? {
            crate::fcm_push::Sent::Gone => skipped.push(format!("{label}'s token unregistered")),
            crate::fcm_push::Sent::Accepted(status) => {
                let arrival = fcm_reached_a_core(&phones, &status)?;
                let note = if skipped.is_empty() {
                    String::new()
                } else {
                    format!(
                        " ({}: the emulators share one Play services identity)",
                        skipped.join(", ")
                    )
                };
                return Ok(format!(
                    "relay answered {status} for {label}'s token; {}'s logcat: {}{note}",
                    arrival.label, arrival.line
                ));
            }
        }
    }
    bail!(
        "no phone took an FCM push ({}; no Play services, or no google-services.json?)",
        skipped.join(", ")
    )
}

/// Where an FCM push arrived: the phone and its core's log line.
struct Arrival {
    label: &'static str,
    line: String,
}

/// Waits for either phone's core to take an FCM push the relay accepted with `status`.
fn fcm_reached_a_core(phones: &[&Device], status: &str) -> Result<Arrival> {
    let deadline = Deadline::after(NETWORK);
    loop {
        for device in phones {
            let log = device.shell("logcat -d -s PingersCore")?;
            if let Some(line) = log.lines().find(|line| line.contains("onPush FCM")) {
                if !line.contains("onPush FCM: ok") {
                    bail!(
                        "{}'s core did not take the FCM push: {}",
                        device.label,
                        line.trim()
                    );
                }
                return Ok(Arrival {
                    label: device.label,
                    line: line.trim().to_owned(),
                });
            }
        }
        if deadline.passed() {
            bail!(
                "the relay took the push ({status}) but it never reached a core (no onPush FCM in either phone's logcat)"
            );
        }
        pause(Duration::from_secs(1));
    }
}

/// The preference `name` of Pingers' push preferences on `device` (a debug build's, read
/// with run-as), once it is set.
fn push_preference(device: &Device, package: &str, name: &str) -> Result<String> {
    let deadline = Deadline::after(NETWORK);
    let key = format!("name=\"{name}\"");
    loop {
        let prefs = device
            .shell(&format!(
                "run-as {package} cat shared_prefs/pingers.push.xml"
            ))
            .unwrap_or_default();
        if let Some(value) = prefs
            .lines()
            .find(|line| line.contains(&key))
            .and_then(|line| line.split('>').nth(1))
            .and_then(|rest| rest.split('<').next())
        {
            return Ok(value.replace("&amp;", "&"));
        }
        if deadline.passed() {
            bail!("no {name} in Pingers' push preferences");
        }
        pause(Duration::from_secs(1));
    }
}

/// The UnifiedPush endpoint the app on `device` registered (a debug build's preferences,
/// read with run-as).
fn unifiedpush_endpoint(device: &Device, package: &str) -> Result<String> {
    push_preference(device, package, "unifiedpush.endpoint").context(
        "A never registered with a UnifiedPush distributor (no endpoint in its preferences)",
    )
}

/// Waits until one of Pingers' notifications on `device` contains `text` (title or body, as
/// `dumpsys notification` shows them) and returns that line.
fn notification_shown(
    device: &Device,
    package: &str,
    text: &str,
    within: Duration,
) -> Result<String> {
    let deadline = Deadline::after(within);
    loop {
        let dump = device.shell("dumpsys notification --noredact")?;
        if let Some(line) = notification_line(&dump, package, text) {
            return Ok(line);
        }
        if deadline.passed() {
            bail!(
                "no notification containing {text:?} within {} s",
                within.as_secs()
            );
        }
        pause(Duration::from_secs(1));
    }
}

/// The first line of `dump` that belongs to a notification of `package` and contains `text`.
fn notification_line(dump: &str, package: &str, text: &str) -> Option<String> {
    let mut in_pingers = false;
    for line in dump.lines() {
        if line.contains("NotificationRecord(") {
            in_pingers = line.contains(&format!("pkg={package} "));
        }
        if in_pingers && line.contains(text) {
            return Some(line.trim().to_owned());
        }
    }
    None
}

/// Installs (or finds) ntfy and points its default server at the stack's, through `adb
/// reverse`. Without an ntfy app, says so: Pingers then has no UnifiedPush distributor.
/// Phone B, whose built-in server is A, joins phone A's Party on server B: it records B with the
/// invite's pins, registers there with the link's code, and asks to join. Server B answering
/// phone B's registration and requests (direct and through B's relay) is the evidence.
/// With `--cross-server`, after B asked to join: server B answered phone B, whose built-in
/// server is server A, so B recorded server B from the link and registered there.
fn server_b_registered(report: &mut Report, scene: &mut Scene, before: u64) -> Result<(), Stopped> {
    step(report, scene, "B registered on server B", |_scene| {
        let deadline = Deadline::after(NETWORK);
        loop {
            let now = server_b_answers()?;
            // Registration, bearer, token issuance and leaf (direct), then the reply mailbox
            // and the join request (through B's relay).
            if now >= before.saturating_add(4) {
                return Ok(format!(
                    "server B answered {} more requests",
                    now.saturating_sub(before)
                ));
            }
            if deadline.passed() {
                bail!(
                    "server B answered only {} more requests",
                    now.saturating_sub(before)
                );
            }
            pause(Duration::from_secs(2));
        }
    })
}

/// How many requests server B answered, by its own metrics (public and OHTTP channels).
fn server_b_answers() -> Result<u64> {
    let address = stack::second_server_api();
    let output = std::process::Command::new("curl")
        .args(["--silent", "--show-error", "--http2-prior-knowledge"])
        .arg(format!("http://{address}/metrics"))
        .output()
        .context("running curl for server B's metrics")?;
    if !output.status.success() {
        bail!("server B's metrics did not answer");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(answered(&text))
}

/// The answered requests in a metrics text: every `pingers_requests_total` line whose outcome
/// is `ok`.
fn answered(metrics: &str) -> u64 {
    metrics
        .lines()
        .filter(|line| {
            line.starts_with("pingers_requests_total") && line.contains("outcome=\"ok\"")
        })
        .filter_map(|line| line.rsplit(' ').next()?.parse::<u64>().ok())
        .sum()
}

fn setup_ntfy(device: &Device, apk: Option<&Path>) -> Result<String> {
    device.adb(&[
        "reverse",
        &format!("tcp:{NTFY_PORT}"),
        &format!("tcp:{NTFY_PORT}"),
    ])?;
    let installed = device
        .shell(&format!("pm list packages {NTFY_PACKAGE}"))?
        .lines()
        .any(|line| line.trim() == format!("package:{NTFY_PACKAGE}"));
    if !installed {
        let Some(apk) = apk else {
            return Ok("no ntfy app (--ntfy-apk); no UnifiedPush distributor".to_owned());
        };
        device.adb(&["install", "-r", "-g", &apk.display().to_string()])?;
    }
    let server = format!("http://127.0.0.1:{NTFY_PORT}");
    if let Err(error) = set_ntfy_server(device, &server) {
        // Never leave a distributor that would register with ntfy.sh.
        let _ignored = device.adb(&["uninstall", NTFY_PACKAGE]);
        bail!("could not set ntfy's default server, so ntfy was removed: {error:#}");
    }
    Ok(format!("ntfy default server {server}"))
}

/// ntfy's Settings, "Default server". ntfy is a View app, so its own words are matched.
fn set_ntfy_server(device: &Device, server: &str) -> Result<()> {
    device.shell(&format!(
        "monkey -p {NTFY_PACKAGE} -c android.intent.category.LAUNCHER 1"
    ))?;
    device.tap_when(&Find::text(Text::Is("More options".into())), UI)?;
    device.tap_when(&Find::text(Text::Is("Settings".into())), UI)?;
    device.tap_when(&Find::text(Text::Is("Default server".into())), UI)?;
    let field = Find::default()
        .or(Text::StartsWith("https://".into()))
        .field();
    let field = device
        .wait_for(&field, UI)
        .or_else(|_| device.wait_for(&Find::text(Text::Is(String::new())).field(), UI))?;
    device.tap(&field)?;
    // Clear what is there, then type ours.
    device.shell("input keyevent KEYCODE_MOVE_END")?;
    device.shell(&format!("input keyevent {}", "KEYCODE_DEL ".repeat(40)))?;
    device.type_text(server)?;
    device.tap_when(
        &Find::text(Text::Is("OK".into()))
            .or(Text::Is("Save".into()))
            .or(Text::Is("SAVE".into())),
        UI,
    )?;
    device.wait_for(&Find::text(Text::Is(server.to_owned())), UI)?;
    // Leave ntfy running in the background, not force-stopped: Android delivers no broadcast to
    // a stopped app, so Pingers' UnifiedPush registration would never get an endpoint.
    device.shell("input keyevent KEYCODE_HOME")?;
    Ok(())
}

/// A fresh install with every runtime permission granted (so onboarding has nothing to ask),
/// animations off, the phone away from the Spot, and Home on screen.
fn install_pingers(device: &Device, apk: &Path, package: &str) -> Result<()> {
    let _ignored = device.adb(&["uninstall", package]);
    device.adb(&["install", "-r", "-g", &apk.display().to_string()])?;
    for setting in [
        "window_animation_scale",
        "transition_animation_scale",
        "animator_duration_scale",
    ] {
        device.shell(&format!("settings put global {setting} 0"))?;
    }
    device.shell("settings put secure location_mode 3")?;
    device.shell(&format!("dumpsys deviceidle whitelist +{package}"))?;
    // The stack's server cannot pass App Links verification (no port 443, a dev CA), so the
    // user's approval stands in for it: a tapped https://10.0.2.2:9443/j link then opens
    // Pingers as a verified one would. tap_link also handles the chooser if this is refused.
    let _approved = device.shell(&format!(
        "pm set-app-links-user-selection --user 0 --package {package} true {STACK_HOST}"
    ));
    device.shell("logcat -c")?;
    device.geo_fix(AWAY)?;
    device.shell(&format!("am start -W -n {package}/{ACTIVITY}"))?;
    through_first_run(device).context("Home did not appear after launch")
}

/// First run (design canvas `SetupWelcome`, `SetupBackground`) comes before Home on a fresh
/// install. `adb install -g` has granted every permission it asks for, so each step's main
/// button only moves on; it is tapped until Home shows.
fn through_first_run(device: &Device) -> Result<()> {
    let home = Find::tag("home_new_party").or(Text::Is("New party".into()));
    let setup = Find::tag("setup_continue");
    let deadline = Deadline::after(UI);
    loop {
        let screen = device.screen()?;
        if screen.find(&home).is_some() {
            return Ok(());
        }
        if deadline.passed() {
            bail!(
                "{device}: no {home} or {setup} within {} s; the screen shows: {}",
                UI.as_secs(),
                screen.summary()
            );
        }
        if let Some(button) = screen.find(&setup) {
            device.tap(button)?;
        }
        pause(Duration::from_millis(500));
    }
}

/// Opens `link` as a tap in another app would: a browsable VIEW intent naming no package, so
/// Android picks the handler (Pingers, by the approved App Link, or from the chooser).
fn tap_link(device: &Device, link: &str) -> Result<()> {
    // Single quotes: the phone's shell must not treat '#' as a comment.
    device.shell(&format!(
        "am start -W -a android.intent.action.VIEW -c android.intent.category.BROWSABLE -d '{link}'"
    ))?;
    // If Android asks which app should open it, Pingers, once.
    let chooser = Find::text(Text::Is("Pingers".into()));
    let deadline = Deadline::after(Duration::from_secs(5));
    while !deadline.passed() {
        let screen = device.screen()?;
        if screen.find(&Find::tag("join_ask")).is_some() {
            return Ok(());
        }
        if let Some(pingers) = screen.find(&chooser) {
            device.tap(pingers)?;
            pause(Duration::from_secs(1));
            let once = Find::text(Text::Is("Just once".into())).or(Text::Is("Once".into()));
            if let Some(button) = device.screen()?.find(&once) {
                device.tap(button)?;
            }
            return Ok(());
        }
        pause(Duration::from_millis(500));
    }
    Ok(())
}

/// Opens `link` in the browser, where the server's landing page at /j hands it to Pingers:
/// gets past Chrome's first-run screens and the dev CA's warning, taps "Open in Pingers" and
/// waits for Open invite (`JoinA1`), which it leaves with "Not now".
fn through_landing_page(device: &Device, link: &str) -> Result<String> {
    device.shell(&format!(
        "am start -W -a android.intent.action.VIEW -d '{link}' {CHROME}"
    ))?;
    // Chrome's first run and the certificate interstitial (the stack's dev CA is not one the
    // browser trusts), then any prompt before it opens another app.
    let past = [
        "Use without an account",
        "Accept & continue",
        "No thanks",
        "Got it",
        "Advanced",
    ];
    let open = Find::text(Text::Is("Open in Pingers".into()));
    let joined = Find::tag("join_ask").or(Text::Is("Ask to join".into()));
    let deadline = Deadline::after(NETWORK);
    let mut tapped_open = false;
    loop {
        let screen = device.screen()?;
        if screen.find(&joined).is_some() {
            break;
        }
        if deadline.passed() {
            bail!(
                "the landing page never handed the invite to Pingers within {} s; the screen shows: {}",
                NETWORK.as_secs(),
                screen.summary()
            );
        }
        let proceed = Find::text(Text::StartsWith(format!("Proceed to {STACK_HOST}")));
        let next = past
            .iter()
            .find_map(|label| screen.find(&Find::text(Text::Is((*label).into()))))
            .or_else(|| screen.find(&proceed))
            .or_else(|| {
                if tapped_open {
                    // Chrome may ask before leaving for an app.
                    screen
                        .find(&Find::text(Text::Is("Continue".into())))
                        .or_else(|| screen.find(&Find::text(Text::Is("Open".into()))))
                } else {
                    None
                }
            });
        if let Some(button) = next {
            device.tap(button)?;
        } else if let Some(button) = screen.find(&open) {
            device.tap(button)?;
            tapped_open = true;
        }
        pause(Duration::from_secs(1));
    }
    device.tap_when(&Find::text(Text::Is("Not now".into())), UI)?;
    Ok("Chrome showed the landing page; Open in Pingers opened Open invite".to_owned())
}

/// Back to Home, then into the (only) Party.
fn open_party(device: &Device, package: &str) -> Result<()> {
    let home = Find::tag("home_new_party");
    for _press in 0..5 {
        if device.screen()?.find(&home).is_some() {
            break;
        }
        device.back()?;
        pause(Duration::from_millis(500));
    }
    if device.screen()?.find(&home).is_none() {
        device.shell(&format!("am start -W -n {package}/{ACTIVITY}"))?;
    }
    device.tap_when(&Find::tag("party_row").or(Text::Is(PARTY.into())), NETWORK)?;
    device.wait_for(&Find::tag("party_add_spot"), UI)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(options: &[&str]) -> Vec<String> {
        options.iter().map(|option| (*option).to_owned()).collect()
    }

    #[test]
    fn answered_requests_add_up_the_ok_lines() {
        let metrics = "# TYPE pingers_requests_total counter\n\
            pingers_requests_total{channel=\"public\",outcome=\"ok\"} 5\n\
            pingers_requests_total{channel=\"public\",outcome=\"client_error\"} 9\n\
            pingers_requests_total{channel=\"ohttp\",outcome=\"ok\"} 3\n";
        assert_eq!(answered(metrics), 8);
    }

    #[test]
    fn options_mix_stack_and_scenario_flags() {
        let parsed = ScenarioOptions::parse(&strings(&[
            "--dir",
            "/s",
            "--avd",
            "Pixel",
            "--leases",
            "/l",
            "--apk",
            "/a.apk",
            "--flavour",
            "gms",
        ]));
        assert_eq!(
            parsed,
            Some(ScenarioOptions {
                stack: StackOptions {
                    dir: Some(PathBuf::from("/s")),
                    ..StackOptions::default()
                },
                avd: Some("Pixel".into()),
                leases: Some(PathBuf::from("/l")),
                apk: Some(PathBuf::from("/a.apk")),
                ntfy_apk: None,
                flavour: Flavour::Gms,
                cross_server: false,
            })
        );
        let crossing = ScenarioOptions::parse(&strings(&["--second-server", "--cross-server"]));
        assert!(
            crossing
                .as_ref()
                .is_some_and(|options| options.cross_server)
        );
        assert!(crossing.is_some_and(|options| options.stack.second_server));
        assert_eq!(ScenarioOptions::parse(&strings(&["--avd"])), None);
        assert_eq!(
            ScenarioOptions::parse(&strings(&["--flavour", "play"])),
            None
        );
        assert_eq!(
            ScenarioOptions::parse(&[]).map(|options| options.flavour.package()),
            Some("org.pingers.app.foss.debug")
        );
        assert_eq!(Flavour::Gms.package(), "org.pingers.app.debug");
    }

    #[test]
    fn the_walk_stays_inside_the_default_spot_and_ends_far_away() {
        // Equirectangular metres near the equator are close enough at these distances.
        let metres = |from: Position, to: Position| {
            let north = (to.latitude - from.latitude) * 111_320.0;
            let east = (to.longitude - from.longitude) * 111_320.0;
            north.hypot(east)
        };
        for inside in [A_INSIDE, B_INSIDE] {
            assert!(metres(SPOT_CENTRE, inside) < 50.0);
        }
        assert!(metres(SPOT_CENTRE, AWAY) > 1_000.0);
    }

    #[test]
    fn only_pingers_notifications_are_read_from_the_dump() {
        let dump = "  NotificationRecord(0x1: pkg=io.heckel.ntfy user=0 id=1)\n    \
                    android.text=String (Hello from A)\n  \
                    NotificationRecord(0x2: pkg=org.pingers.app.foss.debug user=0 id=7)\n    \
                    android.title=String (Ana · Crag)\n    android.text=String (Hello from A)\n";
        assert_eq!(
            notification_line(dump, "org.pingers.app.foss.debug", "Hello from A").as_deref(),
            Some("android.text=String (Hello from A)")
        );
        assert_eq!(
            notification_line(dump, "org.pingers.app.foss.debug", "Ana · Crag").as_deref(),
            Some("android.title=String (Ana · Crag)")
        );
        assert_eq!(
            notification_line(dump, "org.pingers.app.foss.debug", "nothing"),
            None
        );
        let ntfy_only = "NotificationRecord(0x1: pkg=io.heckel.ntfy id=1)\n android.text=String (Hello from A)\n";
        assert_eq!(
            notification_line(ntfy_only, "org.pingers.app.foss.debug", "Hello from A"),
            None
        );
    }

    #[test]
    fn the_report_names_the_failed_step_and_the_furthest_passed() {
        let mut report = Report {
            out: std::env::temp_dir().join("pingers-e2e-report-test-does-not-exist"),
            steps: Vec::new(),
            notes: Vec::new(),
        };
        report.record("local stack up", StepOutcome::Passed("ok".into()));
        report.record(
            "B asks to join (JoinA1)",
            StepOutcome::Failed("no screen".into()),
        );
        let text = report.text();
        assert!(text.contains("failed at step 02 B asks to join (JoinA1)"));
        assert!(text.contains("Furthest step passed: 01 local stack up"));
    }
}
