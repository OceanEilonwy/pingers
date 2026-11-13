// SPDX-License-Identifier: AGPL-3.0-only
//! One wake, start to finish (design E.2).

use core::future::Future;
use core::time::Duration;

use pingers_crypto::{AccountPublicKey, RevokeKey};
use pingers_gate::{
    GateDecision, GateInputs, LocationSharing, MinuteOfDay, PauseState, QuietHoursSetting, decide,
};
use pingers_geo::{
    Fence, Fix, GeofenceCandidate, GeofenceCapacity, PresenceInput, PresenceState, SpotPresence,
    ZoneStatus, select_os_geofences,
};
use pingers_presence::{
    ActiveSpot, BoardMatch, DataMode, EpochChange, FreshFix, KnockContext, KnockDecision,
    KnockFilter, KnockInputs, KnockSituation, LeaseInputs, LivenessEvidence, MailboxPurpose,
    PresenceError, ReadPolicy, RealSlot, RosterLookup, Sharing, SlotCandidate, SlotContent,
    SpotRef, SpotStatus, TrialMatcher, UploadPlan, UploadTrigger, VerifiedKnock, build_knock,
    build_lease, choose_slots, decide_knock, knock_targets, knock_unanswered, matching_epochs,
    open_knock, plan_upload, read_board, verify_knock,
};
use pingers_proto::{
    BoardPage, CoverReadRatio, Epoch, GroupId, LeafIndex, MailboxId, MeetupHint, PartyId,
    ReadBoard, RecordRandomness,
};
use pingers_time::{Bucket, Clock, Timer, UnixSeconds};
use rand_core::CryptoRng;

use crate::budget::Budget;
use crate::ports::{
    DevicePort, FixQuality, LeaseUpload, LocationMode, MeetupAdmission, MeetupPort, OsRegion,
    PartyInfo, PartyView, RegionId, ReplyPost, ServerPort,
};
use crate::state::{
    DecidedKnock, GeofenceAnchor, KnockStatus, KnownEpoch, LeaseSlot, LiveLease, RealRecord,
    SentKnock, SpotTransition, TrackedSpot, WakeState,
};
use crate::{PortError, WakeError};

/// A fix younger than this is fresh enough; otherwise the wake asks for one (design E.2).
pub const FIX_MAX_AGE: Duration = Duration::from_secs(60);

/// How long the routine fix may take (design E.2: `request_fix(Balanced, 15 s)`).
pub const ROUTINE_FIX_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the liveness fix before answering a knock may take (design F.3: about 10 s of GPS).
pub const LIVENESS_FIX_TIMEOUT: Duration = Duration::from_secs(10);

/// When a wake ended early, the next one comes this soon.
pub const RETRY_AFTER: Duration = Duration::from_mins(1);

/// The ports a wake runs against, and the clock. pingers-client builds one per wake.
pub struct WakeEnv<'env, D, P, S, M, T> {
    /// The shell.
    pub device: &'env D,
    /// Parties and keys (pingers-parties).
    pub parties: &'env P,
    /// The server (pingers-sync).
    pub server: &'env S,
    /// Live Meetups (pingers-meetup-session).
    pub meetups: &'env M,
    /// Wall time for buckets and expiries, monotonic time for the budget.
    pub time: &'env T,
}

/// An OS geofence transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GeofenceTransition {
    /// Entered the region.
    Enter,
    /// Left the region.
    Exit,
    /// Dwelt in the region (Android `DWELL`).
    Dwell,
}

/// An OS geofence event, as the shell reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeofenceEvent {
    /// Which region.
    pub region: RegionId,
    /// What happened.
    pub transition: GeofenceTransition,
    /// When.
    pub at: UnixSeconds,
}

/// What a push said it was for, as far as the core can tell from its handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PushReason {
    /// One of this device's mailboxes has a post (a knock or a Welcome).
    Mailbox,
    /// A Party group has a new Commit (design B.4.7): re-post at once.
    GroupCommit,
    /// The handle did not say.
    Unknown,
}

/// Why the wake runs (design E.2, entry points).
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum WakeTrigger {
    /// A location fix arrived.
    Fix(Fix),
    /// An OS geofence transition.
    Geofence(GeofenceEvent),
    /// A scheduled wake (WorkManager) or the app opening.
    Scheduled,
    /// A push.
    Push(PushReason),
    /// Focus / DND changed.
    FocusChanged,
}

/// When knock and reply mailboxes are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MailboxReads {
    /// On a push that names a mailbox, or does not say (the default).
    OnPush,
    /// On every wake: the "No wake-up pushes" setting, where nothing can push (design B.4.4).
    EveryWake,
}

/// The settings a wake reads, fresh, from pingers-client.
#[derive(Clone, Debug, PartialEq)]
pub struct WakeSettings {
    /// The app-wide switch.
    pub location_sharing: LocationSharing,
    /// The manual pause.
    pub pause: PauseState,
    /// Quiet hours.
    pub quiet_hours: QuietHoursSetting,
    /// The local time of day, from the shell.
    pub local_time: MinuteOfDay,
    /// Exclusion zones (local only, design B.11).
    pub zones: Vec<Fence>,
    /// "Low data".
    pub data_mode: DataMode,
    /// The server's `cover_read_ratio`.
    pub cover_read_ratio: CoverReadRatio,
    /// When to read mailboxes.
    pub mailbox_reads: MailboxReads,
    /// How many Spot regions the platform allows.
    pub geofence_capacity: GeofenceCapacity,
}

