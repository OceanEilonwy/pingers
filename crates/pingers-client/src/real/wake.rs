// SPDX-License-Identifier: AGPL-3.0-only
//! [`WakePort`] over pingers-wake's `WakeState`, and the device settings the gate reads.
//!
//! One wake at a time: the state is loaded from the store on the first wake, each wake runs
//! with the settings read fresh, and the `WakeSnapshot` is stored after it. What the wake asks
//! of the shell comes back as the [`WakeOutcome`].

use core::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use pingers_gate::{
    FocusState, GateDecision, GateInputs, LocationSharing, MinuteOfDay, PauseState,
    QuietHoursSetting, decide,
};
use pingers_geo::{Fix, GeofenceCapacity, ZoneStatus};
use pingers_meetup::LocationVisibility;
use pingers_meetup_session::MeetupScreen;
use pingers_presence::DataMode;
use pingers_proto::{CoverReadRatio, GroupId, PartyId, PushHandle};
use pingers_store::{
    HandleKind, HandleOwner, LocationVisibility as StoredVisibility, Session,
    WakePushes as StoredPushes,
};
use pingers_time::{Clock, UnixSeconds};
use pingers_wake::{
    FIX_MAX_AGE, GeofenceEvent, GeofenceTransition as WakeTransition, MailboxReads, PushReason,
    WakeEnv, WakeInput as PipelineInput, WakeNotice, WakeReport, WakeSettings, WakeState,
    WakeTrigger,
};
use tokio::sync::Mutex as AsyncMutex;

use super::Backend;
use super::meetups::Meetups;
use super::records::{self, Key, Reader, Writer, corrupt};
use super::sync::Net;
use super::wake_codec;
use super::wake_ports::{Device, Lens, Time, region_id};
use crate::ports::{
    ClientEvent, Foreground, GeofenceTransition, PipelineSection, SettingChange, WakeInput,
    WakeOutcome, WakePort,
};
use crate::snapshot::{
    DailyWindow, Days, ExclusionZone, HideTrail, OwnPosition, PauseInFocus, QuietHoursPreference,
    Settings, Toggle, WakePushes, ZoneId, ZoneParties, ZoneSchedule, ZoneTimes,
};
use crate::{ClientError, Notice, Platform};

/// How long a wake's pipeline may run. Android gives a receiver or push about ten seconds,
/// and the Android shell cancels each core call after nine (`SINK_TIMEOUT_MILLIS`). A wake
/// cancelled from outside saves nothing, schedules no next wake and requests no fix; one that
/// runs out of its own budget ends its network steps, keeps its state consistent and is
/// retried soon. So the budget stays under the shell's limit, with room for what the client
/// does after the pipeline (joins, the snapshot).
pub(crate) const WAKE_BUDGET: Duration = Duration::from_secs(7);

/// The client's [`WakePort`].
pub(crate) struct Wake {
    backend: Arc<Backend>,
    platform: Arc<dyn Platform>,
    meetups: Arc<Meetups>,
    net: Arc<Net>,
    /// `None` until the first wake loads the stored snapshot.
    state: AsyncMutex<Option<WakeState>>,
    /// The gate's answer in the last wake, and the last fix: in memory only.
    last: Mutex<(Option<GateDecision>, Option<Fix>)>,
    /// A wake left knocks waiting for the liveness fix it asked for
    /// ([`WakeReport::knocks_awaiting_fix`]): the wake that delivers a fix reads mailboxes.
    /// In memory only: the fix comes back to this process, seconds later.
    knocks_awaiting_fix: AtomicBool,
}

impl Wake {
    pub(crate) fn new(
        backend: Arc<Backend>,
        platform: Arc<dyn Platform>,
        meetups: Arc<Meetups>,
        net: Arc<Net>,
    ) -> Self {
        Self {
            backend,
            platform,
            meetups,
            net,
            state: AsyncMutex::new(None),
            last: Mutex::new((None, None)),
            knocks_awaiting_fix: AtomicBool::new(false),
        }
    }

    fn last(&self) -> std::sync::MutexGuard<'_, (Option<GateDecision>, Option<Fix>)> {
        self.last.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The last fix the shell delivered, if it is younger than pingers-wake's `FIX_MAX_AGE`.
    fn fresh_fix(&self) -> Option<Fix> {
        let now = self.backend.outside.clock.now();
        self.last()
            .1
            .filter(|fix| now.saturating_duration_since(fix.taken_at) <= FIX_MAX_AGE)
    }

