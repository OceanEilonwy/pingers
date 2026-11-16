// SPDX-License-Identifier: AGPL-3.0-only
//! The device-wide screens of the design canvas's section 4, as UniFFI records with their
//! words from pingers-words: first run, the first-phone setup link's step, the Settings hub,
//! Quiet hours, Zone times, Wake-ups, what Home says about wake-ups, the notice at the top of
//! Home, and Privacy's words.
//!
//! The phone's own facts (which push app wakes Pingers, map downloads, battery and
//! notification settings, the last wake-up) come in from the app as records; the core's
//! facts come from the [`Snapshot`].

use pingers_screens as screens;
use pingers_time::UnixSeconds;
use pingers_words::Language;

use crate::CoreError;
use crate::callbacks::Notice;
use crate::ids::ZoneId;
use crate::snapshot::{LocalClock, Snapshot};
use crate::views::{TimeOfDay, Visibility, ZoneDays, ZoneTimes};

const LANGUAGE: Language = Language::English;

// --------------------------------------------------------------------------------- First run

/// A first-run step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum SetupStep {
    /// Welcome, and the location permission.
    Location,
    /// Location while the app is closed.
    Background,
    /// Notifications.
    Notifications,
}

impl From<SetupStep> for screens::setup::SetupStep {
    fn from(step: SetupStep) -> Self {
        match step {
            SetupStep::Location => Self::Location,
            SetupStep::Background => Self::Background,
            SetupStep::Notifications => Self::Notifications,
        }
    }
}

/// A first-run screen (canvas `SetupWelcome`, `SetupBackground`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SetupView {
    /// The step after this one, or `None` after the last.
    pub next: Option<SetupStep>,
    /// Which dot is current, from 0.
    pub dot: u32,
    /// How many dots.
    pub dots: u32,
    /// "Welcome to Pingers".
    pub heading: String,
    /// The line under it.
    pub body: String,
    /// The card's title, on the welcome only.
    pub card_title: Option<String>,
    /// The card's text.
    pub card_body: Option<String>,
    /// Which option to choose in Android's dialog.
    pub hint: Option<String>,
    /// The dialog's options as a picture, the one to choose first.
    pub choices: Vec<String>,
    /// "First of three permissions".
    pub progress: String,
    /// "Allow location", "Continue".
    pub primary: String,
    /// "Not now".
    pub not_now: String,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A first-run screen.
#[uniffi::export]
#[must_use]
pub fn setup_view(step: SetupStep) -> SetupView {
    let screen = screens::setup::setup(step.into());
    let text = pingers_words::setup::setup(LANGUAGE, &screen);
    SetupView {
        next: screen.step.next().map(|next| match next {
            screens::setup::SetupStep::Location => SetupStep::Location,
            screens::setup::SetupStep::Background => SetupStep::Background,
            screens::setup::SetupStep::Notifications => SetupStep::Notifications,
        }),
        dot: count(screen.position.index),
        dots: count(screen.position.count),
        heading: text.heading,
        body: text.body,
        card_title: text.card.as_ref().map(|card| card.title.clone()),
        card_body: text.card.map(|card| card.body),
        hint: text.hint,
        choices: text.choices,
        progress: text.progress,
        primary: text.primary,
        not_now: text.not_now,
    }
}

// ---------------------------------------------------------------- The first-phone setup link

/// The first-phone setup link's step: what [`Core::check_setup_link`](crate::Core) answers
/// (`Confirm`, or why the step stops before asking), what
/// [`Core::use_setup_link`](crate::Core) answers, and `Working`, which the app shows while that
/// call runs. Servers are named by host ("pingers.example.org").
#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum SetupLinkState {
    /// Asks before making the account.
    Confirm {
        /// The server the account would be made on.
        server: String,
    },
    /// The account is being made.
    Working {
        /// The server.
        server: String,
    },
    /// The account was made with the link's code.
    SetUp,
    /// The account was made before; nothing was offered.
    AlreadySetUp,
    /// The server refused this code, now or before (then it was not sent again).
    Refused,
    /// The link is for another server than the app's; nothing was sent.
    OtherServer {
        /// The app's server.
        app_server: String,
        /// The link's server.
        link_server: String,
    },
    /// The build pins keys and the link's served keys are not among them; nothing was sent.
    BuildKeysDiffer {
        /// The server.
        server: String,
    },
    /// The gateway key the relay served is not the link's; the code was not sent.
    ServerKeysDiffer {
        /// The server.
        server: String,
    },
    /// The relay or the server did not answer; the user may tap "Try again".
    Unreachable {
        /// The server.
        server: String,
    },
    /// The link does not decode.
    Broken,
}

