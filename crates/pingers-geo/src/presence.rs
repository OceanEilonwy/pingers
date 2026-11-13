// SPDX-License-Identifier: AGPL-3.0-only
//! The per-Spot presence state machine (design F.2).

use core::time::Duration;

use pingers_time::UnixSeconds;

use crate::stub::not_implemented;
use crate::{Fence, Fix, GeoError};

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
/// | Candidate → Outside | no confirming fix within 3 minutes |
/// | Inside → Outside | OS exit confirmed by one fix beyond the fence by `max(50 m, r/4)`; or two such fixes; or no confirming fix for 60 minutes |
/// | any → Suppressed | [`PresenceInput::Suppress`] |
#[derive(Clone, Debug, PartialEq)]
pub struct SpotPresence {
    fence: Fence,
    snapshot: PresenceSnapshot,
}

impl SpotPresence {
    /// Minimum spacing between the two fixes that confirm an entry.
    pub const ENTRY_FIX_SPACING: Duration = Duration::from_secs(30);

    /// How long a Candidate waits for a confirming fix.
    pub const CANDIDATE_TIMEOUT: Duration = Duration::from_mins(3);

    /// How long Inside lasts without a confirming fix.
    pub const INSIDE_TIMEOUT: Duration = Duration::from_hours(1);

    /// A new state machine for `fence`, starting `Outside` at `now`.
    #[must_use]
    pub const fn new(fence: Fence, now: UnixSeconds) -> Self {
        let snapshot = PresenceSnapshot {
            state: PresenceState::Outside,
            since: now,
            last_fix_at: None,
        };
        Self { fence, snapshot }
    }

    /// Restores a state machine from a stored snapshot.
    #[must_use]
    pub const fn restore(fence: Fence, snapshot: PresenceSnapshot) -> Self {
        Self { fence, snapshot }
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
        not_implemented((self, input))
    }
}
