// SPDX-License-Identifier: AGPL-3.0-only
//! The per-Spot presence state machine (design F.2).

use core::time::Duration;

use pingers_time::UnixSeconds;

use crate::{Fence, Fix, GeoError, entry_accuracy_limit, exit_buffer};

/// Where the device stands with respect to one Spot (design F.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresenceState {
    /// Not in the Spot.
    Outside,
    /// Possibly in the Spot; waiting for a second confirming fix.
    Candidate,
    /// Confirmed in the Spot. Only here may the Spot's tag key exist (design B.4.3).
    Inside,
    /// Sharing is suppressed (Focus, pause, quiet hours, exclusion zone); on the wire this is
    /// the same as `Outside`.
    Suppressed,
}

/// Something that may move a Spot's state. Every input carries its own time.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum PresenceInput {
    /// A location fix.
    Fix(Fix),
    /// The OS reported entering the Spot's geofence.
    OsEnter {
        /// When.
        at: UnixSeconds,
    },
    /// The OS reported leaving the Spot's geofence.
    OsExit {
        /// When.
        at: UnixSeconds,
    },
    /// The OS reported dwelling in the Spot's geofence (Android `DWELL`), which confirms entry.
    OsDwell {
        /// When.
        at: UnixSeconds,
    },
    /// pingers-gate decided to suppress sharing.
    Suppress {
        /// When.
        at: UnixSeconds,
    },
    /// pingers-gate allows sharing again; the Spot restarts from `Outside`.
    Resume {
        /// When.
        at: UnixSeconds,
    },
    /// Time passed with no other input; applies the timeouts (3 min Candidate, 60 min Inside).
    Tick {
        /// Now.
        at: UnixSeconds,
    },
}

/// A state change, for the caller to act on (post a lease, derive or drop a tag key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceTransition {
    /// The state before.
    pub from: PresenceState,
    /// The state after.
    pub to: PresenceState,
    /// The time of the input that caused it.
    pub at: UnixSeconds,
}

/// The result of one input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum PresenceChange {
    /// State did not change.
    Unchanged,
    /// State changed.
    Changed(PresenceTransition),
}

/// What the client stores per Spot so the state machine survives a restart (design C.2,
/// `presence` table). Holds no coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceSnapshot {
    /// The state.
    pub state: PresenceState,
    /// When the state was entered.
    pub since: UnixSeconds,
    /// When the last fix was processed, if any.
    pub last_fix_at: Option<UnixSeconds>,
}

/// The F.2 state machine for one Spot.
///
/// | Transition | Condition |
/// |---|---|
/// | Outside → Candidate | OS enter event, or a fix inside the fence |
/// | Candidate → Inside | two fixes ≥ 30 s apart, both inside, accuracy ≤ `max(50 m, r/2)`; or OS dwell |
/// | Candidate → Outside | no confirming fix within 3 minutes; or an OS exit event |
/// | Inside → Outside | OS exit confirmed by one fix beyond the fence by `max(50 m, r/4)`; or two such fixes; or no confirming fix for 60 minutes |
/// | any → Suppressed | [`PresenceInput::Suppress`] |
/// | Suppressed → Outside | [`PresenceInput::Resume`] |
///
/// `r` is [`Fence::characteristic_radius`]. Details the table leaves open, decided here:
///
/// - **Timeouts** are checked before every input except `Suppress` and `Resume`. A timeout
///   reached exactly at the limit has expired ("within 3 minutes" means less than 3 minutes).
///   The Candidate timeout runs from entering Candidate; the Inside timeout from the last
///   confirming fix, OS dwell or entry.
/// - **Entry pair:** the first fix that is inside with good enough accuracy starts the pair;
///   later qualifying fixes under 30 s after it neither confirm nor restart it. A fix outside
///   the fence clears it, so the two confirming fixes are never separated by an outside fix.
///   An inside fix with poor accuracy changes nothing.
/// - **Confirming fix while Inside:** any fix not beyond the exit buffer. It also clears any
///   exit evidence. Exit fixes must be strictly beyond the buffer; their order with the OS
///   exit event does not matter.
/// - **OS dwell** confirms entry only for a circle, whose OS region is the fence itself. A
///   polygon's OS region is its inflated enclosing circle, so dwelling there does not show the
///   device is inside the polygon; for a polygon, dwell counts as an OS enter event.
/// - **While Suppressed** every input other than `Resume` is ignored, though fixes still
///   update [`PresenceSnapshot::last_fix_at`].
/// - **Net change:** one input can trigger a timeout and then a transition; the result
///   reports the overall change, and `Unchanged` when the state ends where it began (a
///   Candidate that timed out and restarted on the same fix).
#[derive(Clone, Debug, PartialEq)]
pub struct SpotPresence {
    fence: Fence,
    snapshot: PresenceSnapshot,
    progress: Progress,
}

