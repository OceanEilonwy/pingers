// SPDX-License-Identifier: AGPL-3.0-only
//! The per-Spot presence state machine (design F.2).

use core::time::Duration;

use pingers_time::UnixSeconds;

use crate::{Fence, Fix, GeoError, Meters, entry_accuracy_limit, exit_buffer};

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
/// - **Confirming fix while Inside:** any fix not beyond the exit buffer. Only a fix inside
///   the fence also clears exit evidence; one in the band between the fence and the buffer
///   leaves a pending OS exit or fix beyond standing. Exit fixes must be strictly beyond the
///   buffer; their order with the OS exit event does not matter, and an OS enter event or
///   (for a circle) OS dwell clears the evidence too.
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
    pub const fn restore(fence: Fence, snapshot: PresenceSnapshot) -> Self {
        let latest = match snapshot.last_fix_at {
            Some(last_fix) if last_fix.get() > snapshot.since.get() => last_fix,
            Some(_) | None => snapshot.since,
        };
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
                let beyond = self.fence.distance_outside(fix.coordinate);
                if beyond > exit_buffer(radius) {
                    self.record_exit_evidence(ExitEvidence::FixBeyond, at);
                } else if beyond == Meters::ZERO {
                    self.confirm(at);
                } else {
                    // In the hysteresis band: still present, but no evidence of being back.
                    self.progress.confirmed_at = at;
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

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::testing::{offset, polygon_around};
    use crate::{Circle, Coordinate};

    /// A 200 m Spot: exit buffer 50 m, entry accuracy limit 100 m.
    const RADIUS: u32 = 200;
    const START: u64 = 10_000;

    fn center() -> Result<Coordinate, GeoError> {
        Coordinate::new(48.85, 2.35)
    }

    fn circle_fence() -> Result<Fence, GeoError> {
        Ok(Fence::Circle(Circle::new(
            center()?,
            Meters::from_whole(RADIUS),
        )?))
    }

    fn time(seconds_after_start: u64) -> UnixSeconds {
        UnixSeconds::new(START.saturating_add(seconds_after_start))
    }

    /// A fix `north` metres north of the centre, with `accuracy`.
    fn fix(north: f64, accuracy: u32, seconds: u64) -> Result<PresenceInput, GeoError> {
        Ok(PresenceInput::Fix(Fix {
            coordinate: offset(center()?, 0.0, north)?,
            accuracy: Meters::from_whole(accuracy),
            taken_at: time(seconds),
        }))
    }

    fn good_fix_inside(seconds: u64) -> Result<PresenceInput, GeoError> {
        fix(0.0, 10, seconds)
    }

    /// Beyond the fence by more than the 50 m exit buffer.
    fn fix_beyond(seconds: u64) -> Result<PresenceInput, GeoError> {
        fix(260.0, 10, seconds)
    }

    fn outside() -> Result<SpotPresence, GeoError> {
        Ok(SpotPresence::new(circle_fence()?, time(0)))
    }

    fn inside_at(seconds: u64) -> Result<SpotPresence, GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(seconds.saturating_sub(30))?)?;
        let _ = presence.handle(good_fix_inside(seconds)?)?;
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(presence)
    }

    fn changed(from: PresenceState, to: PresenceState, seconds: u64) -> PresenceChange {
        PresenceChange::Changed(PresenceTransition {
            from,
            to,
            at: time(seconds),
        })
    }

    #[test]
    fn starts_outside_with_no_fix() -> Result<(), GeoError> {
        let presence = outside()?;
        assert_eq!(
            presence.snapshot(),
            PresenceSnapshot {
                state: PresenceState::Outside,
                since: time(0),
                last_fix_at: None,
            }
        );
        assert_eq!(presence.fence(), &circle_fence()?);
        Ok(())
    }

    #[test]
    fn a_fix_inside_makes_a_candidate() -> Result<(), GeoError> {
        let mut presence = outside()?;
        assert_eq!(
            presence.handle(good_fix_inside(40)?)?,
            changed(PresenceState::Outside, PresenceState::Candidate, 40)
        );
        assert_eq!(presence.snapshot().since, time(40));
        assert_eq!(presence.snapshot().last_fix_at, Some(time(40)));
        Ok(())
    }

    #[test]
    fn a_fix_outside_leaves_outside_alone() -> Result<(), GeoError> {
        let mut presence = outside()?;
        assert_eq!(
            presence.handle(fix(201.0, 10, 5)?)?,
            PresenceChange::Unchanged
        );
        assert_eq!(presence.state(), PresenceState::Outside);
        Ok(())
    }

    #[test]
    fn os_enter_makes_a_candidate() -> Result<(), GeoError> {
        let mut presence = outside()?;
        assert_eq!(
            presence.handle(PresenceInput::OsEnter { at: time(5) })?,
            changed(PresenceState::Outside, PresenceState::Candidate, 5)
        );
        Ok(())
    }

    #[test]
    fn two_good_fixes_30_s_apart_confirm_entry() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(0)?)?;
        assert_eq!(
            presence.handle(good_fix_inside(30)?)?,
            changed(PresenceState::Candidate, PresenceState::Inside, 30)
        );
        assert_eq!(presence.snapshot().since, time(30));
        Ok(())
    }

    #[test]
    fn fixes_29_s_apart_do_not_confirm_entry() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(0)?)?;
        assert_eq!(
            presence.handle(good_fix_inside(29)?)?,
            PresenceChange::Unchanged
        );
        assert_eq!(presence.state(), PresenceState::Candidate);
        Ok(())
    }

    #[test]
    fn an_early_second_fix_does_not_restart_the_pair() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(0)?)?;
        let _ = presence.handle(good_fix_inside(20)?)?;
        // 30 s after the first fix, though only 10 s after the second.
        assert!(presence.handle(good_fix_inside(30)?)?.is_entry());
        Ok(())
    }

    #[test]
    fn a_candidate_from_the_os_needs_two_fixes() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(PresenceInput::OsEnter { at: time(0) })?;
        let _ = presence.handle(good_fix_inside(40)?)?;
        assert_eq!(presence.state(), PresenceState::Candidate);
        let _ = presence.handle(good_fix_inside(70)?)?;
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn entry_fixes_need_accuracy_within_half_the_radius() -> Result<(), GeoError> {
        let mut presence = outside()?;
        // 101 m is worse than max(50 m, 200 m / 2): it makes a candidate but does not count.
        let _ = presence.handle(fix(0.0, 101, 0)?)?;
        assert_eq!(presence.state(), PresenceState::Candidate);
        let _ = presence.handle(fix(0.0, 100, 40)?)?;
        assert_eq!(presence.state(), PresenceState::Candidate);
        let _ = presence.handle(fix(0.0, 101, 80)?)?;
        assert_eq!(presence.state(), PresenceState::Candidate);
        let _ = presence.handle(fix(0.0, 100, 70 + 30)?)?;
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn a_fix_outside_between_entry_fixes_breaks_the_pair() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(0)?)?;
        assert_eq!(
            presence.handle(fix(201.0, 10, 20)?)?,
            PresenceChange::Unchanged
        );
        assert_eq!(presence.state(), PresenceState::Candidate);
        let _ = presence.handle(good_fix_inside(40)?)?;
        assert_eq!(presence.state(), PresenceState::Candidate);
        let _ = presence.handle(good_fix_inside(70)?)?;
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn a_candidate_times_out_after_three_minutes() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(0)?)?;
        assert_eq!(
            presence.handle(PresenceInput::Tick { at: time(179) })?,
            PresenceChange::Unchanged
        );
        assert_eq!(
            presence.handle(PresenceInput::Tick { at: time(180) })?,
            changed(PresenceState::Candidate, PresenceState::Outside, 180)
        );
        Ok(())
    }

    #[test]
    fn a_confirming_fix_after_the_timeout_starts_a_new_candidate() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(0)?)?;
        // The old candidacy has expired, so this fix only starts a new one.
        assert_eq!(
            presence.handle(good_fix_inside(200)?)?,
            PresenceChange::Unchanged
        );
        assert_eq!(presence.state(), PresenceState::Candidate);
        assert_eq!(presence.snapshot().since, time(200));
        let _ = presence.handle(good_fix_inside(230)?)?;
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn an_expired_candidate_with_a_fix_outside_reports_leaving() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(0)?)?;
        assert_eq!(
            presence.handle(fix(201.0, 10, 200)?)?,
            changed(PresenceState::Candidate, PresenceState::Outside, 200)
        );
        Ok(())
    }

    #[test]
    fn os_exit_ends_a_candidate() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(0)?)?;
        assert_eq!(
            presence.handle(PresenceInput::OsExit { at: time(10) })?,
            changed(PresenceState::Candidate, PresenceState::Outside, 10)
        );
        Ok(())
    }

    #[test]
    fn os_dwell_confirms_a_circle() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(PresenceInput::OsEnter { at: time(0) })?;
        assert_eq!(
            presence.handle(PresenceInput::OsDwell { at: time(90) })?,
            changed(PresenceState::Candidate, PresenceState::Inside, 90)
        );
        let mut missed_enter = outside()?;
        assert_eq!(
            missed_enter.handle(PresenceInput::OsDwell { at: time(90) })?,
            changed(PresenceState::Outside, PresenceState::Inside, 90)
        );
        Ok(())
    }

    #[test]
    fn os_dwell_in_a_polygon_region_only_makes_a_candidate() -> Result<(), GeoError> {
        let square = polygon_around(
            center()?,
            &[[0.0, 0.0], [300.0, 0.0], [300.0, 300.0], [0.0, 300.0]],
        )?;
        let mut presence = SpotPresence::new(Fence::Polygon(square), time(0));
        assert_eq!(
            presence.handle(PresenceInput::OsDwell { at: time(90) })?,
            changed(PresenceState::Outside, PresenceState::Candidate, 90)
        );
        assert_eq!(
            presence.handle(PresenceInput::OsDwell { at: time(100) })?,
            PresenceChange::Unchanged
        );
        Ok(())
    }

    #[test]
    fn os_events_that_add_nothing_change_nothing() -> Result<(), GeoError> {
        let mut presence = outside()?;
        assert_eq!(
            presence.handle(PresenceInput::OsExit { at: time(1) })?,
            PresenceChange::Unchanged
        );
        let _ = presence.handle(PresenceInput::OsEnter { at: time(2) })?;
        assert_eq!(
            presence.handle(PresenceInput::OsEnter { at: time(3) })?,
            PresenceChange::Unchanged
        );
        assert_eq!(presence.snapshot().since, time(2));
        Ok(())
    }

    #[test]
    fn os_exit_then_a_fix_beyond_the_buffer_confirms_exit() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        assert_eq!(
            presence.handle(PresenceInput::OsExit { at: time(100) })?,
            PresenceChange::Unchanged
        );
        assert_eq!(
            presence.handle(fix_beyond(110)?)?,
            changed(PresenceState::Inside, PresenceState::Outside, 110)
        );
        Ok(())
    }

    #[test]
    fn a_fix_beyond_then_os_exit_confirms_exit() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let _ = presence.handle(fix_beyond(100)?)?;
        assert_eq!(
            presence.handle(PresenceInput::OsExit { at: time(110) })?,
            changed(PresenceState::Inside, PresenceState::Outside, 110)
        );
        Ok(())
    }

    #[test]
    fn two_fixes_beyond_the_buffer_confirm_exit() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        assert_eq!(
            presence.handle(fix_beyond(100)?)?,
            PresenceChange::Unchanged
        );
        assert_eq!(
            presence.handle(fix_beyond(110)?)?,
            changed(PresenceState::Inside, PresenceState::Outside, 110)
        );
        Ok(())
    }

    #[test]
    fn two_os_exits_alone_do_not_confirm_exit() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let _ = presence.handle(PresenceInput::OsExit { at: time(100) })?;
        let _ = presence.handle(PresenceInput::OsExit { at: time(110) })?;
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn exit_needs_buffer_beyond_fence() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let _ = presence.handle(PresenceInput::OsExit { at: time(100) })?;
        // 249 m from the centre is 49 m beyond a 200 m fence: inside the 50 m buffer.
        assert_eq!(
            presence.handle(fix(249.0, 10, 110)?)?,
            PresenceChange::Unchanged
        );
        assert_eq!(
            presence.handle(fix(249.0, 10, 120)?)?,
            PresenceChange::Unchanged
        );
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn a_fix_in_the_buffer_band_leaves_an_os_exit_pending() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let _ = presence.handle(PresenceInput::OsExit { at: time(100) })?;
        let _ = presence.handle(fix(249.0, 10, 110)?)?;
        // 251 m is 51 m beyond: with the OS exit still standing, this confirms the exit.
        assert_eq!(
            presence.handle(fix(251.0, 10, 120)?)?,
            changed(PresenceState::Inside, PresenceState::Outside, 120)
        );
        Ok(())
    }

    #[test]
    fn a_fix_back_inside_cancels_exit_evidence() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let _ = presence.handle(fix_beyond(100)?)?;
        let _ = presence.handle(good_fix_inside(110)?)?;
        assert_eq!(
            presence.handle(fix_beyond(120)?)?,
            PresenceChange::Unchanged
        );
        let _ = presence.handle(PresenceInput::OsExit { at: time(130) })?;
        assert_eq!(presence.state(), PresenceState::Outside);
        Ok(())
    }

    #[test]
    fn os_enter_cancels_an_os_exit() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let _ = presence.handle(PresenceInput::OsExit { at: time(100) })?;
        let _ = presence.handle(PresenceInput::OsEnter { at: time(105) })?;
        assert_eq!(
            presence.handle(fix_beyond(110)?)?,
            PresenceChange::Unchanged
        );
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn os_dwell_cancels_exit_evidence_for_a_circle() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let _ = presence.handle(fix_beyond(100)?)?;
        let _ = presence.handle(PresenceInput::OsDwell { at: time(105) })?;
        assert_eq!(
            presence.handle(fix_beyond(110)?)?,
            PresenceChange::Unchanged
        );
        Ok(())
    }

    #[test]
    fn inside_times_out_after_an_hour_without_confirmation() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let hour = SpotPresence::INSIDE_TIMEOUT.as_secs();
        assert_eq!(
            presence.handle(PresenceInput::Tick {
                at: time(30 + hour - 1)
            })?,
            PresenceChange::Unchanged
        );
        assert_eq!(
            presence.handle(PresenceInput::Tick {
                at: time(30 + hour)
            })?,
            changed(PresenceState::Inside, PresenceState::Outside, 30 + hour)
        );
        Ok(())
    }

    #[test]
    fn a_fix_within_the_buffer_keeps_inside_alive() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let hour = SpotPresence::INSIDE_TIMEOUT.as_secs();
        let _ = presence.handle(fix(240.0, 10, 1_000)?)?;
        let _ = presence.handle(PresenceInput::Tick {
            at: time(30 + hour),
        })?;
        assert_eq!(presence.state(), PresenceState::Inside);
        let _ = presence.handle(PresenceInput::Tick {
            at: time(1_000 + hour),
        })?;
        assert_eq!(presence.state(), PresenceState::Outside);
        Ok(())
    }

    #[test]
    fn a_fix_exactly_at_the_buffer_is_not_beyond_it() -> Result<(), GeoError> {
        // Size the fence to the fix rather than the other way round: with the fix about 249 m
        // out, `d - 50` and `d - r` are both exact in floating point (Sterbenz), so the fix
        // is exactly 50 m beyond a fence of radius `d - 50` (about 199 m, buffer 50 m).
        let coordinate = offset(center()?, 0.0, 249.0)?;
        let reach = crate::distance(center()?, coordinate).get();
        let fence = Fence::Circle(Circle::new(center()?, Meters::new(reach - 50.0)?)?);
        let buffer = Meters::from_whole(50);
        assert_eq!(fence.distance_outside(coordinate), buffer);
        assert_eq!(exit_buffer(fence.characteristic_radius()), buffer);
        let mut presence = SpotPresence::new(fence, time(0));
        let _ = presence.handle(PresenceInput::OsDwell { at: time(30) })?;
        let _ = presence.handle(PresenceInput::OsExit { at: time(100) })?;
        let at_buffer = PresenceInput::Fix(Fix {
            coordinate,
            accuracy: Meters::from_whole(10),
            taken_at: time(110),
        });
        assert_eq!(presence.handle(at_buffer)?, PresenceChange::Unchanged);
        Ok(())
    }

    #[test]
    fn os_dwell_neither_confirms_nor_clears_evidence_for_a_polygon() -> Result<(), GeoError> {
        // A 400 m square centred on the Spot centre: r = 283 m, exit buffer 71 m.
        let square = polygon_around(
            center()?,
            &[
                [-200.0, -200.0],
                [200.0, -200.0],
                [200.0, 200.0],
                [-200.0, 200.0],
            ],
        )?;
        let mut presence = SpotPresence::new(Fence::Polygon(square), time(0));
        let _ = presence.handle(good_fix_inside(0)?)?;
        let _ = presence.handle(good_fix_inside(30)?)?;
        assert_eq!(presence.state(), PresenceState::Inside);
        let _ = presence.handle(fix(300.0, 10, 100)?)?;
        let _ = presence.handle(PresenceInput::OsDwell { at: time(110) })?;
        assert_eq!(
            presence.handle(fix(300.0, 10, 120)?)?,
            changed(PresenceState::Inside, PresenceState::Outside, 120)
        );
        Ok(())
    }

    #[test]
    fn os_dwell_keeps_a_circle_inside_alive() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let hour = SpotPresence::INSIDE_TIMEOUT.as_secs();
        let _ = presence.handle(PresenceInput::OsDwell { at: time(1_000) })?;
        let _ = presence.handle(PresenceInput::Tick {
            at: time(30 + hour),
        })?;
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn a_fix_beyond_does_not_keep_inside_alive() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let hour = SpotPresence::INSIDE_TIMEOUT.as_secs();
        let _ = presence.handle(fix_beyond(1_000)?)?;
        let _ = presence.handle(PresenceInput::Tick {
            at: time(30 + hour),
        })?;
        assert_eq!(presence.state(), PresenceState::Outside);
        Ok(())
    }

    #[test]
    fn an_expired_inside_with_a_fix_inside_becomes_a_candidate() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        let late = 30 + SpotPresence::INSIDE_TIMEOUT.as_secs();
        assert_eq!(
            presence.handle(good_fix_inside(late)?)?,
            changed(PresenceState::Inside, PresenceState::Candidate, late)
        );
        Ok(())
    }

    #[test]
    fn suppress_from_any_state_and_resume_to_outside() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        assert_eq!(
            presence.handle(PresenceInput::Suppress { at: time(40) })?,
            changed(PresenceState::Inside, PresenceState::Suppressed, 40)
        );
        assert_eq!(
            presence.handle(PresenceInput::Suppress { at: time(50) })?,
            PresenceChange::Unchanged
        );
        assert_eq!(presence.snapshot().since, time(40));
        assert_eq!(
            presence.handle(PresenceInput::Resume { at: time(60) })?,
            changed(PresenceState::Suppressed, PresenceState::Outside, 60)
        );
        Ok(())
    }

    #[test]
    fn resume_when_not_suppressed_changes_nothing() -> Result<(), GeoError> {
        let mut presence = inside_at(30)?;
        assert_eq!(
            presence.handle(PresenceInput::Resume { at: time(40) })?,
            PresenceChange::Unchanged
        );
        assert_eq!(presence.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn suppressed_ignores_everything_but_resume() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(PresenceInput::Suppress { at: time(0) })?;
        for input in [
            good_fix_inside(10)?,
            good_fix_inside(50)?,
            PresenceInput::OsEnter { at: time(60) },
            PresenceInput::OsDwell { at: time(70) },
            PresenceInput::OsExit { at: time(80) },
            PresenceInput::Tick { at: time(10_000) },
        ] {
            assert_eq!(presence.handle(input)?, PresenceChange::Unchanged);
        }
        assert_eq!(presence.state(), PresenceState::Suppressed);
        assert_eq!(presence.snapshot().last_fix_at, Some(time(50)));
        Ok(())
    }

    #[test]
    fn stale_inputs_are_rejected_and_change_nothing() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(100)?)?;
        let before = presence.clone();
        assert_eq!(
            presence.handle(good_fix_inside(99)?),
            Err(GeoError::StaleInput)
        );
        assert_eq!(
            presence.handle(PresenceInput::Suppress { at: time(99) }),
            Err(GeoError::StaleInput)
        );
        assert_eq!(presence, before);
        // An input at the same second is not stale.
        assert!(
            presence
                .handle(PresenceInput::Tick { at: time(100) })
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn inputs_before_creation_are_stale() -> Result<(), GeoError> {
        let mut presence = SpotPresence::new(circle_fence()?, time(100));
        assert_eq!(
            presence.handle(PresenceInput::OsEnter { at: time(99) }),
            Err(GeoError::StaleInput)
        );
        Ok(())
    }

    #[test]
    fn restore_round_trips_the_snapshot() -> Result<(), GeoError> {
        let presence = inside_at(30)?;
        let restored = SpotPresence::restore(circle_fence()?, presence.snapshot());
        assert_eq!(restored.snapshot(), presence.snapshot());
        assert_eq!(restored.state(), PresenceState::Inside);
        Ok(())
    }

    #[test]
    fn restore_rejects_inputs_older_than_the_last_fix() -> Result<(), GeoError> {
        let snapshot = PresenceSnapshot {
            state: PresenceState::Inside,
            since: time(30),
            last_fix_at: Some(time(500)),
        };
        let mut restored = SpotPresence::restore(circle_fence()?, snapshot);
        assert_eq!(
            restored.handle(PresenceInput::Tick { at: time(499) }),
            Err(GeoError::StaleInput)
        );
        // The Inside timeout runs from the last fix.
        let hour = SpotPresence::INSIDE_TIMEOUT.as_secs();
        let _ = restored.handle(PresenceInput::Tick {
            at: time(500 + hour - 1),
        })?;
        assert_eq!(restored.state(), PresenceState::Inside);
        let _ = restored.handle(PresenceInput::Tick {
            at: time(500 + hour),
        })?;
        assert_eq!(restored.state(), PresenceState::Outside);
        Ok(())
    }

    #[test]
    fn restore_without_a_fix_runs_from_since() -> Result<(), GeoError> {
        let snapshot = PresenceSnapshot {
            state: PresenceState::Candidate,
            since: time(500),
            last_fix_at: None,
        };
        let mut restored = SpotPresence::restore(circle_fence()?, snapshot);
        assert_eq!(
            restored.handle(PresenceInput::Tick { at: time(499) }),
            Err(GeoError::StaleInput)
        );
        Ok(())
    }

    #[test]
    fn restore_ignores_a_fix_older_than_since() -> Result<(), GeoError> {
        let snapshot = PresenceSnapshot {
            state: PresenceState::Outside,
            since: time(500),
            last_fix_at: Some(time(100)),
        };
        let mut restored = SpotPresence::restore(circle_fence()?, snapshot);
        assert_eq!(
            restored.handle(PresenceInput::Tick { at: time(499) }),
            Err(GeoError::StaleInput)
        );
        Ok(())
    }

    #[test]
    fn a_restored_candidate_needs_a_fresh_pair() -> Result<(), GeoError> {
        let mut presence = outside()?;
        let _ = presence.handle(good_fix_inside(0)?)?;
        let mut restored = SpotPresence::restore(circle_fence()?, presence.snapshot());
        let _ = restored.handle(good_fix_inside(40)?)?;
        assert_eq!(restored.state(), PresenceState::Candidate);
        let _ = restored.handle(good_fix_inside(70)?)?;
        assert_eq!(restored.state(), PresenceState::Inside);
        Ok(())
    }

    impl PresenceChange {
        fn is_entry(self) -> bool {
            matches!(
                self,
                Self::Changed(PresenceTransition {
                    to: PresenceState::Inside,
                    ..
                })
            )
        }
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;

        /// A fix input at `north` metres from the centre, `seconds` after the start.
        fn fix_at(north: f64, accuracy: u32, seconds: u64) -> Result<PresenceInput, GeoError> {
            fix(north, accuracy, seconds)
        }

        #[derive(Clone, Copy, Debug)]
        enum Step {
            Fix { north: f64, accuracy: u32 },
            OsEnter,
            OsExit,
            OsDwell,
            Suppress,
            Resume,
            Tick,
        }

        fn step() -> impl Strategy<Value = Step> {
            prop_oneof![
                4 => (-400.0..400.0_f64, 1_u32..200).prop_map(|(north, accuracy)| Step::Fix { north, accuracy }),
                1 => Just(Step::OsEnter),
                1 => Just(Step::OsExit),
                1 => Just(Step::OsDwell),
                1 => Just(Step::Suppress),
                1 => Just(Step::Resume),
                1 => Just(Step::Tick),
            ]
        }

        fn input(step: Step, seconds: u64) -> Result<PresenceInput, GeoError> {
            let at = time(seconds);
            Ok(match step {
                Step::Fix { north, accuracy } => fix_at(north, accuracy, seconds)?,
                Step::OsEnter => PresenceInput::OsEnter { at },
                Step::OsExit => PresenceInput::OsExit { at },
                Step::OsDwell => PresenceInput::OsDwell { at },
                Step::Suppress => PresenceInput::Suppress { at },
                Step::Resume => PresenceInput::Resume { at },
                Step::Tick => PresenceInput::Tick { at },
            })
        }

        /// Steps with gaps in seconds, up to just over the Candidate timeout.
        fn script() -> impl Strategy<Value = Vec<(Step, u64)>> {
            prop::collection::vec((step(), 0_u64..200), 1..40)
        }

        proptest! {
            #[test]
            fn every_transition_is_one_the_table_allows(script in script()) {
                let mut presence = outside()?;
                let mut seconds = 0_u64;
                for (step, gap) in script {
                    seconds = seconds.saturating_add(gap);
                    let change = presence.handle(input(step, seconds)?)?;
                    if let PresenceChange::Changed(transition) = change {
                        let allowed = match (transition.from, transition.to) {
                            (PresenceState::Suppressed, to) => {
                                matches!(step, Step::Resume) && to == PresenceState::Outside
                            }
                            (_, PresenceState::Suppressed) => matches!(step, Step::Suppress),
                            (PresenceState::Outside, PresenceState::Inside) => {
                                matches!(step, Step::OsDwell)
                            }
                            (PresenceState::Outside | PresenceState::Inside, PresenceState::Candidate)
                            | (PresenceState::Candidate | PresenceState::Inside, PresenceState::Outside)
                            | (PresenceState::Candidate, PresenceState::Inside) => true,
                            (from, to) => from == to,
                        };
                        prop_assert!(allowed, "{transition:?} after {step:?}");
                        prop_assert_ne!(transition.from, transition.to);
                        prop_assert_eq!(transition.at, time(seconds));
                    }
                }
            }

            #[test]
            fn without_os_dwell_entry_needs_two_good_fixes_30_s_apart(script in script()) {
                let mut presence = outside()?;
                let mut seconds = 0_u64;
                let mut good_inside_fixes: Vec<u64> = Vec::new();
                for (step, gap) in script {
                    if matches!(step, Step::OsDwell) {
                        continue;
                    }
                    seconds = seconds.saturating_add(gap);
                    if let Step::Fix { north, accuracy } = step
                        && north.abs() <= 200.5
                        && accuracy <= 100
                    {
                        good_inside_fixes.push(seconds);
                    }
                    let change = presence.handle(input(step, seconds)?)?;
                    if let PresenceChange::Changed(PresenceTransition { to: PresenceState::Inside, .. }) = change {
                        let earliest = good_inside_fixes.first().copied().unwrap_or(seconds);
                        prop_assert!(seconds.saturating_sub(earliest) >= 30);
                        let by_fix = matches!(step, Step::Fix { .. });
                        prop_assert!(by_fix);
                    }
                }
            }

            #[test]
            fn wandering_inside_the_exit_buffer_never_leaves(
                norths in prop::collection::vec((-249.0..249.0_f64, 0_u64..3_000), 1..40),
            ) {
                let mut presence = inside_at(30)?;
                let mut seconds = 30_u64;
                for (north, gap) in norths {
                    seconds = seconds.saturating_add(gap);
                    let _ = presence.handle(PresenceInput::OsExit { at: time(seconds) })?;
                    let _ = presence.handle(fix_at(north, 150, seconds)?)?;
                    prop_assert_eq!(presence.state(), PresenceState::Inside);
                }
            }

            #[test]
            fn fixes_outside_the_fence_never_enter(
                norths in prop::collection::vec((201.0..5_000.0_f64, 0_u64..300), 1..40),
                south in any::<bool>(),
            ) {
                let mut presence = outside()?;
                let mut seconds = 0_u64;
                for (north, gap) in norths {
                    seconds = seconds.saturating_add(gap);
                    let signed = if south { -north } else { north };
                    let _ = presence.handle(fix_at(signed, 5, seconds)?)?;
                    prop_assert_eq!(presence.state(), PresenceState::Outside);
                }
            }

            #[test]
            fn a_burst_of_fixes_under_30_s_never_enters(
                offsets in prop::collection::vec(0_u64..30, 1..20),
            ) {
                let mut presence = outside()?;
                let mut sorted = offsets;
                sorted.sort_unstable();
                for seconds in sorted {
                    let _ = presence.handle(good_fix_inside(seconds)?)?;
                    prop_assert_ne!(presence.state(), PresenceState::Inside);
                }
            }

            #[test]
            fn a_stale_input_is_an_error_and_changes_nothing(
                script in script(),
                step in step(),
                back in 1_u64..1_000,
            ) {
                let mut presence = outside()?;
                let mut seconds = 1_000_u64;
                for (scripted, gap) in script {
                    seconds = seconds.saturating_add(gap);
                    let _ = presence.handle(input(scripted, seconds)?)?;
                }
                let before = presence.clone();
                prop_assert_eq!(
                    presence.handle(input(step, seconds.saturating_sub(back))?),
                    Err(GeoError::StaleInput)
                );
                prop_assert_eq!(presence, before);
            }

            #[test]
            fn restoring_a_snapshot_keeps_state_and_snapshot(script in script()) {
                let mut presence = outside()?;
                let mut seconds = 0_u64;
                for (step, gap) in script {
                    seconds = seconds.saturating_add(gap);
                    let _ = presence.handle(input(step, seconds)?)?;
                    let restored = SpotPresence::restore(circle_fence()?, presence.snapshot());
                    prop_assert_eq!(restored.snapshot(), presence.snapshot());
                }
            }
        }
    }
}