/// One wake's inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct WakeInput {
    /// Why it runs.
    pub trigger: WakeTrigger,
    /// The settings, read fresh.
    pub settings: WakeSettings,
    /// How long the platform lets it run.
    pub budget: Duration,
}

/// A step of the wake that waits on something outside the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WakeStep {
    /// The routine fix.
    Fix,
    /// Leaving Meetups because sharing is suppressed.
    LeaveMeetups,
    /// Pulling the group logs.
    GroupLogs,
    /// Uploading the lease (and the first board page).
    Upload,
    /// Further board pages.
    BoardPages,
    /// Posting a knock.
    Knock,
    /// Reading knock and reply mailboxes.
    Mailboxes,
    /// The liveness fix.
    LivenessFix,
    /// Handing a knock or a Welcome to pingers-meetup-session.
    Handoff,
    /// Registering a mailbox for the pool.
    MailboxPool,
}

/// How a wake ended. Whatever the ending, presence is up to date, every lease the server may
/// hold is remembered, and the next wake is scheduled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeEnding {
    /// Every step ran.
    Completed,
    /// The budget ran out during this step; later network steps were skipped.
    OutOfBudget(WakeStep),
    /// A port failed during this step; later network steps were skipped.
    PortFailed {
        /// The step.
        step: WakeStep,
        /// What failed.
        error: PortError,
    },
}

/// What happened to the lease in this wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadOutcome {
    /// No upload was due before this time (design B.4.5 spacing).
    NotDue(UnixSeconds),
    /// Uploaded.
    Uploaded {
        /// Why.
        trigger: UploadTrigger,
        /// How many of the four records were real (local knowledge, never sent).
        real_records: usize,
        /// Whether the board was read.
        read: ReadBoard,
    },
    /// Due, but the wake ended before or during the upload.
    Interrupted(UploadTrigger),
}

/// The decision on one knock this device received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnockOutcome {
    /// The Spot.
    pub spot: SpotRef,
    /// The decision.
    pub decision: KnockDecision,
}

/// Something to tell the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WakeNotice {
    /// A knock at this Spot went unanswered for a lease (design B.4.4 step 7).
    UnansweredKnock(SpotRef),
}

/// What one wake did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeReport {
    /// The gate's answer.
    pub decision: GateDecision,
    /// Spots that changed state.
    pub transitions: Vec<SpotTransition>,
    /// The lease.
    pub upload: UploadOutcome,
    /// Spots this device is in where a peer's record matched ("a member of the Party is
    /// here"). Never a Spot the device is not in.
    pub matched: Vec<SpotRef>,
    /// Knocks posted.
    pub knocks_sent: usize,
    /// Knocks received and decided.
    pub knock_decisions: Vec<KnockOutcome>,
    /// Welcomes handed to pingers-meetup-session.
    pub welcomes: usize,
    /// Notices.
    pub notices: Vec<WakeNotice>,
    /// The delay given to `schedule_wake`.
    pub next_wake: Duration,
    /// How it ended.
    pub ending: WakeEnding,
}

impl WakeState {
    /// Runs one wake: gate, presence, geofences, group logs, lease and board, knocks, mailboxes
    /// and liveness checks, then the next geofences and wake (design E.2).
    ///
    /// Network failures and an exhausted budget end the network part early
    /// ([`WakeReport::ending`]); the state stays consistent and the next wake is scheduled.
    ///
    /// # Errors
    ///
    /// [`WakeError::Presence`] if a protocol step fails in a way its inputs rule out (a crypto
    /// failure while deriving keys, sealing or building a lease).
    pub async fn run<D, P, S, M, T, R>(
        &mut self,
        env: &WakeEnv<'_, D, P, S, M, T>,
        input: WakeInput,
        rng: &mut R,
    ) -> Result<WakeReport, WakeError>
    where
        D: DevicePort,
        P: PartyView,
        S: ServerPort,
        M: MeetupPort,
        T: Clock + Timer,
        R: CryptoRng + Send,
    {
        let mut run = Run {
            env,
            rng,
            trigger: input.trigger,
            settings: &input.settings,
            budget: Budget::start(env.time, input.budget),
            now: Clock::now(env.time),
            ending: None,
            report: Progress::default(),
        };
        run.wake(self).await
    }
}

/// What the report collects as the wake goes.
#[derive(Default)]
struct Progress {
    transitions: Vec<SpotTransition>,
    matched: Vec<SpotRef>,
    knocks_sent: usize,
    knock_decisions: Vec<KnockOutcome>,
    welcomes: usize,
    notices: Vec<WakeNotice>,
}

/// A knock that opened and verified under one of this device's records.
struct ReceivedKnock {
    real: RealRecord,
    own_record: RecordRandomness,
    knock: VerifiedKnock,
}

struct Run<'run, 'env, D, P, S, M, T: Timer, R> {
    env: &'run WakeEnv<'env, D, P, S, M, T>,
    rng: &'run mut R,
    trigger: WakeTrigger,
    settings: &'run WakeSettings,
    budget: Budget<'env, T>,
    now: UnixSeconds,
    ending: Option<WakeEnding>,
    report: Progress,
}

