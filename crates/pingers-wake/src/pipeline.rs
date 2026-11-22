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
    MeetupKey, PresenceError, ReadPolicy, RealSlot, RosterLookup, Sharing, SlotCandidate,
    SlotContent, SpotRef, SpotStatus, TrialMatcher, UploadPlan, UploadTrigger, VerifiedKnock,
    build_knock, build_lease, choose_slots, decide_knock, knock_targets, knock_unanswered,
    matching_epochs, open_knock, plan_upload, read_board, verify_knock,
};
use pingers_proto::{
    BoardPage, CoverReadRatio, Epoch, GroupId, LeafIndex, MailboxId, MeetupHint, PartyId,
    ReadBoard, RecordRandomness, ServerId,
};
use pingers_time::{Bucket, Clock, Timer, UnixSeconds};
use rand_core::CryptoRng;

use crate::budget::Budget;
use crate::ports::{
    DevicePort, FixQuality, LeaseUpload, LocationMode, MeetupAdmission, MeetupPort, OsRegion,
    PartyInfo, PartyView, RegionId, ReplyPost, ServerPort,
};
use crate::state::{
    Board, DecidedKnock, GeofenceAnchor, KnockStatus, KnownEpoch, LeaseSlot, LiveLease, RealRecord,
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
    /// A scheduled wake (WorkManager, iOS background refresh).
    Scheduled,
    /// The app came on screen (design B.4.9: the user opening the app). As a scheduled wake,
    /// but the Party logs and this device's mailboxes are read whatever the push setting, so
    /// a change whose push was missed, or never sent, shows once the app is opened.
    Opened,
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
    /// Leaving Meetups because sharing is suppressed, or because the device left their Spot.
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
    /// The one push registration of the wake: a stale seal renewed, or a mailbox for the pool.
    PushRegistration,
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

/// What one board round did: the lease uploaded to one server's board, and how the round's
/// network steps ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardReport {
    /// The server.
    pub server: ServerId,
    /// The lease on its board.
    pub upload: UploadOutcome,
    /// How the round ended. If the device part of the wake ended early (the routine fix ran
    /// out the budget, or leaving a Meetup failed), every round's network steps are skipped and
    /// this is that ending.
    pub ending: WakeEnding,
}

/// What one wake did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeReport {
    /// The gate's answer.
    pub decision: GateDecision,
    /// Spots that changed state.
    pub transitions: Vec<SpotTransition>,
    /// The lease of the first board round (in round order, see [`WakeReport::boards`]). With
    /// no round at all (no Party, and no board that still needs reading) nothing was due:
    /// [`UploadOutcome::NotDue`] with the time of the next wake.
    pub upload: UploadOutcome,
    /// One entry per board round, in the order they ran: the server whose last successful
    /// upload is oldest (never uploaded first, ties by server origin) runs first.
    pub boards: Vec<BoardReport>,
    /// Spots this device is in where a peer's record matched ("a member of the Party is
    /// here"). Never a Spot the device is not in.
    pub matched: Vec<SpotRef>,
    /// Knocks posted.
    pub knocks_sent: usize,
    /// Knocks received and decided.
    pub knock_decisions: Vec<KnockOutcome>,
    /// Knocks left undecided because the liveness fix was not to hand: it was asked for and
    /// comes in a wake of its own. Their mailboxes are kept, so the shell should read
    /// mailboxes in the wake that delivers that fix ([`MailboxReads::EveryWake`] for it).
    pub knocks_awaiting_fix: usize,
    /// Welcomes handed to pingers-meetup-session.
    pub welcomes: usize,
    /// Notices.
    pub notices: Vec<WakeNotice>,
    /// The delay given to `schedule_wake`.
    pub next_wake: Duration,
    /// How it ended, in summary: the device part's ending if it ended early, else the first
    /// board round's (in round order) that did not complete, else [`WakeEnding::Completed`].
    /// Each round's own ending is in [`WakeReport::boards`].
    pub ending: WakeEnding,
}

