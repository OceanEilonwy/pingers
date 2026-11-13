// SPDX-License-Identifier: AGPL-3.0-only
//! Meetup session and location consent (sans-IO).
//!
//! What happens inside a Meetup (architecture §3, design B.5, B.6, B.6.1):
//! - exact-location consent: location keys and generations, grants, asks with expiry,
//!   rotation on revoke, scope and leave; fixed-cadence dummy frames;
//! - chat threads (one level), the meet point (last writer wins by Lamport clock, `AtMember`
//!   only from that member), keep-alives, the silent-member removal timer;
//! - the compass, from the device's own fix and heading plus the target's latest decrypted
//!   position.
//!
//! [`MeetupSession`] is a sans-IO state machine: the caller feeds it [`MeetupInput`]s with the
//! current time and a random generator, and carries out the [`MeetupOutput`]s (encrypting
//! frames into the MLS group, sending them on the live channel, raising events, committing
//! removals). It never computes distance or bearing to a member whose location key it does not
//! hold (design B.6.1).
//!
//! Location frames go out at one of two fixed cadences ([`CadenceMode`]), real or dummy, so
//! traffic does not depend on whether, or with whom, a member shares, nor on movement.
#![forbid(unsafe_code)]

mod board;
mod cadence;
mod chat;
mod compass;
mod consent;
mod convert;
mod error;
mod meet_point;
mod requests;
mod session;
mod sharing;
#[cfg(test)]
mod tests;

pub use cadence::{BACKGROUND_FRAME_INTERVAL, CadenceMode, LIVE_FRAME_INTERVAL};
pub use compass::{
    CompassReading, CompassTarget, Observer, TargetPosition, compass, coordinate_from_wire,
    coordinate_to_wire,
};
pub use consent::{
    AllowListChoice, AlwaysAllow, AskBack, ExactLocation, GrantScope, LocationAnswer,
    LocationVisibility,
};
pub use error::MeetupError;
pub use meet_point::{MeetPointDraft, MeetPointRegister, MeetPointUpdate};
pub use session::{
    ALONE_LIMIT, ASK_EXPIRY, EndReason, KEEP_ALIVE_INTERVAL, LOCATION_FRAME_INTERVAL, MeetupAction,
    MeetupConfig, MeetupEvent, MeetupInput, MeetupOutput, MeetupSession, Member, OutgoingFrame,
    PeerLocation, SHORT_GRANT, SILENCE_LIMIT,
};
