// SPDX-License-Identifier: AGPL-3.0-only
//! The background pipeline: one wake, start to finish.
//!
//! Entry points ([`WakeTrigger`]): a fix, an OS geofence transition, a scheduled wake, a push,
//! a Focus change. [`WakeState::run`] runs the fixed sequence of design E.2:
//!
//! 1. ask pingers-gate (Focus read fresh, exclusion zones and their trail first);
//! 2. update Spot presence (pingers-geo), choose the OS geofences again if the device moved,
//!    and leave Meetups the device left or may no longer share in;
//! 3. then one board round per server (each Party lives on one server, with its own board,
//!    mailboxes and group logs), for every server with a Party and every board that still holds
//!    a lease or waits for a Welcome:
//!    1. pull that server's group logs, then build and upload one lease to its board (its
//!       Parties' real records plus dummies), read the board and trial-match
//!       (pingers-presence), only against that server's Spots that are confirmed `Inside`;
//!    2. knock on matches, and answer knocks on that board's records with a liveness check;
//!    3. hand accepted knocks and Welcomes to pingers-meetup-session;
//!    4. refill that server's mailbox pool;
//! 4. set the location mode and schedule the next wake.
//!
//! The wake owns its budget: every outside call is bounded by it. Each board round gets an
//! equal share of what is left when it starts, and a failure or timeout ends only that round's
//! network steps ([`BoardReport`]), so one unreachable server never stalls the others. The
//! state stays consistent and the next wake is always scheduled ([`WakeEnding`]).
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
    BoardReport, FIX_MAX_AGE, GeofenceEvent, GeofenceTransition, KnockOutcome, LIVENESS_FIX_TIMEOUT,
    MailboxReads, PushReason, RETRY_AFTER, ROUTINE_FIX_TIMEOUT, UploadOutcome, WakeEnding, WakeEnv,
    WakeInput, WakeNotice, WakeReport, WakeSettings, WakeStep, WakeTrigger,
};
pub use state::{
    BoardSnapshot, DecidedKnock, KnockStatus, KnownEpoch, LeaseSlot, LiveLease, PeerActivity, RealRecord,
    SentKnock, SpotSnapshot, SpotTransition, WakeSnapshot, WakeState,
};