impl WakeState {
    /// Runs one wake: gate, presence and geofences once, then one board round per server
    /// (group logs, lease and board, knocks, mailboxes and liveness checks), then the location
    /// mode and the next wake (design E.2).
    ///
    /// Each round gets an equal share of the budget left when it starts. A network failure or
    /// a share running out ends only that round's network steps ([`BoardReport::ending`]); the
    /// state stays consistent and the next wake is scheduled.
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
        let budget = Budget::start(env.time, input.budget);
        let mut run = Run {
            env,
            rng,
            trigger: input.trigger,
            settings: &input.settings,
            budget: budget.whole(),
            wake_budget: budget,
            now: Clock::now(env.time),
            ending: None,
            liveness: None,
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
    knocks_awaiting_fix: usize,
    welcomes: usize,
    notices: Vec<WakeNotice>,
}

/// A knock that opened and verified under one of this device's records.
struct ReceivedKnock {
    real: RealRecord,
    own_record: RecordRandomness,
    knock: VerifiedKnock,
    /// `PSK_meet(b)` for the record it answers, `b` being the record's start bucket.
    key: MeetupKey,
}

/// The liveness evidence of this wake: one GPS fix and the gate read again, shared by every
/// round that has knocks to decide.
#[derive(Clone, Copy)]
struct Liveness {
    fix: Option<Fix>,
    sharing: Sharing,
}

struct Run<'run, 'env, D, P, S, M, T: Timer, R> {
    env: &'run WakeEnv<'env, D, P, S, M, T>,
    rng: &'run mut R,
    trigger: WakeTrigger,
    settings: &'run WakeSettings,
    /// The whole wake's budget.
    wake_budget: Budget<'env, T>,
    /// The budget of the current part: the whole budget for the device part, a share of it
    /// for each board round.
    budget: Budget<'env, T>,
    now: UnixSeconds,
    /// How the current part ended early; reset for each board round.
    ending: Option<WakeEnding>,
    liveness: Option<Liveness>,
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
        let mut parties = self.env.parties.parties();
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
        } else {
            self.leave_meetups_left_behind(state).await;
        }

        let device_ending = self.ending;
        let mut wake = WakeFacts {
            decision,
            reads_due: self.settings.mailbox_reads == MailboxReads::EveryWake
                || matches!(
                    trigger,
                    WakeTrigger::Opened
                        | WakeTrigger::Push(PushReason::Mailbox | PushReason::Unknown)
                ),
            commit_push: trigger == WakeTrigger::Push(PushReason::GroupCommit),
            new_epochs: note_epochs(state, &parties),
        };
        let servers = round_order(state, &parties);
        let mut boards = Vec::with_capacity(servers.len());
        let mut rounds_left = servers.len();
        for server in servers {
            // An equal share of what is left: time an earlier round does not use flows on.
            self.budget = self.wake_budget.share(rounds_left);
            rounds_left = rounds_left.saturating_sub(1);
            // A device part that ended early skips every round's network steps.
            self.ending = device_ending;
            let mut board = state.take_board(&server);
            let mut memory = Memory {
                device: state,
                board: &mut board,
            };
            let upload = self.round(&mut memory, &mut parties, &mut wake).await;
            // Given back on every path, so an error leaves no board behind.
            state.put_board(board);
            boards.push(BoardReport {
                server,
                upload: upload?,
                ending: self.ending.unwrap_or(WakeEnding::Completed),
            });
        }
        self.budget = self.wake_budget.whole();
        self.ending = device_ending;