impl From<screens::setup::SetupLink> for SetupLinkState {
    fn from(link: screens::setup::SetupLink) -> Self {
        use screens::setup::SetupLink;
        let name = |host: &pingers_client::snapshot::Host| host.as_str().to_owned();
        match link {
            SetupLink::Confirm { server } => Self::Confirm {
                server: name(&server),
            },
            SetupLink::Working { server } => Self::Working {
                server: name(&server),
            },
            SetupLink::SetUp => Self::SetUp,
            SetupLink::AlreadySetUp => Self::AlreadySetUp,
            SetupLink::Refused => Self::Refused,
            SetupLink::OtherServer { app, link } => Self::OtherServer {
                app_server: name(&app),
                link_server: name(&link),
            },
            SetupLink::BuildKeysDiffer { server } => Self::BuildKeysDiffer {
                server: name(&server),
            },
            SetupLink::ServerKeysDiffer { server } => Self::ServerKeysDiffer {
                server: name(&server),
            },
            SetupLink::Unreachable { server } => Self::Unreachable {
                server: name(&server),
            },
            SetupLink::Broken => Self::Broken,
        }
    }
}

impl From<SetupLinkState> for screens::setup::SetupLink {
    /// A host that is not one (the app passes back what the core gave it, so never) makes the
    /// step say the link does not work.
    fn from(state: SetupLinkState) -> Self {
        use pingers_client::snapshot::Host;
        let host = |text: &str| Host::new(text).ok();
        let server = |text: &str, make: fn(Host) -> Self| host(text).map_or(Self::Broken, make);
        match state {
            SetupLinkState::Confirm { server: text } => {
                server(&text, |server| Self::Confirm { server })
            }
            SetupLinkState::Working { server: text } => {
                server(&text, |server| Self::Working { server })
            }
            SetupLinkState::SetUp => Self::SetUp,
            SetupLinkState::AlreadySetUp => Self::AlreadySetUp,
            SetupLinkState::Refused => Self::Refused,
            SetupLinkState::OtherServer {
                app_server,
                link_server,
            } => match (host(&app_server), host(&link_server)) {
                (Some(app), Some(link)) => Self::OtherServer { app, link },
                _ => Self::Broken,
            },
            SetupLinkState::BuildKeysDiffer { server: text } => {
                server(&text, |server| Self::BuildKeysDiffer { server })
            }
            SetupLinkState::ServerKeysDiffer { server: text } => {
                server(&text, |server| Self::ServerKeysDiffer { server })
            }
            SetupLinkState::Unreachable { server: text } => {
                server(&text, |server| Self::Unreachable { server })
            }
            SetupLinkState::Broken => Self::Broken,
        }
    }
}

/// What the setup link step's main button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum SetupLinkAction {
    /// [`Core::use_setup_link`](crate::Core) ("Set up").
    SetUp,
    /// [`Core::use_setup_link`](crate::Core) again ("Try again"): only when tapped.
    TryAgain,
    /// Leaves the step, on to first run or Home ("Continue", "Close").
    Leave,
}

impl From<screens::setup::SetupLinkAction> for SetupLinkAction {
    fn from(action: screens::setup::SetupLinkAction) -> Self {
        match action {
            screens::setup::SetupLinkAction::SetUp => Self::SetUp,
            screens::setup::SetupLinkAction::TryAgain => Self::TryAgain,
            screens::setup::SetupLinkAction::Leave => Self::Leave,
        }
    }
}

/// The setup link step, in words, laid out like a first-run step.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SetupLinkView {
    /// "Set up Pingers on this phone".
    pub heading: String,
    /// What it does, or what happened and what to do next.
    pub body: String,
    /// The main button's words; `None` while the account is being made.
    pub primary: Option<String>,
    /// What the main button does.
    pub action: Option<SetupLinkAction>,
    /// "Not now", which leaves the step without using the link.
    pub not_now: Option<String>,
    /// Whether the account is being made: a progress indicator instead of buttons.
    pub busy: bool,
}

/// The setup link step for `state`.
#[uniffi::export]
#[must_use]
pub fn setup_link_view(state: SetupLinkState) -> SetupLinkView {
    let screen = screens::setup::SetupLink::from(state);
    let text = pingers_words::setup::setup_link(LANGUAGE, &screen);
    SetupLinkView {
        heading: text.heading,
        body: text.body,
        primary: text.primary,
        action: screen.primary().map(SetupLinkAction::from),
        not_now: text.not_now,
        busy: matches!(screen, screens::setup::SetupLink::Working { .. }),
    }
}