impl<D, P, S, M, T, R> Run<'_, '_, D, P, S, M, T, R>
where
    D: DevicePort,
    P: PartyView,
    S: ServerPort,
    M: MeetupPort,
    T: Clock + Timer,
    R: CryptoRng + Send,
{
    async fn wake(&mut self, state: &mut WakeState) -> Result<WakeReport, WakeError> {
        let trigger = self.trigger;
        let parties = self.env.parties.parties();
        state.reconcile(&parties, self.now);
        state.prune(self.now);
        self.expire_knocks(state);

        let fix = self.routine_fix(state).await;
        let zones = self.observe_zones(state, fix);
        let decision = self.gate(zones);
        self.update_presence(state, fix, decision);
        let horizon_exit = matches!(
            trigger,
            WakeTrigger::Geofence(GeofenceEvent {
                region: RegionId::Horizon,
                transition: GeofenceTransition::Exit,
                ..
            })
        );
        self.choose_geofences(state, horizon_exit);

        if decision != GateDecision::Share && self.any_meetup(state) {
            let meetups = self.env.meetups;
            self.port(WakeStep::LeaveMeetups, meetups.leave_all()).await;
        }

        let Synced {
            parties,
            epoch_changed,
            upload_trigger,
            plan,
            network_needed,
            reads_due,
        } = self.sync_parties(state, parties, decision).await;
        let active = self.derive_keys(state, &parties, decision)?;
        let keys = Keys {
            parties: &parties,
            active: &active,
        };
        // Knocks answer the records the server held when the wake began, including any the
        // upload is about to replace.
        let held_at_start = state.leases.clone();
        let upload = match plan {
            UploadPlan::NotBefore(time) => UploadOutcome::NotDue(time),
            UploadPlan::UploadNow => {
                let epoch_change = if epoch_changed {
                    EpochChange::Changed
                } else {
                    EpochChange::Unchanged
                };
                let why = UploadReason {
                    trigger: upload_trigger,
                    epoch_change,
                };
                self.upload(state, &keys, why).await?
            }
        };

        if reads_due {
            self.answer_knocks(state, &keys, &held_at_start).await;
            self.read_replies(state).await;
        }
        if network_needed {
            let server = self.env.server;
            self.port(WakeStep::MailboxPool, server.refill_mailbox_pool())
                .await;
        }

        let next_wake = self.finish(state);
        Ok(WakeReport {
            decision,
            transitions: core::mem::take(&mut self.report.transitions),
            upload,
            matched: core::mem::take(&mut self.report.matched),
            knocks_sent: self.report.knocks_sent,
            knock_decisions: core::mem::take(&mut self.report.knock_decisions),
            welcomes: self.report.welcomes,
            notices: core::mem::take(&mut self.report.notices),
            next_wake,
            ending: self.ending.unwrap_or(WakeEnding::Completed),
        })
    }

    /// Decides whether this wake needs the network at all (an upload due, a Commit pushed, or
    /// mailboxes to read); if so pulls the group logs first (design B.4.7) and decides again
    /// with the new epochs. A quiet wake makes no request.
    async fn sync_parties(
        &mut self,
        state: &mut WakeState,
        mut parties: Vec<PartyInfo>,
        decision: GateDecision,
    ) -> Synced {
        let reads_due = self.settings.mailbox_reads == MailboxReads::EveryWake
            || matches!(
                self.trigger,
                WakeTrigger::Push(PushReason::Mailbox | PushReason::Unknown)
            );
        let commit_push = self.trigger == WakeTrigger::Push(PushReason::GroupCommit);
        let mut epoch_changed = commit_push | note_epochs(state, &parties);
        let facts = Facts {
            decision,
            epoch_changed,
        };
        let mut upload_trigger = self.upload_trigger(state, &parties, facts);
        let mut plan = plan_upload(upload_trigger, state.last_upload, self.now);
        // A Commit push always makes the upload due (`EpochChanged`).
        let network_needed = plan == UploadPlan::UploadNow || reads_due;
        let server = self.env.server;
        if network_needed
            && self
                .port(WakeStep::GroupLogs, server.pull_group_logs())
                .await
                .is_some()
        {
            parties = self.env.parties.parties();
            state.reconcile(&parties, self.now);
            epoch_changed |= note_epochs(state, &parties);
            let facts = Facts {
                decision,
                epoch_changed,
            };
            upload_trigger = self.upload_trigger(state, &parties, facts);
            plan = plan_upload(upload_trigger, state.last_upload, self.now);
        }
        Synced {
            parties,
            epoch_changed,
            upload_trigger,
            plan,
            network_needed,
            reads_due,
        }
    }

    /// Awaits a port call within the budget. After the first failure or timeout, later calls
    /// are not made.
    async fn port<O>(
        &mut self,
        step: WakeStep,
        call: impl Future<Output = Result<O, PortError>>,
    ) -> Option<O> {
        if self.ending.is_some() {
            return None;
        }
        match self.budget.within(call).await {
            None => {
                self.ending = Some(WakeEnding::OutOfBudget(step));
                None
            }
            Some(Err(error)) => {
                self.ending = Some(WakeEnding::PortFailed { step, error });
                None
            }
            Some(Ok(output)) => Some(output),
        }
    }

    /// Asks the platform for a fix within the budget.
    async fn fix(&mut self, step: WakeStep, quality: FixQuality, timeout: Duration) -> Option<Fix> {
        if self.ending.is_some() {
            return None;
        }
        let device = self.env.device;
        let answer = self
            .budget
            .within(device.request_fix(quality, timeout))
            .await;
        if answer.is_none() {
            self.ending = Some(WakeEnding::OutOfBudget(step));
        }
        answer.flatten()
    }

    /// The gate, with Focus read fresh (design B.10: at every wake and every knock).
    fn gate(&self, zones: ZoneStatus) -> GateDecision {
        decide(&GateInputs {
            location_sharing: self.settings.location_sharing,
            pause: self.settings.pause,
            focus: self.env.device.focus_state(),
            quiet_hours: self.settings.quiet_hours,
            local_time: self.settings.local_time,
            zones,
        })
    }

    /// The fix that came with the wake, or a fresh one if the last is older than a minute, a
    /// Spot is waiting for a confirming fix, or sharing resumes (every Spot restarts Outside). No fix is taken while sharing is suppressed for
    /// a reason a fix cannot change.
    async fn routine_fix(&mut self, state: &WakeState) -> Option<Fix> {
        if let WakeTrigger::Fix(fix) = self.trigger {
            return Some(fix);
        }
        let before = self.gate(state.trail.status_at(self.now));
        let fix_could_matter = matches!(
            before,
            GateDecision::Share
                | GateDecision::Suppress(pingers_gate::SuppressReason::ExclusionZone)
        );
        let last_fix_old = state
            .last_fix
            .is_none_or(|last| self.now.saturating_duration_since(last.taken_at) >= FIX_MAX_AGE);
        let resuming = before == GateDecision::Share
            && state
                .spots
                .iter()
                .any(|tracked| tracked.presence.state() == PresenceState::Suppressed);
        if fix_could_matter && (last_fix_old || resuming || awaiting_fix(state)) {
            self.fix(WakeStep::Fix, FixQuality::Balanced, ROUTINE_FIX_TIMEOUT)
                .await
        } else {
            None
        }
    }

    /// Exclusion zones first (design B.11); remembers the newest fix.
    fn observe_zones(&self, state: &mut WakeState, fix: Option<Fix>) -> ZoneStatus {
        let Some(fix) = fix else {
            return state.trail.status_at(self.now);
        };
        if state
            .last_fix
            .is_none_or(|last| last.taken_at <= fix.taken_at)
        {
            state.last_fix = Some(fix);
        }
        state.trail.observe(&self.settings.zones, &fix)
    }

    /// Feeds the gate's answer, the OS event and the fix to every Spot, oldest input first,
    /// then a tick for the timeouts.
    fn update_presence(&mut self, state: &mut WakeState, fix: Option<Fix>, decision: GateDecision) {
        if decision != GateDecision::Share {
            let changes = state.feed(None, PresenceInput::Suppress { at: self.now });
            self.report.transitions.extend(changes);
            return;
        }
        let mut inputs: Vec<(Option<SpotRef>, PresenceInput, UnixSeconds)> = Vec::new();
        if let WakeTrigger::Geofence(event) = self.trigger
            && let RegionId::Spot(spot) = event.region
        {
            let at = event.at;
            let input = match event.transition {
                GeofenceTransition::Enter => PresenceInput::OsEnter { at },
                GeofenceTransition::Exit => PresenceInput::OsExit { at },
                GeofenceTransition::Dwell => PresenceInput::OsDwell { at },
            };
            inputs.push((Some(spot), input, at));
        }
        if let Some(fix) = fix {
            inputs.push((None, PresenceInput::Fix(fix), fix.taken_at));
        }
        inputs.sort_by_key(|(_spot, _input, at)| *at);
        let resume_at = inputs
            .first()
            .map_or(self.now, |(_spot, _input, at)| (*at).min(self.now));
        let suppressed: Vec<SpotRef> = state
            .spots
            .iter()
            .filter(|tracked| tracked.presence.state() == PresenceState::Suppressed)
            .map(|tracked| tracked.spot)
            .collect();
        for spot in suppressed {
            let changes = state.feed(Some(spot), PresenceInput::Resume { at: resume_at });
            self.report.transitions.extend(changes);
        }
        for (spot, input, _at) in inputs {
            let changes = state.feed(spot, input);
            self.report.transitions.extend(changes);
        }
        let changes = state.feed(None, PresenceInput::Tick { at: self.now });
        self.report.transitions.extend(changes);
    }

    /// Registers the nearest Spots and a horizon region when the Spots changed, the device
    /// left the horizon, or nothing was registered yet (design E.2).
    fn choose_geofences(&self, state: &mut WakeState, horizon_exit: bool) {
        let Some(position) = state.last_fix.map(|fix| fix.coordinate) else {
            return;
        };
        let spots: Vec<(SpotRef, Fence)> = state
            .spots
            .iter()
            .map(|tracked| (tracked.spot, tracked.presence.fence().clone()))
            .collect();
        let left_horizon = state
            .geofences
            .as_ref()
            .and_then(|anchor| anchor.horizon)
            .is_some_and(|horizon| !horizon.contains(position));
        if !horizon_exit && !left_horizon && state.geofences_cover(&spots) {
            return;
        }
        let candidates: Vec<GeofenceCandidate<'_, SpotRef>> = spots
            .iter()
            .map(|(spot, fence)| GeofenceCandidate { key: *spot, fence })
            .collect();
        let selection = select_os_geofences(&candidates, position, self.settings.geofence_capacity);
        let mut regions: Vec<OsRegion> = selection
            .regions
            .iter()
            .map(|chosen| OsRegion {
                id: RegionId::Spot(chosen.key),
                region: chosen.region,
            })
            .collect();
        regions.extend(selection.horizon.map(|horizon| OsRegion {
            id: RegionId::Horizon,
            region: horizon,
        }));
        self.env.device.set_os_geofences(&regions);
        state.geofences = Some(GeofenceAnchor {
            horizon: selection.horizon,
            spots,
        });
    }

    fn any_meetup(&self, state: &WakeState) -> bool {
        state
            .spots
            .iter()
            .any(|tracked| self.env.meetups.current_meetup(tracked.spot).is_some())
    }

    /// The Spots that get a real record: confirmed Inside, sharing allowed, at the Party's
    /// current epoch (posting always uses the current epoch, design B.4.7).
    fn posting(state: &WakeState, parties: &[PartyInfo], decision: GateDecision) -> Vec<RealKey> {
        if decision != GateDecision::Share {
            return Vec::new();
        }
        parties
            .iter()
            .flat_map(|party| {
                party.spots.iter().map(move |info| RealKey {
                    spot: SpotRef {
                        party: party.party,
                        spot: info.spot,
                    },
                    epoch: party.epoch.current,
                })
            })
            .filter(|key| state.spot(key.spot).is_some_and(TrackedSpot::inside))
            .collect()
    }

    /// Why to upload (design B.4.5, B.4.7, B.10).
    fn upload_trigger(
        &self,
        state: &WakeState,
        parties: &[PartyInfo],
        facts: Facts,
    ) -> UploadTrigger {
        let Facts {
            decision,
            epoch_changed,
        } = facts;
        let posting = Self::posting(state, parties, decision);
        // Only a confirmed Spot with sharing allowed can become Inside in this wake.
        let entered = self
            .report
            .transitions
            .iter()
            .any(|change| change.transition.to == PresenceState::Inside);
        let stale_record = state.leases.iter().any(|lease| {
            lease.real_records().any(|(_slot, real)| {
                !posting
                    .iter()
                    .any(|key| key.spot == real.spot && key.epoch == real.epoch)
            })
        });
        if entered {
            UploadTrigger::ConfirmedEntry
        } else if epoch_changed {
            UploadTrigger::EpochChanged
        } else if stale_record && decision != GateDecision::Share {
            UploadTrigger::SuppressedWithRealRecord
        } else if stale_record {
            UploadTrigger::ExitWithRealRecord
        } else {
            UploadTrigger::Wake
        }
    }

    /// Spot keys for every confirmed-Inside Spot at every epoch `matching_epochs` allows.
    /// They live only for this wake: a Spot that leaves Inside, or an epoch that falls out of
    /// the grace period, has no keys in the next one.
    fn derive_keys(
        &self,
        state: &WakeState,
        parties: &[PartyInfo],
        decision: GateDecision,
    ) -> Result<Vec<ActiveSpot>, PresenceError> {
        let mut active = Vec::new();
        if decision != GateDecision::Share {
            return Ok(active);
        }
        for party in parties {
            let epochs = matching_epochs(&party.epoch, self.now);
            for info in &party.spots {
                let spot = SpotRef {
                    party: party.party,
                    spot: info.spot,
                };
                if !state.spot(spot).is_some_and(TrackedSpot::inside) {
                    continue;
                }
                for epoch in core::iter::once(epochs.current).chain(epochs.previous) {
                    let Some(root) = self.env.parties.presence_root(party.party, epoch) else {
                        continue;
                    };
                    active.push(ActiveSpot::derive(
                        &root,
                        &SpotStatus {
                            spot,
                            epoch,
                            state: PresenceState::Inside,
                        },
                    )?);
                }
            }
        }
        Ok(active)
    }

    /// Picks up to four Spots for real records and takes a knock mailbox for each; a Spot
    /// without a mailbox gets no record this time (design B.4.2, B.4.4).
    fn real_slots<'keys>(
        &self,
        state: &WakeState,
        keys: &Keys<'keys>,
        start: Bucket,
    ) -> Vec<RealSlot<'keys>> {
        let Keys { parties, active } = *keys;
        let newest = state.leases.last().copied();
        let held = |spot: SpotRef| {
            newest.is_some_and(|lease| lease.real_records().any(|(_slot, real)| real.spot == spot))
        };
        let mut candidates: Vec<SlotCandidate<'keys>> = active
            .iter()
            .filter(|keys| current_epoch(parties, keys.spot()) == Some(keys.epoch()))
            .map(|keys| SlotCandidate {
                spot: keys,
                last_peer_activity: state.peer_activity_at(keys.spot()),
            })
            .collect();
        // Spots left out of the last lease go first, so ties rotate (design B.4.2).
        candidates.sort_by_key(|candidate| held(candidate.spot.spot()));
        let mut real = Vec::new();
        for candidate in choose_slots(candidates) {
            let Some(knock_mailbox) = self.env.server.take_mailbox(MailboxPurpose::Knock, start)
            else {
                continue;
            };
            let meetup_hint = self
                .env
                .meetups
                .current_meetup(candidate.spot.spot())
                .map(|group| MeetupHint::for_group(&group));
            real.push(RealSlot {
                spot: candidate.spot,
                knock_mailbox,
                meetup_hint,
            });
        }
        real
    }

    /// Builds the lease, remembers it, uploads it, and trial-matches and knocks on what the
    /// board shows.
    async fn upload(
        &mut self,
        state: &mut WakeState,
        keys: &Keys<'_>,
        why: UploadReason,
    ) -> Result<UploadOutcome, WakeError> {
        let Keys { parties, active } = *keys;
        if self.ending.is_some() {
            return Ok(UploadOutcome::Interrupted(why.trigger));
        }
        let start = self.now.bucket();
        let real = self.real_slots(state, keys, start);
        let real_records = real.len();
        let revoke_key = RevokeKey::generate(self.rng);
        let built = build_lease(
            &LeaseInputs {
                start,
                real,
                revoke_key: &revoke_key,
            },
            self.rng,
        )?;
        let live = remember(start, &built.slots, parties);
        // Remembered before the upload: if the wake stops mid-request the server may still
        // hold it, so its records must be skipped and knocks on them answered.
        state.leases.push(live);
        let board_read = read_board(
            why.trigger,
            &ReadPolicy {
                mode: self.settings.data_mode,
                epoch_change: why.epoch_change,
                ratio: self.settings.cover_read_ratio,
            },
            self.rng,
        );
        let server = self.env.server;
        let upload = LeaseUpload {
            lease: built.lease,
            revoke_key,
            read: board_read,
        };
        let Some(first_page) = self
            .port(WakeStep::Upload, server.upload_lease(upload))
            .await
        else {
            return Ok(UploadOutcome::Interrupted(why.trigger));
        };
        // The upload replaced every earlier lease.
        state.leases = vec![live];
        state.last_upload = Some(self.now);
        if let Some(page) = first_page {
            let matches = self.read_board(state, active, page).await?;
            self.knock(state, keys, &matches).await?;
        }
        Ok(UploadOutcome::Uploaded {
            trigger: why.trigger,
            real_records,
            read: board_read,
        })
    }

    /// Reads every page and trial-matches it against the active Spots. A read made for cover,
    /// or with no Spot Inside, is downloaded in full and discarded (design B.4.3).
    async fn read_board(
        &mut self,
        state: &mut WakeState,
        active: &[ActiveSpot],
        first: BoardPage,
    ) -> Result<Vec<BoardMatch>, PresenceError> {
        let trial = TrialMatcher::new(active, self.now.bucket());
        let own = state.own_records();
        let mut matches = Vec::new();
        let mut page = first;
        loop {
            if !active.is_empty() {
                match trial.match_page(&page, &own) {
                    Ok(found) => matches.extend(found),
                    // A page whose segment counts do not add up is the server's fault; it is
                    // skipped like an unreadable page.
                    Err(PresenceError::Proto(_)) => {}
                    Err(other) => return Err(other),
                }
            }
            let Some(cursor) = page.next.take() else {
                break;
            };
            let server = self.env.server;
            match self
                .port(WakeStep::BoardPages, server.board_page(cursor))
                .await
            {
                Some(next) => page = next,
                None => break,
            }
        }
        for found in &matches {
            state.saw_peer(found.spot, self.now);
            if !self.report.matched.contains(&found.spot) {
                self.report.matched.push(found.spot);
            }
        }
        Ok(matches)
    }

    /// Knocks up to three matches (design B.4.4 step 2), skipping mailboxes already knocked
    /// and records of Meetups this device is in.
    async fn knock(
        &mut self,
        state: &mut WakeState,
        keys: &Keys<'_>,
        matches: &[BoardMatch],
    ) -> Result<(), PresenceError> {
        let Keys { parties, active } = *keys;
        let already_knocked: Vec<MailboxId> = state
            .sent_knocks
            .iter()
            .map(|sent| sent.knock_mailbox)
            .collect();
        let own_meetups: Vec<GroupId> = state
            .spots
            .iter()
            .filter_map(|tracked| self.env.meetups.current_meetup(tracked.spot))
            .collect();
        let targets = knock_targets(
            matches,
            &KnockFilter {
                already_knocked: &already_knocked,
                own_meetups: &own_meetups,
            },
        );
        for target in targets {
            let keys = active
                .iter()
                .find(|keys| keys.spot() == target.spot && keys.epoch() == target.epoch);
            let party = parties
                .iter()
                .find(|party| party.party == target.spot.party);
            let (Some(keys), Some(party)) = (keys, party) else {
                continue;
            };
            let Some(reply_mailbox) = self
                .env
                .server
                .take_mailbox(MailboxPurpose::Reply, self.now.bucket())
            else {
                continue;
            };
            let Ok(key_package) = self.env.parties.key_package(party.party) else {
                continue;
            };
            let knock = build_knock(
                &KnockInputs {
                    matched: target,
                    spot: keys,
                    own_record: state.own_record_for(target.spot),
                    own_leaf: party.own_leaf,
                    key_package,
                    reply_mailbox,
                    meetup_subscription: None,
                    signing_key: self.env.parties.signing_key(),
                    class: party.knock_class,
                },
                self.rng,
            )?;
            // Remembered first: a knock that may have reached the server is not repeated.
            state.sent_knocks.push(SentKnock {
                spot: target.spot,
                epoch: target.epoch,
                knock_mailbox: knock.mailbox,
                matched: target.record.randomness,
                reply_mailbox,
                sent_at: self.now,
                status: KnockStatus::Waiting,
            });
            let server = self.env.server;
            if self
                .port(WakeStep::Knock, server.post_knock(knock))
                .await
                .is_none()
            {
                break;
            }
            self.report.knocks_sent = self.report.knocks_sent.saturating_add(1);
        }
        Ok(())
    }

    /// Reads the knock mailbox of every real record the server may hold, verifies what opens,
    /// re-validates with a fresh fix and the gate, and decides (design B.4.4 steps 4 to 6).
    /// Records whose Spot has no keys now are read like the others, but nothing opens.
    async fn answer_knocks(
        &mut self,
        state: &mut WakeState,
        keys: &Keys<'_>,
        held_at_start: &[LiveLease],
    ) {
        let mut leases = held_at_start.to_vec();
        for lease in &state.leases {
            if !leases.contains(lease) {
                leases.push(*lease);
            }
        }
        let received = self.receive_knocks(state, keys, &leases).await;
        if received.is_empty() {
            return;
        }
        self.decide_knocks(state, received).await;
    }

    /// Every new knock that opens and verifies under one of this device's records.
    async fn receive_knocks(
        &mut self,
        state: &WakeState,
        keys: &Keys<'_>,
        leases: &[LiveLease],
    ) -> Vec<ReceivedKnock> {
        let Keys { parties, active } = *keys;
        let mut received = Vec::new();
        let records: Vec<(Bucket, RecordRandomness, RealRecord)> = leases
            .iter()
            .flat_map(|lease| {
                lease
                    .real_records()
                    .map(move |(slot, real)| (lease.start, slot.randomness, real))
            })
            .collect();
        for (start, own_record, real) in records {
            let server = self.env.server;
            let Some(posts) = self
                .port(WakeStep::Mailboxes, server.read_mailbox(real.knock_mailbox))
                .await
            else {
                break;
            };
            let Some(keys) = active
                .iter()
                .find(|keys| keys.spot() == real.spot && keys.epoch() == real.epoch)
            else {
                continue;
            };
            let context = KnockContext {
                spot: keys,
                start,
                own_record,
            };
            let roster = PartyRoster {
                parties: self.env.parties,
                party: real.spot.party,
                current: current_epoch(parties, real.spot).unwrap_or(real.epoch),
            };
            for post in posts {
                let Ok(opened) = open_knock(&context, &post) else {
                    continue;
                };
                let Ok(knock) = verify_knock(&context, opened, &roster) else {
                    continue;
                };
                let decided = state
                    .decided_knocks
                    .iter()
                    .any(|decided| decided.reply_mailbox == knock.knock.reply_mailbox);
                if !decided {
                    received.push(ReceivedKnock {
                        real,
                        own_record,
                        knock,
                    });
                }
            }
        }
        received
    }

    /// The liveness check and the decision on each received knock (design B.4.4 steps 4 to 6).
    async fn decide_knocks(&mut self, state: &mut WakeState, received: Vec<ReceivedKnock>) {
        let (fresh, sharing) = self.liveness(state).await;
        let waiting: Vec<RecordRandomness> = state
            .sent_knocks
            .iter()
            .filter(|sent| sent.status == KnockStatus::Waiting)
            .map(|sent| sent.matched)
            .collect();
        for received in received {
            let spot = received.real.spot;
            let tracked = state.spot(spot);
            let spot_state = if tracked.is_some_and(TrackedSpot::inside) {
                PresenceState::Inside
            } else {
                PresenceState::Outside
            };
            let fresh_fix = match (fresh, tracked) {
                (Some(fix), Some(tracked)) if tracked.presence.fence().contains(fix.coordinate) => {
                    FreshFix::InsideSpot
                }
                (Some(_), _) => FreshFix::OutsideSpot,
                (None, _) => FreshFix::Unavailable,
            };
            let current_meetup = self.env.meetups.current_meetup(spot);
            let decision = decide_knock(
                &received.knock,
                &LivenessEvidence {
                    spot_state,
                    fresh_fix,
                    sharing,
                },
                &KnockSituation {
                    own_record: &received.own_record,
                    knocked_records: &waiting,
                    current_meetup,
                },
            );
            state.decided_knocks.push(DecidedKnock {
                reply_mailbox: received.knock.knock.reply_mailbox,
                at: self.now,
            });
            self.report
                .knock_decisions
                .push(KnockOutcome { spot, decision });
            if let KnockDecision::Accept(admission) = decision {
                state.saw_peer(spot, self.now);
                let meetups = self.env.meetups;
                let admission = MeetupAdmission {
                    spot,
                    epoch: received.real.epoch,
                    bucket: self.now.bucket(),
                    admission,
                    knock: received.knock,
                };
                self.port(WakeStep::Handoff, meetups.admit(admission)).await;
            }
        }
    }

    /// A fresh GPS fix and the gate read again, for the liveness check. The fix also moves
    /// presence and the exclusion trail.
    async fn liveness(&mut self, state: &mut WakeState) -> (Option<Fix>, Sharing) {
        let before = self.gate(state.trail.status_at(self.now));
        let fix = if before == GateDecision::Share {
            self.fix(
                WakeStep::LivenessFix,
                FixQuality::High,
                LIVENESS_FIX_TIMEOUT,
            )
            .await
        } else {
            None
        };
        let zones = self.observe_zones(state, fix);
        if let Some(fix) = fix {
            let changes = state.feed(None, PresenceInput::Fix(fix));
            self.report.transitions.extend(changes);
        }
        let sharing = if self.gate(zones) == GateDecision::Share {
            Sharing::Allowed
        } else {
            Sharing::Suppressed
        };
        (fix, sharing)
    }

    /// Reads the reply mailbox of every knock still waiting and hands Welcomes on.
    async fn read_replies(&mut self, state: &mut WakeState) {
        let waiting: Vec<SentKnock> = state
            .sent_knocks
            .iter()
            .filter(|sent| sent.status == KnockStatus::Waiting)
            .copied()
            .collect();
        for sent in waiting {
            let server = self.env.server;
            let Some(posts) = self
                .port(WakeStep::Mailboxes, server.read_mailbox(sent.reply_mailbox))
                .await
            else {
                break;
            };
            let mut handed = false;
            for post in posts {
                let meetups = self.env.meetups;
                let reply = ReplyPost {
                    spot: sent.spot,
                    epoch: sent.epoch,
                    post,
                };
                if self
                    .port(WakeStep::Handoff, meetups.welcome(reply))
                    .await
                    .is_some()
                {
                    handed = true;
                    self.report.welcomes = self.report.welcomes.saturating_add(1);
                }
            }
            if handed {
                state.saw_peer(sent.spot, self.now);
                for knock in &mut state.sent_knocks {
                    if knock.reply_mailbox == sent.reply_mailbox {
                        knock.status = KnockStatus::Answered;
                    }
                }
            }
        }
    }

    /// Knocks older than a lease are forgotten; those still waiting raise a notice per Spot.
    fn expire_knocks(&mut self, state: &mut WakeState) {
        let now = self.now;
        for sent in &state.sent_knocks {
            let notice = WakeNotice::UnansweredKnock(sent.spot);
            if sent.status == KnockStatus::Waiting
                && knock_unanswered(sent.sent_at, now)
                && !self.report.notices.contains(&notice)
            {
                self.report.notices.push(notice);
            }
        }
        state
            .sent_knocks
            .retain(|sent| !knock_unanswered(sent.sent_at, now));
    }

    /// Location mode and the next wake.
    fn finish(&self, state: &WakeState) -> Duration {
        if !self.any_meetup(state) {
            let mode = if state.spots.iter().any(TrackedSpot::inside) {
                LocationMode::Dwell
            } else {
                LocationMode::Idle
            };
            self.env.device.set_location_mode(mode);
        }
        let next = if self.ending.is_some() {
            RETRY_AFTER
        } else if awaiting_fix(state) {
            SpotPresence::ENTRY_FIX_SPACING
        } else {
            let since = state.last_upload.map_or(Duration::ZERO, |last| {
                self.now.saturating_duration_since(last)
            });
            pingers_presence::TARGET_UPLOAD_SPACING
                .saturating_sub(since)
                .max(RETRY_AFTER)
        };
        self.env.device.schedule_wake(next);
        next
    }
}