/// What has been seen towards leaving `Inside`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitEvidence {
    None,
    OsExit,
    FixBeyond,
}

/// Progress within the current state that the snapshot does not hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Progress {
    /// Inputs earlier than this are stale.
    last_input_at: UnixSeconds,
    /// In Candidate: the first fix of a possible confirming pair.
    entry_fix_at: Option<UnixSeconds>,
    /// In Inside: when presence was last confirmed.
    confirmed_at: UnixSeconds,
    /// In Inside: evidence of leaving.
    exit_evidence: ExitEvidence,
}

impl Progress {
    const fn fresh(at: UnixSeconds) -> Self {
        Self {
            last_input_at: at,
            entry_fix_at: None,
            confirmed_at: at,
            exit_evidence: ExitEvidence::None,
        }
    }
}

impl PresenceInput {
    const fn at(self) -> UnixSeconds {
        match self {
            Self::Fix(fix) => fix.taken_at,
            Self::OsEnter { at }
            | Self::OsExit { at }
            | Self::OsDwell { at }
            | Self::Suppress { at }
            | Self::Resume { at }
            | Self::Tick { at } => at,
        }
    }
}

impl SpotPresence {
    /// Minimum spacing between the two fixes that confirm an entry.
    pub const ENTRY_FIX_SPACING: Duration = Duration::from_secs(30);

    /// How long a Candidate waits for a confirming fix.
    pub const CANDIDATE_TIMEOUT: Duration = Duration::from_mins(3);

    /// How long Inside lasts without a confirming fix.
    pub const INSIDE_TIMEOUT: Duration = Duration::from_hours(1);

    /// A new state machine for `fence`, starting `Outside` at `now`. Inputs earlier than
    /// `now` are stale.
    #[must_use]
    pub const fn new(fence: Fence, now: UnixSeconds) -> Self {
        let snapshot = PresenceSnapshot {
            state: PresenceState::Outside,
            since: now,
            last_fix_at: None,
        };
        Self {
            fence,
            snapshot,
            progress: Progress::fresh(now),
        }
    }

    /// Restores a state machine from a stored snapshot.
    ///
    /// What the snapshot does not hold restarts conservatively: a Candidate needs a fresh
    /// confirming pair, exit evidence is forgotten, and the Inside timeout and the stale-input
    /// cut-off run from the later of `since` and `last_fix_at`.
    #[must_use]
    pub fn restore(fence: Fence, snapshot: PresenceSnapshot) -> Self {
        let latest = snapshot
            .last_fix_at
            .map_or(snapshot.since, |last_fix| last_fix.max(snapshot.since));
        Self {
            fence,
            snapshot,
            progress: Progress::fresh(latest),
        }
    }

    /// The current state.
    #[must_use]
    pub const fn state(&self) -> PresenceState {
        self.snapshot.state
    }

    /// What to store.
    #[must_use]
    pub const fn snapshot(&self) -> PresenceSnapshot {
        self.snapshot
    }

    /// The fence this machine watches.
    #[must_use]
    pub const fn fence(&self) -> &Fence {
        &self.fence
    }

    /// Applies one input.
    ///
    /// # Errors
    ///
    /// [`GeoError::StaleInput`] if the input's time is earlier than the last processed input;
    /// the state is unchanged and the caller drops the input.
    pub fn handle(&mut self, input: PresenceInput) -> Result<PresenceChange, GeoError> {
        let at = input.at();
        if at < self.progress.last_input_at {
            return Err(GeoError::StaleInput);
        }
        let before = self.snapshot.state;
        self.progress.last_input_at = at;
        if let PresenceInput::Fix(_) = input {
            self.snapshot.last_fix_at = Some(at);
        }
        match input {
            PresenceInput::Suppress { .. } => {
                if before != PresenceState::Suppressed {
                    self.enter(PresenceState::Suppressed, at);
                }
            }
            PresenceInput::Resume { .. } => {
                if before == PresenceState::Suppressed {
                    self.enter(PresenceState::Outside, at);
                }
            }
            PresenceInput::Fix(_)
            | PresenceInput::OsEnter { .. }
            | PresenceInput::OsExit { .. }
            | PresenceInput::OsDwell { .. }
            | PresenceInput::Tick { .. } => {
                if before != PresenceState::Suppressed {
                    self.apply_timeouts(at);
                    self.apply(input, at);
                }
            }
        }
        let after = self.snapshot.state;
        Ok(if before == after {
            PresenceChange::Unchanged
        } else {
            PresenceChange::Changed(PresenceTransition {
                from: before,
                to: after,
                at,
            })
        })
    }

