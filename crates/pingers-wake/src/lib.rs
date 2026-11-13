// SPDX-License-Identifier: AGPL-3.0-only
//! The background pipeline: one wake, start to finish.
//!
//! Entry points ([`WakeTrigger`]): a fix, an OS geofence transition, a scheduled wake, a push,
//! a Focus change. [`WakeState::run`] runs the fixed sequence of design E.2:
//!
//! 1. ask pingers-gate (Focus read fresh, exclusion zones and their trail first);
//! 2. update Spot presence (pingers-geo), and choose the OS geofences again if the device moved;
//! 3. pull the group logs, then build and upload the lease, read the board and trial-match
//!    (pingers-presence), only against Spots that are confirmed `Inside`;
//! 4. knock on matches, and answer knocks with a liveness check;
//! 5. hand accepted knocks and Welcomes to pingers-meetup-session;
//! 6. refill the mailbox pool, set the location mode and schedule the next wake.
//!
//! The wake owns its budget: every outside call is bounded by it, and when it runs out the
//! wake skips the remaining network steps, keeps its state consistent and still schedules
//! the next wake ([`WakeEnding`]).
//!
//! Everything outside the core is reached through the ports in [`ports`]; pingers-client
//! implements them over pingers-sync, pingers-parties, pingers-meetup-session and the shell.
//! The state between wakes is [`WakeState`]; pingers-client stores its [`WakeSnapshot`].
#![forbid(unsafe_code)]

mod budget;
mod error;
mod pipeline;
pub mod ports;
mod state;

#[cfg(test)]
mod tests;

pub use error::{PortError, WakeError};
pub use pipeline::{
    FIX_MAX_AGE, GeofenceEvent, GeofenceTransition, KnockOutcome, LIVENESS_FIX_TIMEOUT,
    MailboxReads, PushReason, RETRY_AFTER, ROUTINE_FIX_TIMEOUT, UploadOutcome, WakeEnding, WakeEnv,
    WakeInput, WakeNotice, WakeReport, WakeSettings, WakeStep, WakeTrigger,
};
pub use state::{
    DecidedKnock, KnockStatus, KnownEpoch, LeaseSlot, LiveLease, PeerActivity, RealRecord,
    SentKnock, SpotSnapshot, SpotTransition, WakeSnapshot, WakeState,
};