// ---------------------------------------------------------------------------- Settings hub

/// What can wake Pingers on this phone, as the app finds it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PushServiceInfo {
    /// A UnifiedPush app, by its label ("ntfy").
    PushApp {
        /// Its label.
        label: String,
    },
    /// Several push apps, none chosen.
    NotChosen,
    /// Google's push service (Firebase).
    Google,
    /// Nothing.
    Nothing,
}

impl From<&PushServiceInfo> for screens::device::PushService {
    fn from(info: &PushServiceInfo) -> Self {
        match info {
            PushServiceInfo::PushApp { label } => {
                screens::device::AppLabel::new(label).map_or(Self::NotChosen, Self::PushApp)
            }
            PushServiceInfo::NotChosen => Self::NotChosen,
            PushServiceInfo::Google => Self::Google,
            PushServiceInfo::Nothing => Self::Nothing,
        }
    }
}

/// What the Settings hub needs from the phone.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DevicePlatform {
    /// What can wake Pingers.
    pub push_service: PushServiceInfo,
    /// How many map areas are downloaded.
    pub map_areas: u32,
    /// Their total size in bytes.
    pub map_bytes: u64,
}

impl From<&DevicePlatform> for screens::device::DevicePlatform {
    fn from(platform: &DevicePlatform) -> Self {
        Self {
            push_service: (&platform.push_service).into(),
            maps: screens::device::MapsOnPhone {
                areas: usize::try_from(platform.map_areas).unwrap_or(usize::MAX),
                size: screens::device::Bytes::new(platform.map_bytes),
            },
        }
    }
}

/// The Settings hub (canvas `Settings`), the Home cog.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DeviceSettingsView {
    /// "Settings".
    pub title: String,
    /// "This phone, for every Party".
    pub subtitle: String,
    /// "BE", once a name is set.
    pub you_avatar: Option<String>,
    /// "Ben", or "Your name".
    pub you_title: String,
    /// "Your name in every Party".
    pub you_subtitle: String,
    /// "When not to share".
    pub when_heading: String,
    /// "Quiet hours".
    pub quiet_hours: String,
    /// "23:00–07:00 every day", or "Off".
    pub quiet_hours_summary: String,
    /// "Exclusion zones".
    pub zones: String,
    /// "Home · Work, weekdays 08:00–18:00".
    pub zones_summary: String,
    /// "This phone".
    pub phone_heading: String,
    /// "Wake-ups".
    pub wakeups: String,
    /// "On · through ntfy".
    pub wakeups_summary: String,
    /// "Downloaded maps".
    pub maps: String,
    /// "2 areas · 246 MB".
    pub maps_summary: String,
    /// "How Pingers keeps you private".
    pub security_link: String,
    /// Where server settings are.
    pub footnote: String,
}

// ----------------------------------------------------------------------------- Quiet hours

/// A stretch of the 24-hour bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct BarSpan {
    /// Where it starts, in minutes since midnight.
    pub start_minute: u32,
    /// How long, in minutes.
    pub length_minutes: u32,
}

/// The Quiet hours editor (canvas `QuietHours`, `QuietHoursError`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct QuietHoursEditorView {
    /// "Quiet hours".
    pub title: String,
    /// What quiet hours do.
    pub intro: String,
    /// "Quiet hours", the switch.
    pub switch_label: String,
    /// The switch.
    pub on: bool,
    /// "Starts".
    pub starts: String,
    /// Where the window starts.
    pub from: TimeOfDay,
    /// "23:00".
    pub from_text: String,
    /// "Starts at 23:00. Change".
    pub starts_action: String,
    /// "Ends".
    pub ends: String,
    /// Where it ends.
    pub to: TimeOfDay,
    /// "07:00".
    pub to_text: String,
    /// "next day".
    pub to_note: Option<String>,
    /// "Ends at 07:00 the next day. Change".
    pub ends_action: String,
    /// "8 hours, every day", or "No quiet time set".
    pub length: String,
    /// The bar's stretches.
    pub bar: Vec<BarSpan>,
    /// "Quiet from 23:00 to 07:00".
    pub bar_label: String,
    /// "The start and end times must differ.", when they are equal.
    pub error: Option<String>,
    /// The time zone and Meetups.
    pub footnote: String,
    /// "Save".
    pub save: String,
    /// Whether Save is enabled.
    pub can_save: bool,
}