    /// Runs one pipeline wake and stores the state after it.
    pub(crate) async fn pipeline(
        &self,
        trigger: WakeTrigger,
    ) -> Result<(WakeReport, WakeOutcome), ClientError> {
        let mut state = self.state.lock().await;
        // A snapshot from before boards were kept per server belongs to the instance's server.
        // The links are loaded before the wake: its synchronous mailbox port reads them.
        self.net.servers.all().await?;
        let legacy_server = self.net.servers.built_in().cloned();
        let (stored, lens, snapshot) = self
            .backend
            .store(move |session, clock, rng| {
                Ok((
                    Stored::load(session)?,
                    Lens::load(session, clock, rng)?,
                    wake_codec::load(session, legacy_server.as_ref())?,
                ))
            })
            .await?;
        let wake_state = state.get_or_insert_with(|| WakeState::restore(snapshot));
        let device = Device::new(
            Arc::clone(&self.platform),
            stored.device.pause_in_focus == PauseInFocus::Off,
            self.fresh_fix(),
        );
        let time = Time {
            clock: Arc::clone(&self.backend.outside.clock),
            timer: Arc::clone(&self.backend.outside.timer),
        };
        let env = WakeEnv {
            device: &device,
            parties: &lens,
            server: self.net.as_ref(),
            meetups: self.meetups.as_ref(),
            time: &time,
        };
        let mut settings = stored.wake_settings(time.clock.now())?;
        // The fix a knock's liveness check waited for: read the knock again, with it to hand.
        if matches!(trigger, WakeTrigger::Fix(_))
            && self.knocks_awaiting_fix.swap(false, Ordering::Relaxed)
        {
            settings.mailbox_reads = MailboxReads::EveryWake;
        }
        let input = PipelineInput {
            trigger,
            settings,
            budget: WAKE_BUDGET,
        };
        let mut rng = (self.backend.outside.rng)();
        let report = Box::pin(wake_state.run(&env, input, &mut rng)).await?;
        if report.knocks_awaiting_fix > 0 {
            self.knocks_awaiting_fix.store(true, Ordering::Relaxed);
        }
        let snapshot = wake_state.snapshot();
        self.backend
            .store(move |session, _clock, _rng| wake_codec::save(session, &snapshot))
            .await?;
        self.last().0 = Some(report.decision);
        let mut outcome = device.into_outcome();
        outcome.events = report
            .notices
            .iter()
            .filter_map(|notice| {
                if let WakeNotice::UnansweredKnock(spot) = *notice {
                    Some(ClientEvent::Notice(Notice::UnansweredKnock {
                        party: spot.party,
                        spot: spot.spot,
                    }))
                } else {
                    None
                }
            })
            .chain(self.net.take_notices().into_iter().map(ClientEvent::Notice))
            .collect();
        Ok((report, outcome))
    }

    /// The wake-up pushes setting.
    async fn wake_pushes(&self) -> Result<WakePushes, ClientError> {
        self.backend
            .store(|session, _clock, _rng| Ok(Stored::load(session)?.wake_pushes))
            .await
    }

    /// What a push handle names: one of this device's mailboxes, a group's log (and which
    /// group), or neither.
    async fn push_reason(
        &self,
        handle: PushHandle,
    ) -> Result<(PushReason, Option<GroupId>), ClientError> {
        self.backend
            .store(move |session, _clock, _rng| {
                Ok(match session.push_handle_owner(&handle)? {
                    Some(HandleOwner {
                        kind: HandleKind::Mailbox,
                        ..
                    }) => (PushReason::Mailbox, None),
                    Some(HandleOwner {
                        kind: HandleKind::Subscription,
                        server,
                    }) => {
                        // The subscription's group, looked up on the server that holds it.
                        let group = session
                            .server(&server)?
                            .subscriptions()
                            .by_handle(&handle)?
                            .map(|subscription| subscription.group);
                        (PushReason::GroupCommit, group)
                    }
                    None => (PushReason::Unknown, None),
                })
            })
            .await
    }
}