/// The Parties after the group-log sync, and what to do about the lease.
struct Synced {
    parties: Vec<PartyInfo>,
    epoch_changed: bool,
    upload_trigger: UploadTrigger,
    plan: UploadPlan,
    network_needed: bool,
    reads_due: bool,
}

/// What the upload trigger depends on besides the state.
#[derive(Clone, Copy)]
struct Facts {
    decision: GateDecision,
    epoch_changed: bool,
}

/// Why the lease is uploaded, for the upload and read decisions.
#[derive(Clone, Copy)]
struct UploadReason {
    trigger: UploadTrigger,
    epoch_change: EpochChange,
}

/// The Parties after the group-log sync and this wake's Spot keys.
#[derive(Clone, Copy)]
struct Keys<'keys> {
    parties: &'keys [PartyInfo],
    active: &'keys [ActiveSpot],
}

/// A Spot and the epoch a real record for it is sealed under.
struct RealKey {
    spot: SpotRef,
    epoch: Epoch,
}

/// The Party's current epoch.
fn current_epoch(parties: &[PartyInfo], spot: SpotRef) -> Option<Epoch> {
    parties
        .iter()
        .find(|party| party.party == spot.party)
        .map(|party| party.epoch.current)
}

/// What the client remembers of a built lease.
fn remember(
    start: Bucket,
    slots: &[pingers_presence::SlotRecord; pingers_proto::Lease::SLOTS],
    parties: &[PartyInfo],
) -> LiveLease {
    LiveLease {
        start,
        slots: slots.map(|slot| LeaseSlot {
            randomness: slot.randomness,
            real: match slot.content {
                SlotContent::Real {
                    spot,
                    knock_mailbox,
                } => current_epoch(parties, spot).map(|epoch| RealRecord {
                    spot,
                    epoch,
                    knock_mailbox,
                }),
                SlotContent::Dummy => None,
            },
        }),
    }
}