/// The Quiet hours editor for the times picked so far.
///
/// # Errors
///
/// [`CoreError::Invalid`] for a time outside the day.
#[uniffi::export]
pub fn quiet_hours_editor(
    on: bool,
    from: TimeOfDay,
    to: TimeOfDay,
) -> Result<QuietHoursEditorView, CoreError> {
    let state = if on {
        screens::snapshot::Toggle::On
    } else {
        screens::snapshot::Toggle::Off
    };
    Ok(editor_view(&screens::settings::quiet_hours_editor(
        state,
        from.try_into()?,
        to.try_into()?,
    )))
}

fn editor_view(view: &screens::settings::QuietHoursEditor) -> QuietHoursEditorView {
    let on = view.state == screens::snapshot::Toggle::On;
    let text = pingers_words::device::quiet_hours(LANGUAGE, view);
    let bar = match &view.window {
        screens::settings::QuietWindow::Quiet { bar, .. } => bar
            .iter()
            .map(|span| BarSpan {
                start_minute: span.start.minutes_since_midnight(),
                length_minutes: span.length.get(),
            })
            .collect(),
        screens::settings::QuietWindow::Empty => Vec::new(),
    };
    QuietHoursEditorView {
        title: text.title,
        intro: text.intro,
        switch_label: text.switch_label,
        on,
        starts: text.starts,
        from: view.from.into(),
        from_text: text.from,
        starts_action: text.starts_action,
        ends: text.ends,
        to: view.to.into(),
        to_text: text.to,
        to_note: text.to_note,
        ends_action: text.ends_action,
        length: text.length,
        bar,
        bar_label: text.bar_label,
        error: text.error,
        footnote: text.footnote,
        save: text.save,
        can_save: view.can_save,
    }
}

// ------------------------------------------------------------------------------ Zone times

/// The Zone times editor (canvas `ZoneTimes`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ZoneTimesEditorView {
    /// The zone.
    pub zone: ZoneId,
    /// The times picked so far.
    pub times: ZoneTimes,
    /// "Work".
    pub title: String,
    /// "Exclusion zone · drawn area · all parties".
    pub subtitle: String,
    /// "When it applies".
    pub when_heading: String,
    /// "When this zone applies".
    pub when_label: String,
    /// "All the time".
    pub always: String,
    /// "Only at certain times".
    pub certain_times: String,
    /// Whether "All the time" is chosen.
    pub is_always: bool,
    /// "Days".
    pub days_label: String,
    /// "Every day", "Weekdays", "Weekends", in `ZoneDays` order.
    pub days: Vec<String>,
    /// "From".
    pub from: String,
    /// "08:00".
    pub from_time: Option<String>,
    /// "From 08:00. Change".
    pub from_action: Option<String>,
    /// "Until".
    pub until: String,
    /// "18:00".
    pub until_time: Option<String>,
    /// "Until 18:00. Change".
    pub until_action: Option<String>,
    /// "Monday to Friday, 08:00–18:00", bold.
    pub summary_lead: String,
    /// The rest of the summary.
    pub summary_rest: String,
    /// The core's refusal, when start and end are equal.
    pub error: Option<String>,
    /// Where the zone is kept.
    pub footnote: String,
    /// "Save".
    pub save: String,
    /// Whether Save is enabled.
    pub can_save: bool,
    /// "Delete zone".
    pub delete: String,
}

// ------------------------------------------------------------------------------- Wake-ups

/// Battery optimisation for Pingers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum BatteryUse {
    /// "Unrestricted".
    Unrestricted,
    /// "Optimised".
    Optimised,
}

impl From<BatteryUse> for screens::wakeups::BatteryUse {
    fn from(battery: BatteryUse) -> Self {
        match battery {
            BatteryUse::Unrestricted => Self::Unrestricted,
            BatteryUse::Optimised => Self::Optimised,
        }
    }
}

/// What the Wake-ups screens need from the phone.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WakePlatform {
    /// What can wake Pingers.
    pub push_service: PushServiceInfo,
    /// "Keep Pingers running".
    pub keep_running: bool,
    /// Battery optimisation.
    pub battery: BatteryUse,
    /// Whether notifications are allowed.
    pub notifications_allowed: bool,
    /// When the last wake-up push arrived, in Unix seconds.
    pub last_wake_at: Option<u64>,
    /// Whether it came at the priority it was sent with.
    pub last_wake_on_time: bool,
    /// How many urgent wake-ups Android delayed in the last seven days.
    pub delayed_this_week: u32,
    /// When the core last raised `Notice::PushDowngraded`, in Unix seconds.
    pub downgraded_at: Option<u64>,
}