#[async_trait]
impl WakePort for Wake {
    /// Joins are read like knock and reply mailboxes (design B.4.9): on a push that names a
    /// mailbox or does not say, on every wake with wake-up pushes off, and when the app comes
    /// to the foreground.
    async fn run(&self, input: WakeInput) -> Result<WakeOutcome, ClientError> {
        let mut opened = false;
        let trigger = match input {
            WakeInput::Fix(fix) => {
                self.last().1 = Some(fix);
                self.meetups.own_fix(fix).await;
                WakeTrigger::Fix(fix)
            }
            WakeInput::Geofence { region, transition } => WakeTrigger::Geofence(GeofenceEvent {
                region: region_id(&region)?,
                transition: match transition {
                    GeofenceTransition::Enter => WakeTransition::Enter,
                    GeofenceTransition::Dwell => WakeTransition::Dwell,
                    GeofenceTransition::Exit => WakeTransition::Exit,
                },
                at: self.backend.outside.clock.now(),
            }),
            WakeInput::Scheduled => WakeTrigger::Scheduled,
            WakeInput::Push(handle) => {
                let (reason, group) = self.push_reason(handle).await?;
                if let Some(group) = group {
                    self.meetups.pushed(group).await?;
                }
                WakeTrigger::Push(reason)
            }
            WakeInput::FocusChanged(_focus) => WakeTrigger::FocusChanged,
            WakeInput::Foreground(Foreground::Visible) => {
                self.meetups.screen(MeetupScreen::Open).await;
                opened = true;
                WakeTrigger::Opened
            }
            WakeInput::Foreground(Foreground::Hidden) => {
                self.meetups.screen(MeetupScreen::Closed).await;
                return Ok(WakeOutcome::default());
            }
        };
        let joins_due = opened
            || matches!(
                trigger,
                WakeTrigger::Push(PushReason::Mailbox | PushReason::Unknown)
            );
        let pipeline = Box::pin(self.pipeline(trigger)).await;
        // Joins are read even when the pipeline failed (no fix, a board that would not sync).
        let joins = if joins_due
            || self.wake_pushes().await? == WakePushes::NoWakePushes
            || super::join::waits_unannounced(&self.net).await?
        {
            super::join::poll(&self.net).await?
        } else {
            Vec::new()
        };
        let mut outcome = pipeline?.1;
        outcome.events.extend(joins);
        Ok(outcome)
    }

    async fn change_setting(&self, change: SettingChange) -> Result<(), ClientError> {
        self.backend
            .store(move |session, _clock, _rng| {
                let mut device = DeviceSettings::load(session)?;
                match change {
                    SettingChange::LocationSharing(sharing) => device.location_sharing = sharing,
                    SettingChange::Pause(pause) => device.pause = pause,
                    SettingChange::PauseInFocus(setting) => device.pause_in_focus = setting,
                    SettingChange::QuietHours(quiet) => device.quiet_hours = quiet,
                    SettingChange::HideTrail(setting) => device.hide_trail = setting,
                    SettingChange::Visibility(visibility) => {
                        return Ok(session
                            .settings()
                            .set_location_visibility(stored_visibility(visibility))?);
                    }
                    SettingChange::Zones(zones) => {
                        return records::write(session, &Key::Zones, |writer| {
                            write_zones(writer, &zones);
                        });
                    }
                }
                device.save(session)
            })
            .await
    }

    async fn section(&self) -> Result<PipelineSection, ClientError> {
        let stored = self
            .backend
            .store(|session, _clock, _rng| Stored::load(session))
            .await?;
        let focus = self.platform.focus_state();
        let now = self.backend.outside.clock.now();
        let (last_decision, last_fix) = *self.last();
        // Before the first wake the gate has not run; ask it with no zone known to apply.
        let gate = match last_decision {
            Some(decision) => decision,
            None => decide(&GateInputs {
                location_sharing: stored.device.location_sharing,
                pause: stored.device.pause,
                focus: match stored.device.pause_in_focus {
                    PauseInFocus::On => focus,
                    PauseInFocus::Off => FocusState::Off,
                },
                quiet_hours: stored.device.quiet_setting(),
                local_time: local_time(now)?,
                zones: ZoneStatus::Clear,
            }),
        };
        Ok(PipelineSection {
            settings: Settings {
                location_sharing: stored.device.location_sharing,
                pause_in_focus: stored.device.pause_in_focus,
                focus,
                quiet_hours: stored.device.quiet_hours,
                visibility: stored.visibility,
                wake_pushes: stored.wake_pushes,
                hide_trail: stored.device.hide_trail,
            },
            gate,
            zones: stored.zones,
            own_position: last_fix.map(|fix| OwnPosition { fix, heading: None }),
        })
    }
}