        let in_use = servers_of(&parties);
        state.drop_idle_boards(&in_use);
        let ended_early = device_ending.is_some()
            || boards
                .iter()
                .any(|board| board.ending != WakeEnding::Completed);
        let next_wake = self.finish(state, &in_use, ended_early);
        let ending = device_ending
            .or_else(|| {
                boards
                    .iter()
                    .map(|board| board.ending)
                    .find(|ending| *ending != WakeEnding::Completed)
            })
            .unwrap_or(WakeEnding::Completed);
        let upload = boards.first().map_or_else(
            || UploadOutcome::NotDue(self.now.checked_add(next_wake).unwrap_or(self.now)),
            |board| board.upload,
        );
        Ok(WakeReport {
            decision,
            transitions: core::mem::take(&mut self.report.transitions),
            upload,
            boards,
            matched: core::mem::take(&mut self.report.matched),
            knocks_sent: self.report.knocks_sent,
            knock_decisions: core::mem::take(&mut self.report.knock_decisions),
            knocks_awaiting_fix: self.report.knocks_awaiting_fix,
            welcomes: self.report.welcomes,
            notices: core::mem::take(&mut self.report.notices),
            next_wake,
            ending,
        })
    }

    /// One server's board round: group logs, lease and board, knocks, mailboxes, mailbox pool.
    /// Only that server's Parties take part: their Spots get keys and real records, the rest of
    /// the lease is dummies, so each board sees one lease per device per bucket.
    async fn round(
        &mut self,
        memory: &mut Memory<'_>,
        parties: &mut Vec<PartyInfo>,
        wake: &mut WakeFacts,
    ) -> Result<UploadOutcome, WakeError> {
        let server = memory.board.server.clone();
        let Synced {
            plan:
                BoardPlan {
                    epoch_changed,
                    upload_trigger,
                    plan,
                },
            network_needed,
        } = self.sync_board(memory, parties, wake).await;
        let on_board: Vec<PartyInfo> = parties
            .iter()
            .filter(|party| party.server == server)
            .cloned()
            .collect();
        let active = self.derive_keys(memory.device, &on_board, wake.decision)?;
        let keys = Keys {
            server: &server,
            parties: &on_board,
            active: &active,
        };
        // Knocks answer the records the server held when the round began, including any the
        // upload is about to replace.
        let held_at_start = memory.board.leases.clone();
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
                self.upload(memory, &keys, why).await?
            }
        };

        if wake.reads_due {
            self.answer_knocks(memory, &keys, &held_at_start).await;
            self.read_replies(memory.device, memory.board, &keys).await;
        }
        if network_needed {
            let port = self.env.server;
            self.port(
                WakeStep::PushRegistration,
                port.maintain_push_registrations(&server),
            )
            .await;
        }
        Ok(upload)
    }

    /// Decides whether this board needs the network at all (an upload due, a Commit pushed, or
    /// mailboxes to read); if so pulls the server's group logs first (design B.4.7) and decides
    /// again with the new epochs. A quiet round makes no request.
    async fn sync_board(
        &mut self,
        memory: &mut Memory<'_>,
        parties: &mut Vec<PartyInfo>,
        wake: &mut WakeFacts,
    ) -> Synced {
        let server = memory.board.server.clone();
        let mut plan = self.plan_board(memory, parties, wake);
        // A Commit push always makes the upload due (`EpochChanged`) on a board with Parties.
        let network_needed = plan.plan == UploadPlan::UploadNow || wake.reads_due;
        let port = self.env.server;
        if network_needed
            && self
                .port(WakeStep::GroupLogs, port.pull_group_logs(&server))
                .await
                .is_some()
        {
            *parties = self.env.parties.parties();
            memory.device.reconcile(parties, self.now);
            wake.new_epochs.extend(note_epochs(memory.device, parties));
            plan = self.plan_board(memory, parties, wake);
        }
        Synced {
            plan,
            network_needed,
        }
    }

    /// Whether this board's Parties moved to a new epoch, why to upload to it, and whether
    /// the upload is due. A board with no Party left uploads only to withdraw a real record
    /// it still holds; it gets no cover leases.
    fn plan_board(
        &self,
        memory: &Memory<'_>,
        parties: &[PartyInfo],
        wake: &WakeFacts,
    ) -> BoardPlan {
        let board = &*memory.board;
        let on_board: Vec<PartyInfo> = parties
            .iter()
            .filter(|party| party.server == board.server)
            .cloned()
            .collect();
        let epoch_changed = !on_board.is_empty()
            && (wake.commit_push
                || on_board
                    .iter()
                    .any(|party| wake.new_epochs.contains(&party.party)));
        let facts = Facts {
            decision: wake.decision,
            epoch_changed,
        };
        let upload_trigger = self.upload_trigger(memory, &on_board, facts);
        let plan = if on_board.is_empty() && upload_trigger == UploadTrigger::Wake {
            UploadPlan::NotBefore(lease_end(board).unwrap_or(self.now))
        } else {
            plan_upload(upload_trigger, board.last_upload, self.now)
        };
        BoardPlan {
            epoch_changed,
            upload_trigger,
            plan,
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

    /// Leaves the Meetup at every Spot the device has confirmed it left (design B.5,
    /// "Leaving": exit confirmed). The last member to leave ends the Meetup.
    async fn leave_meetups_left_behind(&mut self, state: &WakeState) {
        let left: Vec<SpotRef> = state
            .spots
            .iter()
            .filter(|tracked| tracked.presence.state() == PresenceState::Outside)
            .map(|tracked| tracked.spot)
            .filter(|spot| self.env.meetups.current_meetup(*spot).is_some())
            .collect();
        for spot in left {
            let meetups = self.env.meetups;
            self.port(WakeStep::LeaveMeetups, meetups.leave(spot)).await;
        }
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

    /// Why to upload to one board, given the Parties on it (design B.4.5, B.4.7, B.10).
    fn upload_trigger(
        &self,
        memory: &Memory<'_>,
        parties: &[PartyInfo],
        facts: Facts,
    ) -> UploadTrigger {
        let Facts {
            decision,
            epoch_changed,
        } = facts;
        let board = &*memory.board;
        let posting = Self::posting(memory.device, parties, decision);
        // Only a confirmed Spot with sharing allowed can become Inside in this wake.
        let entered = self.report.transitions.iter().any(|change| {
            change.transition.to == PresenceState::Inside
                && parties.iter().any(|party| party.party == change.spot.party)
        });
        let stale_record = board.leases.iter().any(|lease| {
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
        memory: &Memory<'_>,
        keys: &Keys<'keys>,
        start: Bucket,
    ) -> Vec<RealSlot<'keys>> {
        let Keys {
            server,
            parties,
            active,
        } = *keys;
        let state = &*memory.device;
        let newest = memory.board.leases.last().copied();
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
            let Some(knock_mailbox) =
                self.env
                    .server
                    .take_mailbox(server, MailboxPurpose::Knock, start)
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
        memory: &mut Memory<'_>,
        keys: &Keys<'_>,
        why: UploadReason,
    ) -> Result<UploadOutcome, WakeError> {
        let Keys {
            server, parties, ..
        } = *keys;
        if self.ending.is_some() {
            return Ok(UploadOutcome::Interrupted(why.trigger));
        }
        let start = self.now.bucket();
        let real = self.real_slots(memory, keys, start);
        let real_records = real.len();
        // Every dummy slot gets a decoy knock mailbox from the same pool, read with the real
        // ones, so the mailbox reads per lease do not count the device's Spots.
        let decoys: Vec<MailboxId> = (real_records..pingers_proto::Lease::SLOTS)
            .map_while(|_| self.env.server.take_decoy_mailbox(server, start))
            .collect();
        let revoke_key = RevokeKey::generate(self.rng);
        let built = build_lease(
            &LeaseInputs {
                start,
                real,
                revoke_key: &revoke_key,
            },
            self.rng,
        )?;
        let live = remember(start, &built.slots, parties, &decoys);
        // Remembered before the upload: if the wake stops mid-request the server may still
        // hold it, so its records must be skipped and knocks on them answered.
        memory.board.leases.push(live);
        let board_read = read_board(
            why.trigger,
            &ReadPolicy {
                mode: self.settings.data_mode,
                epoch_change: why.epoch_change,
                ratio: self.settings.cover_read_ratio,
            },
            self.rng,
        );
        let port = self.env.server;
        let upload = LeaseUpload {
            lease: built.lease,
            revoke_key,
            read: board_read,
        };
        let Some(first_page) = self
            .port(WakeStep::Upload, port.upload_lease(server, upload))
            .await
        else {
            return Ok(UploadOutcome::Interrupted(why.trigger));
        };
        // The upload replaced every earlier lease on this board.
        memory.board.leases = vec![live];
        memory.board.last_upload = Some(self.now);
        if let Some(page) = first_page {
            let matches = self.read_board(memory, keys, page).await?;
            self.knock(memory, keys, &matches).await?;
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
        memory: &mut Memory<'_>,
        keys: &Keys<'_>,
        first: BoardPage,
    ) -> Result<Vec<BoardMatch>, PresenceError> {
        let Keys { server, active, .. } = *keys;
        let trial = TrialMatcher::new(active, self.now.bucket());
        let own = memory.board.own_records();
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
            let port = self.env.server;
            match self
                .port(WakeStep::BoardPages, port.board_page(server, cursor))
                .await
            {
                Some(next) => page = next,
                None => break,
            }
        }
        for found in &matches {
            memory.device.saw_peer(found.spot, self.now);
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
        memory: &mut Memory<'_>,
        keys: &Keys<'_>,
        matches: &[BoardMatch],
    ) -> Result<(), PresenceError> {
        let Keys {
            server,
            parties,
            active,
        } = *keys;
        let state = &*memory.device;
        let board = &mut *memory.board;
        let already_knocked: Vec<MailboxId> = board
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
            let Some(reply_mailbox) =
                self.env
                    .server
                    .take_mailbox(server, MailboxPurpose::Reply, self.now.bucket())
            else {
                continue;
            };
            let meetups = self.env.meetups;
            let Some(carried) = self
                .port(
                    WakeStep::Knock,
                    meetups.knock_key_package(party.party, reply_mailbox),
                )
                .await
            else {
                break;
            };
            let knock = build_knock(
                &KnockInputs {
                    matched: target,
                    spot: keys,
                    own_record: board.own_record_for(target.spot),
                    own_leaf: party.own_leaf,
                    key_package: carried.key_package,
                    reply_mailbox,
                    meetup_subscription: carried.subscription,
                    signing_key: self.env.parties.signing_key(),
                    class: party.knock_class,
                },
                self.rng,
            )?;
            // Remembered first: a knock that may have reached the server is not repeated.
            board.sent_knocks.push(SentKnock {
                spot: target.spot,
                epoch: target.epoch,
                knock_mailbox: knock.mailbox,
                matched: target.record.randomness,
                matched_start: target.start,
                reply_mailbox,
                sent_at: self.now,
                status: KnockStatus::Waiting,
            });
            let port = self.env.server;
            if self
                .port(WakeStep::Knock, port.post_knock(server, knock))
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
        memory: &mut Memory<'_>,
        keys: &Keys<'_>,
        held_at_start: &[LiveLease],
    ) {
        let mut leases = held_at_start.to_vec();
        for lease in &memory.board.leases {
            if !leases.contains(lease) {
                leases.push(*lease);
            }
        }
        let Read {
            received,
            mailboxes,
        } = self.receive_knocks(memory.board, keys, &leases).await;
        let awaiting_fix = if received.is_empty() {
            Vec::new()
        } else {
            self.decide_knocks(memory.device, memory.board, received)
                .await
        };
        // Decided (or not knocks at all): what was read can go, so it cannot fill the mailbox.
        // A knock awaiting its liveness fix stays, to be read again in the wake that brings it.
        for mailbox in mailboxes
            .into_iter()
            .filter(|mailbox| !awaiting_fix.contains(mailbox))
        {
            let port = self.env.server;
            if self
                .port(WakeStep::Mailboxes, port.delete_read(keys.server, mailbox))
                .await
                .is_none()
            {
                break;
            }
        }
    }

    /// Every new knock that opens and verifies under one of this device's records.
    async fn receive_knocks(
        &mut self,
        board: &Board,
        keys: &Keys<'_>,
        leases: &[LiveLease],
    ) -> Read {
        let Keys {
            server,
            parties,
            active,
        } = *keys;
        let mut received = Vec::new();
        let mut read_mailboxes = Vec::new();
        // Real records' knock mailboxes and the decoys of the dummy slots, in a random order:
        // the server sees the same number of reads whatever number of Spots has records.
        let mut reads: Vec<MailboxRead> = leases
            .iter()
            .flat_map(|lease| {
                lease
                    .real_records()
                    .map(move |(slot, real)| MailboxRead::Real {
                        start: lease.start,
                        own_record: slot.randomness,
                        real,
                    })
                    .chain(lease.decoys().map(MailboxRead::Decoy))
            })
            .collect();
        shuffle(&mut reads, self.rng);
        for read in reads {
            let mailbox = match read {
                MailboxRead::Real { real, .. } => real.knock_mailbox,
                MailboxRead::Decoy(mailbox) => mailbox,
            };
            let port = self.env.server;
            let Some(posts) = self
                .port(WakeStep::Mailboxes, port.read_mailbox(server, mailbox))
                .await
            else {
                break;
            };
            read_mailboxes.push(mailbox);
            let MailboxRead::Real {
                start,
                own_record,
                real,
            } = read
            else {
                // Nobody knocks on a decoy: whatever it holds is not read.
                continue;
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
                let decided = board
                    .decided_knocks
                    .iter()
                    .any(|decided| decided.reply_mailbox == knock.knock.reply_mailbox);
                if decided {
                    continue;
                }
                let Ok(key) = keys.meetup_key(start) else {
                    continue;
                };
                received.push(ReceivedKnock {
                    real,
                    own_record,
                    knock,
                    key,
                });
            }
        }
        Read {
            received,
            mailboxes: read_mailboxes,
        }
    }

    /// The liveness check and the decision on each received knock (design B.4.4 steps 4 to 6).
    ///
    /// Returns the knock mailboxes of knocks left undecided for want of a fix. A device that
    /// stands still in a Spot has usually taken no fix lately, so the liveness fix is asked
    /// for and arrives in a wake of its own, seconds later. Declining then would drop every
    /// knock on a device that is really there; instead the knock waits, unrecorded and in its
    /// mailbox, for the wake that brings the fix ([`WakeReport::knocks_awaiting_fix`]).
    async fn decide_knocks(
        &mut self,
        state: &mut WakeState,
        board: &mut Board,
        received: Vec<ReceivedKnock>,
    ) -> Vec<MailboxId> {
        let Liveness {
            fix: fresh,
            sharing,
        } = self.liveness(state).await;
        let mut awaiting_fix = Vec::new();
        let waiting: Vec<RecordRandomness> = board
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
            // Inside and allowed to share, with only the fix missing: it was asked for (the
            // gate allowed it), so wait for it rather than decline.
            if fresh.is_none() && sharing == Sharing::Allowed && spot_state == PresenceState::Inside
            {
                awaiting_fix.push(received.real.knock_mailbox);
                self.report.knocks_awaiting_fix = self.report.knocks_awaiting_fix.saturating_add(1);
                continue;
            }
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
            board.decided_knocks.push(DecidedKnock {
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
                    key: received.key,
                    admission,
                    knock: received.knock,
                };
                self.port(WakeStep::Handoff, meetups.admit(admission)).await;
            }
        }
        awaiting_fix
    }

    /// A fresh GPS fix and the gate read again, for the liveness check, taken once per wake
    /// and shared by every round with knocks to decide. The fix also moves presence and the
    /// exclusion trail.
    async fn liveness(&mut self, state: &mut WakeState) -> Liveness {
        if let Some(liveness) = self.liveness {
            return liveness;
        }
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
        let liveness = Liveness { fix, sharing };
        self.liveness = Some(liveness);
        liveness
    }

    /// Reads the reply mailbox of every knock still waiting and hands Welcomes on, with the
    /// `PSK_meet` of the record the knock answered. A device no longer Inside the Spot has no
    /// keys for it: it still reads the mailbox, like any other, but joins nothing.
    async fn read_replies(&mut self, state: &mut WakeState, board: &mut Board, keys: &Keys<'_>) {
        let waiting: Vec<SentKnock> = board
            .sent_knocks
            .iter()
            .filter(|sent| sent.status == KnockStatus::Waiting)
            .copied()
            .collect();
        for sent in waiting {
            let port = self.env.server;
            let Some(posts) = self
                .port(
                    WakeStep::Mailboxes,
                    port.read_mailbox(keys.server, sent.reply_mailbox),
                )
                .await
            else {
                break;
            };
            let spot_keys = keys
                .active
                .iter()
                .find(|active| active.spot() == sent.spot && active.epoch() == sent.epoch);
            let Some(spot_keys) = spot_keys else {
                continue;
            };
            let mut handed = false;
            for post in posts {
                let Ok(key) = spot_keys.meetup_key(sent.matched_start) else {
                    continue;
                };
                let meetups = self.env.meetups;
                let reply = ReplyPost {
                    key,
                    reply_mailbox: sent.reply_mailbox,
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
                for knock in &mut board.sent_knocks {
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
        for board in &mut state.boards {
            for sent in &board.sent_knocks {
                let notice = WakeNotice::UnansweredKnock(sent.spot);
                if sent.status == KnockStatus::Waiting
                    && knock_unanswered(sent.sent_at, now)
                    && !self.report.notices.contains(&notice)
                {
                    self.report.notices.push(notice);
                }
            }
            board
                .sent_knocks
                .retain(|sent| !knock_unanswered(sent.sent_at, now));
        }
    }

    /// Location mode and the next wake. The cover spacing runs from the oldest last upload
    /// among the boards with Parties, so every one of them gets its upload on time.
    fn finish(&self, state: &WakeState, in_use: &[ServerId], ended_early: bool) -> Duration {
        if !self.any_meetup(state) {
            let mode = if state.spots.iter().any(TrackedSpot::inside) {
                LocationMode::Dwell
            } else {
                LocationMode::Idle
            };
            self.env.device.set_location_mode(mode);
        }
        let next = if ended_early {
            RETRY_AFTER
        } else if awaiting_fix(state) {
            SpotPresence::ENTRY_FIX_SPACING
        } else {
            let since = state
                .boards
                .iter()
                .filter(|board| in_use.contains(&board.server))
                .map(|board| {
                    board.last_upload.map_or(Duration::ZERO, |last| {
                        self.now.saturating_duration_since(last)
                    })
                })
                .max()
                .unwrap_or(Duration::ZERO);
            pingers_presence::TARGET_UPLOAD_SPACING
                .saturating_sub(since)
                .max(RETRY_AFTER)
        };
        self.env.device.schedule_wake(next);
        next
    }
}

/// One board round's memory: the device-wide state, and the server's board taken out of it
/// for the round.
struct Memory<'round> {
    device: &'round mut WakeState,
    board: &'round mut Board,
}

/// What holds for every board round of a wake.
struct WakeFacts {
    decision: GateDecision,
    reads_due: bool,
    commit_push: bool,
    /// Parties seen moving to a new epoch in this wake, before or after a group-log pull.
    new_epochs: Vec<PartyId>,
}

/// What to do about one board's lease.
struct BoardPlan {
    epoch_changed: bool,
    upload_trigger: UploadTrigger,
    plan: UploadPlan,
}

/// The plan after the board's group-log sync, and whether the round uses the network.
struct Synced {
    plan: BoardPlan,
    network_needed: bool,
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

/// One board round's server, the Parties on it after its group-log sync, and their Spot keys.
#[derive(Clone, Copy)]
struct Keys<'keys> {
    server: &'keys ServerId,
    parties: &'keys [PartyInfo],
    active: &'keys [ActiveSpot],
}

/// Every server with a Party, once each, in the Parties' order.
fn servers_of(parties: &[PartyInfo]) -> Vec<ServerId> {
    let mut servers: Vec<ServerId> = Vec::new();
    for party in parties {
        if !servers.contains(&party.server) {
            servers.push(party.server.clone());
        }
    }
    servers
}

/// The servers that get a board round: every server with a Party, and every board that still
/// holds a lease or waits for a Welcome. The board whose last successful upload is oldest goes
/// first (never uploaded first, ties by server origin), so a server that keeps failing or
/// hanging at the front cannot starve the others across wakes.
fn round_order(state: &WakeState, parties: &[PartyInfo]) -> Vec<ServerId> {
    let mut servers = servers_of(parties);
    for board in &state.boards {
        if board.holds_something() && !servers.contains(&board.server) {
            servers.push(board.server.clone());
        }
    }
    let last_upload = |server: &ServerId| {
        state
            .boards
            .iter()
            .find(|board| board.server == *server)
            .and_then(|board| board.last_upload)
    };
    servers.sort_by(|left, right| {
        last_upload(left)
            .cmp(&last_upload(right))
            .then_with(|| left.as_str().cmp(right.as_str()))
    });
    servers
}

/// When the board's newest lease expires, if it holds one.
fn lease_end(board: &Board) -> Option<UnixSeconds> {
    board
        .leases
        .last()
        .and_then(|lease| lease.start.checked_add(pingers_presence::LEASE_BUCKETS))
        .map(Bucket::start)
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

/// What a round's knock mailbox reads brought, and which mailboxes were read.
struct Read {
    received: Vec<ReceivedKnock>,
    mailboxes: Vec<MailboxId>,
}

/// One knock mailbox to read: a real record's, or a dummy slot's decoy.
#[derive(Clone, Copy)]
enum MailboxRead {
    Real {
        start: Bucket,
        own_record: RecordRandomness,
        real: RealRecord,
    },
    Decoy(MailboxId),
}

/// Fisher-Yates, from `rng`.
fn shuffle<T>(items: &mut [T], rng: &mut impl CryptoRng) {
    for index in (1..items.len()).rev() {
        let bound = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
        // The modulo bias over 2^64 is negligible for a list of a few reads.
        let pick = usize::try_from(rng.next_u64().checked_rem(bound).unwrap_or(0)).unwrap_or(0);
        items.swap(index, pick);
    }
}

/// What the client remembers of a built lease, its dummy slots given `decoys` in order.
fn remember(
    start: Bucket,
    slots: &[pingers_presence::SlotRecord; pingers_proto::Lease::SLOTS],
    parties: &[PartyInfo],
    decoys: &[MailboxId],
) -> LiveLease {
    let mut decoys = decoys.iter().copied();
    LiveLease {
        start,
        slots: slots.map(|slot| match slot.content {
            SlotContent::Real {
                spot,
                knock_mailbox,
            } => {
                let real = current_epoch(parties, spot).map(|epoch| RealRecord {
                    spot,
                    epoch,
                    knock_mailbox,
                });
                LeaseSlot {
                    randomness: slot.randomness,
                    real,
                    // A record of a Party that is gone is still read, as a decoy.
                    decoy_mailbox: real.is_none().then_some(knock_mailbox),
                }
            }
            SlotContent::Dummy => LeaseSlot {
                randomness: slot.randomness,
                real: None,
                decoy_mailbox: decoys.next(),
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

/// Records each Party's epoch; returns the known Parties that moved to a new one.
fn note_epochs(state: &mut WakeState, parties: &[PartyInfo]) -> Vec<PartyId> {
    let mut changed = Vec::new();
    let mut next = Vec::with_capacity(parties.len());
    for party in parties {
        let current = party.epoch.current;
        if state
            .epochs
            .iter()
            .any(|known| known.party == party.party && known.epoch != current)
        {
            changed.push(party.party);
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