impl WakePlatform {
    fn screen(&self, now: UnixSeconds) -> screens::wakeups::WakePlatform {
        screens::wakeups::WakePlatform {
            push_service: (&self.push_service).into(),
            keep_running: if self.keep_running {
                screens::snapshot::Toggle::On
            } else {
                screens::snapshot::Toggle::Off
            },
            battery: self.battery.into(),
            notifications: if self.notifications_allowed {
                screens::wakeups::Notifications::Allowed
            } else {
                screens::wakeups::Notifications::Blocked
            },
            last_wake: self.last_wake_at.map(|at| screens::wakeups::LastWake {
                at: UnixSeconds::new(at),
                timing: if self.last_wake_on_time {
                    screens::wakeups::WakeTiming::OnTime
                } else {
                    screens::wakeups::WakeTiming::Late
                },
            }),
            delayed_this_week: usize::try_from(self.delayed_this_week).unwrap_or(usize::MAX),
            downgrade: screens::wakeups::Downgrade::since(
                self.downgraded_at.map(UnixSeconds::new),
                now,
            ),
        }
    }
}

/// Which Wake-ups screen to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum WakeupsKind {
    /// "No wake-up pushes" chosen (canvas `WakeupsOn`, switch on).
    Off,
    /// Working (canvas `WakeupsOn`).
    Working,
    /// Nothing can wake Pingers (canvas `Wakeups`).
    NothingCanWake,
    /// Android holds back wake-ups (canvas `WakeupsDelayed`).
    Delayed,
}

/// A row with a title and a line under it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TitledRow {
    /// The title.
    pub title: String,
    /// The line under it.
    pub detail: String,
}

/// A Wake-ups screen. Optional parts are present on the screens that have them.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per switch on the screen"
)]
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WakeupsView {
    /// Which screen.
    pub kind: WakeupsKind,
    /// "Wake-ups".
    pub title: String,
    /// The intro (off or working).
    pub intro: Option<String>,
    /// "Working", "Off" (off or working).
    pub status_title: Option<String>,
    /// "Last wake-up arrived on time, yesterday 18:42".
    pub status_detail: Option<String>,
    /// "Nothing can wake Pingers" (a fault).
    pub fault_title: Option<String>,
    /// What it costs.
    pub fault_body: Option<String>,
    /// "How to fix it".
    pub fix_heading: Option<String>,
    /// "Install a push app, such as ".
    pub install_before: Option<String>,
    /// "ntfy", bold.
    pub install_app: Option<String>,
    /// ". It learns that a push arrived, never what it says."
    pub install_after: Option<String>,
    /// "Get ntfy".
    pub install_action: Option<String>,
    /// Step 2.
    pub come_back: Option<String>,
    /// "Try these".
    pub try_heading: Option<String>,
    /// "Battery use: Unrestricted".
    pub battery: Option<TitledRow>,
    /// "Change", while optimised.
    pub battery_action: Option<String>,
    /// "Notifications · Allowed".
    pub notifications: Option<TitledRow>,
    /// Whether notifications are allowed.
    pub notifications_allowed: bool,
    /// "Delivery" or "On this phone".
    pub settings_heading: String,
    /// "No wake-up pushes" and its line.
    pub no_wake_pushes: TitledRow,
    /// Whether "No wake-up pushes" is on.
    pub no_wake_pushes_on: bool,
    /// "Push service" and what it is.
    pub push_service: TitledRow,
    /// Whether tapping the push service row offers a choice.
    pub push_service_choosable: bool,
    /// "Keep Pingers running" and its line.
    pub keep_running: TitledRow,
    /// Whether it is on.
    pub keep_running_on: bool,
    /// Who learns what (off or working).
    pub footnote: Option<String>,
}

fn row(text: pingers_words::wakeups::RowText) -> TitledRow {
    TitledRow {
        title: text.title,
        detail: text.detail,
    }
}

/// What Home says about wake-ups (canvas `WakeupsOffHome`, `WakeupsHome`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HomeWakeupsView {
    /// Nothing.
    Nothing,
    /// The quiet grey line: off by choice.
    OffByChoice {
        /// "Wake-ups off · Meetups can take up to an hour to start".
        text: String,
    },
    /// The orange banner, a real fault.
    Fault {
        /// Which Wake-ups screen Fix opens.
        kind: WakeupsKind,
        /// "Friends nearby may be noticed late".
        title: String,
        /// "Nothing can wake Pingers on this phone yet."
        detail: String,
        /// "Fix".
        fix: String,
    },
}