    fn enter(&mut self, state: PresenceState, at: UnixSeconds) {
        self.snapshot.state = state;
        self.snapshot.since = at;
        self.progress = Progress::fresh(at);
    }

    fn apply_timeouts(&mut self, at: UnixSeconds) {
        let expired = match self.snapshot.state {
            PresenceState::Candidate => {
                at.saturating_duration_since(self.snapshot.since) >= Self::CANDIDATE_TIMEOUT
            }
            PresenceState::Inside => {
                at.saturating_duration_since(self.progress.confirmed_at) >= Self::INSIDE_TIMEOUT
            }
            PresenceState::Outside | PresenceState::Suppressed => false,
        };
        if expired {
            self.enter(PresenceState::Outside, at);
        }
    }

    fn apply(&mut self, input: PresenceInput, at: UnixSeconds) {
        let state = self.snapshot.state;
        match input {
            PresenceInput::Fix(fix) => self.apply_fix(&fix),
            PresenceInput::OsEnter { .. } => match state {
                PresenceState::Outside => self.enter(PresenceState::Candidate, at),
                PresenceState::Inside => self.progress.exit_evidence = ExitEvidence::None,
                PresenceState::Candidate | PresenceState::Suppressed => {}
            },
            PresenceInput::OsExit { .. } => match state {
                PresenceState::Candidate => self.enter(PresenceState::Outside, at),
                PresenceState::Inside => self.record_exit_evidence(ExitEvidence::OsExit, at),
                PresenceState::Outside | PresenceState::Suppressed => {}
            },
            PresenceInput::OsDwell { .. } => self.apply_dwell(at),
            PresenceInput::Suppress { .. }
            | PresenceInput::Resume { .. }
            | PresenceInput::Tick { .. } => {}
        }
    }

    fn apply_dwell(&mut self, at: UnixSeconds) {
        let confirms_entry = matches!(self.fence, Fence::Circle(_));
        match self.snapshot.state {
            PresenceState::Outside | PresenceState::Candidate if confirms_entry => {
                self.enter(PresenceState::Inside, at);
            }
            PresenceState::Outside => self.enter(PresenceState::Candidate, at),
            PresenceState::Inside if confirms_entry => self.confirm(at),
            PresenceState::Candidate | PresenceState::Inside | PresenceState::Suppressed => {}
        }
    }

    fn apply_fix(&mut self, fix: &Fix) {
        let at = fix.taken_at;
        let radius = self.fence.characteristic_radius();
        match self.snapshot.state {
            PresenceState::Outside => {
                if self.fence.contains(fix.coordinate) {
                    self.enter(PresenceState::Candidate, at);
                    if fix.accuracy <= entry_accuracy_limit(radius) {
                        self.progress.entry_fix_at = Some(at);
                    }
                }
            }
            PresenceState::Candidate => {
                if !self.fence.contains(fix.coordinate) {
                    self.progress.entry_fix_at = None;
                } else if fix.accuracy <= entry_accuracy_limit(radius) {
                    match self.progress.entry_fix_at {
                        Some(first)
                            if at.saturating_duration_since(first) >= Self::ENTRY_FIX_SPACING =>
                        {
                            self.enter(PresenceState::Inside, at);
                        }
                        Some(_) => {}
                        None => self.progress.entry_fix_at = Some(at),
                    }
                }
            }
            PresenceState::Inside => {
                if self.fence.distance_outside(fix.coordinate) > exit_buffer(radius) {
                    self.record_exit_evidence(ExitEvidence::FixBeyond, at);
                } else {
                    self.confirm(at);
                }
            }
            PresenceState::Suppressed => {}
        }
    }

    /// In Inside: an exit needs an OS exit and a fix beyond, or two fixes beyond.
    fn record_exit_evidence(&mut self, evidence: ExitEvidence, at: UnixSeconds) {
        let confirmed = match self.progress.exit_evidence {
            ExitEvidence::None => false,
            ExitEvidence::OsExit => evidence == ExitEvidence::FixBeyond,
            ExitEvidence::FixBeyond => true,
        };
        if confirmed {
            self.enter(PresenceState::Outside, at);
        } else {
            self.progress.exit_evidence = evidence;
        }
    }

    fn confirm(&mut self, at: UnixSeconds) {
        self.progress.confirmed_at = at;
        self.progress.exit_evidence = ExitEvidence::None;
    }
}