/// The local minute of the day.
///
/// WAITING(platform): `Platform` does not report the local time zone yet, so this is UTC and
/// quiet hours are read in UTC until the shell passes its offset.
fn local_time(now: UnixSeconds) -> Result<MinuteOfDay, ClientError> {
    let minute = now
        .get()
        .checked_rem(86_400)
        .unwrap_or(0)
        .checked_div(60)
        .unwrap_or(0);
    MinuteOfDay::new(u16::try_from(minute).map_err(|_never| corrupt())?).map_err(|_never| corrupt())
}

/// Everything a wake or the settings screen reads from the store.
struct Stored {
    device: DeviceSettings,
    visibility: LocationVisibility,
    wake_pushes: WakePushes,
    zones: Vec<ExclusionZone>,
}

impl Stored {
    fn load(session: &Session) -> Result<Self, ClientError> {
        Ok(Self {
            device: DeviceSettings::load(session)?,
            visibility: match session.settings().location_visibility()? {
                StoredVisibility::AskEachTime => LocationVisibility::AskEachTime,
                StoredVisibility::Everyone => LocationVisibility::Everyone,
                StoredVisibility::Nobody => LocationVisibility::Nobody,
            },
            wake_pushes: match session.settings().wake_pushes()? {
                StoredPushes::Enabled => WakePushes::On,
                StoredPushes::Disabled => WakePushes::NoWakePushes,
            },
            zones: records::read(session, &Key::Zones, read_zones)?.unwrap_or_default(),
        })
    }

    /// The settings pingers-wake takes.
    ///
    /// Every zone is passed, whatever its schedule: evaluating schedules is not the client's
    /// job and hiding more than asked fails safe. WAITING(pingers-sync): `cover_read_ratio`
    /// comes from the server's `/v1/config`; until then every upload reads the board.
    fn wake_settings(&self, now: UnixSeconds) -> Result<WakeSettings, ClientError> {
        Ok(WakeSettings {
            location_sharing: self.device.location_sharing,
            pause: self.device.pause,
            quiet_hours: self.device.quiet_setting(),
            local_time: local_time(now)?,
            zones: self.zones.iter().map(|zone| zone.fence.clone()).collect(),
            data_mode: DataMode::Normal,
            cover_read_ratio: CoverReadRatio::ALWAYS,
            mailbox_reads: match self.wake_pushes {
                WakePushes::On => MailboxReads::OnPush,
                WakePushes::NoWakePushes => MailboxReads::EveryWake,
            },
            geofence_capacity: GeofenceCapacity::ANDROID,
        })
    }
}

/// The device-wide switches no store table holds yet.
#[derive(Clone, Copy)]
struct DeviceSettings {
    location_sharing: LocationSharing,
    pause: PauseState,
    pause_in_focus: PauseInFocus,
    quiet_hours: QuietHoursPreference,
    hide_trail: HideTrail,
}

impl DeviceSettings {
    /// The stored settings, or the first-run defaults: sharing on, not paused, Focus pauses,
    /// quiet hours off (window 22:00 to 07:00), trail hidden.
    fn load(session: &Session) -> Result<Self, ClientError> {
        if let Some(stored) = records::read(session, &Key::DeviceSettings, Self::decode)? {
            return Ok(stored);
        }
        Ok(Self {
            location_sharing: LocationSharing::On,
            pause: PauseState::NotPaused,
            pause_in_focus: PauseInFocus::On,
            quiet_hours: QuietHoursPreference {
                state: Toggle::Off,
                window: window(22 * 60, 7 * 60)?,
            },
            hide_trail: HideTrail::On,
        })
    }