// --------------------------------------------------------------------------------- Notices

/// A notice the core raised, and when, as the app keeps it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RaisedNotice {
    /// The notice.
    pub notice: Notice,
    /// When it was last raised, in Unix seconds.
    pub at: u64,
}

/// How serious a notice is, and so its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum NoticeSeverity {
    /// Red: something stopped.
    Stopped,
    /// Orange: something may not work.
    Warning,
    /// White: worth knowing.
    Info,
    /// Grey: things are slow.
    Slow,
}

/// Where a notice's link goes.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NoticeLink {
    /// Android's date and time settings.
    DateTime,
    /// This Party's settings.
    Party {
        /// The Party.
        party: crate::ids::PartyId,
    },
    /// Privacy settings.
    Privacy,
}

/// The notice at the top of Home (canvas `Notices`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NoticeView {
    /// Its colour.
    pub severity: NoticeSeverity,
    /// "Your phone's clock is wrong".
    pub title: String,
    /// What it means.
    pub body: String,
    /// "Open date and time settings".
    pub action: Option<String>,
    /// Where the link goes.
    pub link: Option<NoticeLink>,
}

fn home_notice(notice: &Notice) -> Option<screens::notices::HomeNotice> {
    use screens::notices::HomeNotice;
    Some(match notice {
        Notice::ClockSkew => HomeNotice::ClockWrong,
        Notice::RelayKeyMismatch => HomeNotice::RelayKeyChanged,
        Notice::BoardEquivocation => HomeNotice::ServerMayHide,
        Notice::DndUnavailable => HomeNotice::DndUnreadable,
        Notice::BoardTooLarge => HomeNotice::ServerBusy,
        Notice::UnansweredKnock { party, spot } => HomeNotice::UnansweredKnock {
            party: party.try_into().ok()?,
            spot: spot.try_into().ok()?,
        },
        Notice::PushDowngraded | Notice::PrivacyPassOnly | Notice::SameOperator | Notice::Other => {
            return None;
        }
    })
}

// ------------------------------------------------------------------------------ Privacy

/// A visibility choice on Privacy, in words.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct VisibilityChoice {
    /// The choice.
    pub value: Visibility,
    /// "Ask me each time".
    pub label: String,
    /// "Others see you're here, not where".
    pub help: Option<String>,
}

/// Privacy's words (canvas `PrivacySettings`): sharing and Focus, who sees the exact spot,
/// and Delivery as a link to Wake-ups. Quiet hours and exclusion zones are in device
/// Settings only.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PrivacyText {
    /// "Privacy".
    pub title: String,
    /// "Paused · Sleep Focus is on", while paused.
    pub paused: Option<String>,
    /// "Nobody can tell you've paused. Resumes when Focus ends."
    pub paused_detail: Option<String>,
    /// "When to share".
    pub when_heading: String,
    /// "Location sharing".
    pub sharing: String,
    /// "Pause during Focus".
    pub pause_in_focus: String,
    /// "Do Not Disturb, Sleep, any Focus".
    pub pause_in_focus_help: String,
    /// "Who sees my exact spot in meetups".
    pub who_heading: String,
    /// The three choices.
    pub visibility: Vec<VisibilityChoice>,
    /// "Delivery".
    pub delivery_heading: String,
    /// "Wake-ups".
    pub wakeups: String,
    /// "On · through ntfy".
    pub wakeups_summary: String,
    /// Why wake-ups reach this phone late or not at all (the core's downgrade), under the
    /// Wake-ups link; `None` when they arrive as they should or are off by choice.
    pub push_delivery: Option<String>,
    /// "How Pingers keeps you private".
    pub security_link: String,
}

#[uniffi::export]
impl Snapshot {
    /// The Settings hub.
    #[must_use]
    pub fn device_settings(&self, platform: &DevicePlatform) -> DeviceSettingsView {
        let view = screens::device::device_settings(self.client(), &platform.into());
        let text = pingers_words::device::device_settings(LANGUAGE, &view);
        DeviceSettingsView {
            title: text.title,
            subtitle: text.subtitle,
            you_avatar: text.you_avatar,
            you_title: text.you_title,
            you_subtitle: text.you_subtitle,
            when_heading: text.when_heading,
            quiet_hours: text.quiet_hours,
            quiet_hours_summary: text.quiet_hours_summary,
            zones: text.zones,
            zones_summary: text.zones_summary,
            phone_heading: text.phone_heading,
            wakeups: text.wakeups,
            wakeups_summary: text.wakeups_summary,
            maps: text.maps,
            maps_summary: text.maps_summary,
            security_link: text.security_link,
            footnote: text.footnote,
        }
    }