/// Whether a Spot is waiting for a fix: a Candidate needs its confirming fix, a restored
/// Inside its first confirmation.
fn awaiting_fix(state: &WakeState) -> bool {
    state.spots.iter().any(|tracked| {
        let presence = tracked.presence.state();
        presence == PresenceState::Candidate
            || (presence == PresenceState::Inside && !tracked.inside())
    })
}

/// Records each Party's epoch; returns whether any known Party moved to a new one.
fn note_epochs(state: &mut WakeState, parties: &[PartyInfo]) -> bool {
    let mut changed = false;
    let mut next = Vec::with_capacity(parties.len());
    for party in parties {
        let current = party.epoch.current;
        if state
            .epochs
            .iter()
            .any(|known| known.party == party.party && known.epoch != current)
        {
            changed = true;
        }
        next.push(KnownEpoch {
            party: party.party,
            epoch: current,
        });
    }
    state.epochs = next;
    changed
}

/// One Party's roster, through the Party view.
struct PartyRoster<'view, P> {
    parties: &'view P,
    party: PartyId,
    current: Epoch,
}

impl<P: PartyView> RosterLookup for PartyRoster<'_, P> {
    fn signature_key(&self, leaf: LeafIndex, epoch: Epoch) -> Option<AccountPublicKey> {
        self.parties.signature_key(self.party, leaf, epoch)
    }

    fn current_epoch(&self) -> Epoch {
        self.current
    }
}