    fn save(self, session: &Session) -> Result<(), ClientError> {
        records::write(session, &Key::DeviceSettings, |writer| {
            writer.byte(u8::from(self.location_sharing == LocationSharing::On));
            writer.byte(u8::from(self.pause == PauseState::Paused));
            writer.byte(u8::from(self.pause_in_focus == PauseInFocus::On));
            writer.byte(u8::from(self.quiet_hours.state == Toggle::On));
            write_window(writer, self.quiet_hours.window);
            writer.byte(u8::from(self.hide_trail == HideTrail::On));
        })
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, ClientError> {
        Ok(Self {
            location_sharing: pick(reader, LocationSharing::On, LocationSharing::Off)?,
            pause: pick(reader, PauseState::Paused, PauseState::NotPaused)?,
            pause_in_focus: pick(reader, PauseInFocus::On, PauseInFocus::Off)?,
            quiet_hours: QuietHoursPreference {
                state: pick(reader, Toggle::On, Toggle::Off)?,
                window: read_window(reader)?,
            },
            hide_trail: pick(reader, HideTrail::On, HideTrail::Off)?,
        })
    }

    fn quiet_setting(&self) -> QuietHoursSetting {
        match self.quiet_hours.state {
            Toggle::Off => QuietHoursSetting::Off,
            Toggle::On => QuietHoursSetting::Scheduled(self.quiet_hours.window),
        }
    }
}

fn pick<T>(reader: &mut Reader<'_>, on: T, off: T) -> Result<T, ClientError> {
    match reader.byte()? {
        1 => Ok(on),
        0 => Ok(off),
        _ => Err(corrupt()),
    }
}

fn window(start: u16, end: u16) -> Result<DailyWindow, ClientError> {
    let minute = |value| MinuteOfDay::new(value).map_err(|_bad| corrupt());
    DailyWindow::new(minute(start)?, minute(end)?).map_err(|_bad| corrupt())
}

fn write_window(writer: &mut Writer, window: DailyWindow) {
    writer.u16(window.start().get());
    writer.u16(window.end().get());
}

fn read_window(reader: &mut Reader<'_>) -> Result<DailyWindow, ClientError> {
    let start = reader.u16()?;
    window(start, reader.u16()?)
}

fn write_zones(writer: &mut Writer, zones: &[ExclusionZone]) {
    writer.len(zones.len());
    for zone in zones {
        writer.u32(zone.id.get());
        writer.text(zone.name.as_str());
        writer.fence(&zone.fence);
        match zone.schedule.times {
            ZoneTimes::Always => writer.byte(0),
            ZoneTimes::Window { days, window } => {
                writer.byte(match days {
                    Days::EveryDay => 1,
                    Days::Weekdays => 2,
                    Days::Weekends => 3,
                });
                write_window(writer, window);
            }
        }
        match &zone.schedule.parties {
            ZoneParties::All => writer.option(None::<()>, |_writer, ()| {}),
            ZoneParties::Only(parties) => writer.option(Some(parties), |writer, parties| {
                writer.len(parties.len());
                for party in parties {
                    writer.fixed(party.as_bytes());
                }
            }),
        }
    }
}

fn read_zones(reader: &mut Reader<'_>) -> Result<Vec<ExclusionZone>, ClientError> {
    reader.list(|reader| {
        let id = ZoneId::new(reader.u32()?);
        let name = crate::snapshot::Name::new(&reader.text()?).map_err(|_bad| corrupt())?;
        let fence = reader.fence()?;
        let times = match reader.byte()? {
            0 => ZoneTimes::Always,
            code => ZoneTimes::Window {
                days: match code {
                    1 => Days::EveryDay,
                    2 => Days::Weekdays,
                    3 => Days::Weekends,
                    _ => return Err(corrupt()),
                },
                window: read_window(reader)?,
            },
        };
        let parties = match reader
            .option(|reader| reader.list(|reader| Ok(PartyId::from_bytes(reader.fixed()?))))?
        {
            None => ZoneParties::All,
            Some(parties) => ZoneParties::Only(parties),
        };
        Ok(ExclusionZone {
            id,
            name,
            fence,
            schedule: ZoneSchedule { times, parties },
        })
    })
}

const fn stored_visibility(visibility: LocationVisibility) -> StoredVisibility {
    match visibility {
        LocationVisibility::AskEachTime => StoredVisibility::AskEachTime,
        LocationVisibility::Everyone => StoredVisibility::Everyone,
        LocationVisibility::Nobody => StoredVisibility::Nobody,
    }
}