    /// The Quiet hours editor, starting from the stored quiet hours.
    #[must_use]
    pub fn quiet_hours_start(&self) -> QuietHoursEditorView {
        let quiet = screens::settings::privacy(self.client()).quiet_hours;
        editor_view(&screens::settings::quiet_hours_editor(
            quiet.state,
            quiet.from,
            quiet.to,
        ))
    }

    /// The Zone times editor for `zone`, with `times` picked so far, or its stored times.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] for a zone that is gone; [`CoreError::Invalid`] for a time
    /// outside the day.
    pub fn zone_times_editor(
        &self,
        zone: ZoneId,
        times: Option<ZoneTimes>,
    ) -> Result<ZoneTimesEditorView, CoreError> {
        let draft = times.map(TryInto::try_into).transpose()?;
        let view = screens::zones::zone_times_editor(self.client(), zone.into(), draft)?;
        let text = pingers_words::device::zone_times(LANGUAGE, &view);
        let (times, is_always) = match view.draft {
            screens::zones::ZoneTimesDraft::Always => (ZoneTimes::Always, true),
            screens::zones::ZoneTimesDraft::Window { days, from, to } => (
                ZoneTimes::Window {
                    days: ZoneDays::from(days),
                    from: from.into(),
                    to: to.into(),
                },
                false,
            ),
        };
        Ok(ZoneTimesEditorView {
            zone: view.zone.id.into(),
            times,
            title: text.title,
            subtitle: text.subtitle,
            when_heading: text.when_heading,
            when_label: text.when_label,
            always: text.always,
            certain_times: text.certain_times,
            is_always,
            days_label: text.days_label,
            days: text.days.into(),
            from: text.from,
            from_time: text.from_time,
            from_action: text.from_action,
            until: text.until,
            until_time: text.until_time,
            until_action: text.until_action,
            summary_lead: text.summary_lead,
            summary_rest: text.summary_rest,
            can_save: text.error.is_none(),
            error: text.error,
            footnote: text.footnote,
            save: text.save,
            delete: text.delete,
        })
    }

    /// The Wake-ups screen for the current state.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a UTC offset beyond ±18 hours.
    pub fn wakeups(
        &self,
        platform: &WakePlatform,
        clock: LocalClock,
    ) -> Result<WakeupsView, CoreError> {
        let clock: screens::clock::LocalClock = clock.try_into()?;
        let view = screens::wakeups::wakeups(self.client(), &platform.screen(clock.now));
        let text = pingers_words::wakeups::wakeups(LANGUAGE, &view, clock);
        let kind = match view.status {
            screens::wakeups::WakeupsStatus::Off => WakeupsKind::Off,
            screens::wakeups::WakeupsStatus::Working(_) => WakeupsKind::Working,
            screens::wakeups::WakeupsStatus::NothingCanWake => WakeupsKind::NothingCanWake,
            screens::wakeups::WakeupsStatus::Delayed(_) => WakeupsKind::Delayed,
        };
        let install = text.install;
        Ok(WakeupsView {
            kind,
            title: text.title,
            intro: text.intro,
            status_title: text.status.as_ref().map(|status| status.title.clone()),
            status_detail: text.status.map(|status| status.detail),
            fault_title: text.fault.as_ref().map(|fault| fault.title.clone()),
            fault_body: text.fault.map(|fault| fault.body),
            fix_heading: text.fix_heading,
            install_before: install.as_ref().map(|step| step.before.clone()),
            install_app: install.as_ref().map(|step| step.app.clone()),
            install_after: install.as_ref().map(|step| step.after.clone()),
            install_action: install.map(|step| step.action),
            come_back: text.come_back,
            try_heading: text.try_heading,
            battery_action: text
                .battery
                .as_ref()
                .and_then(|battery| battery.action.clone()),
            battery: text.battery.map(|battery| TitledRow {
                title: battery.title,
                detail: battery.detail,
            }),
            notifications: text.notifications.map(row),
            notifications_allowed: platform.notifications_allowed,
            settings_heading: text.settings_heading,
            no_wake_pushes: row(text.no_wake_pushes),
            no_wake_pushes_on: view.pushes == screens::snapshot::WakePushes::NoWakePushes,
            push_service: row(text.push_service),
            push_service_choosable: matches!(
                view.push_service,
                screens::device::PushService::PushApp(_) | screens::device::PushService::NotChosen
            ) && view.pushes == screens::snapshot::WakePushes::On,
            keep_running: row(text.keep_running),
            keep_running_on: platform.keep_running,
            footnote: text.footnote,
        })
    }

    /// What Home says about wake-ups.
    #[must_use]
    pub fn home_wakeups(&self, platform: &WakePlatform, now: u64) -> HomeWakeupsView {
        let screen = platform.screen(UnixSeconds::new(now));
        let view = screens::wakeups::home_wakeups(self.client(), &screen);
        match pingers_words::wakeups::home_wakeups(LANGUAGE, view) {
            pingers_words::wakeups::HomeWakeupsText::Nothing => HomeWakeupsView::Nothing,
            pingers_words::wakeups::HomeWakeupsText::OffByChoice(text) => {
                HomeWakeupsView::OffByChoice { text }
            }
            pingers_words::wakeups::HomeWakeupsText::Fault { title, detail, fix } => {
                HomeWakeupsView::Fault {
                    kind: match view {
                        screens::wakeups::HomeWakeups::Fault(
                            screens::wakeups::WakeFault::Delayed,
                        ) => WakeupsKind::Delayed,
                        screens::wakeups::HomeWakeups::Fault(
                            screens::wakeups::WakeFault::NothingCanWake,
                        )
                        | screens::wakeups::HomeWakeups::Nothing
                        | screens::wakeups::HomeWakeups::OffByChoice => WakeupsKind::NothingCanWake,
                    },
                    title,
                    detail,
                    fix,
                }
            }
        }
    }

    /// The one notice Home shows now, from those the app was told about.
    #[must_use]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "UniFFI hands lists over by value"
    )]
    pub fn top_notice(&self, raised: Vec<RaisedNotice>, now: u64) -> Option<NoticeView> {
        let raised: Vec<_> = raised
            .iter()
            .filter_map(|raised| {
                Some(screens::notices::RaisedNotice {
                    notice: home_notice(&raised.notice)?,
                    at: UnixSeconds::new(raised.at),
                })
            })
            .collect();
        let view = screens::notices::top_notice(self.client(), &raised, UnixSeconds::new(now))?;
        let text = pingers_words::notices::notice(LANGUAGE, &view);
        Some(NoticeView {
            severity: match view.severity {
                screens::notices::Severity::Stopped => NoticeSeverity::Stopped,
                screens::notices::Severity::Warning => NoticeSeverity::Warning,
                screens::notices::Severity::Info => NoticeSeverity::Info,
                screens::notices::Severity::Slow => NoticeSeverity::Slow,
            },
            title: text.title,
            body: text.body,
            action: text.action,
            link: view.action.map(|action| match action {
                screens::notices::NoticeAction::DateTimeSettings => NoticeLink::DateTime,
                screens::notices::NoticeAction::PartySettings(party) => NoticeLink::Party {
                    party: party.into(),
                },
                screens::notices::NoticeAction::PrivacySettings => NoticeLink::Privacy,
            }),
        })
    }

    /// Privacy's words, with what wakes Pingers for its Wake-ups link.
    #[must_use]
    pub fn privacy_text(&self, push_service: &PushServiceInfo) -> PrivacyText {
        let view = screens::settings::privacy(self.client());
        let wakeups = screens::device::WakeupsSummary {
            pushes: view.wake_pushes,
            service: push_service.into(),
        };
        let text = pingers_words::settings::privacy(LANGUAGE, &view, &wakeups);
        PrivacyText {
            title: text.title,
            paused: text.paused,
            paused_detail: text.paused_detail,
            when_heading: text.when_heading,
            sharing: text.sharing,
            pause_in_focus: text.pause_in_focus,
            pause_in_focus_help: text.pause_in_focus_help,
            who_heading: text.who_heading,
            visibility: text
                .visibility
                .into_iter()
                .map(|option| VisibilityChoice {
                    value: option.value.into(),
                    label: option.label,
                    help: option.help,
                })
                .collect(),
            delivery_heading: text.delivery_heading,
            wakeups: text.wakeups,
            wakeups_summary: text.wakeups_summary,
            push_delivery: text.push_delivery,
            security_link: text.security_link,
        }
    }
}

#[cfg(test)]
mod tests;
