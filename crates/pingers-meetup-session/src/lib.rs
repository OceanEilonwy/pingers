// SPDX-License-Identifier: AGPL-3.0-only
//! A live meetup: channel, members, lifecycle.
//!
//! A Meetup while it is live (architecture §3, design B.5, B.6, B.6.1, G.4 to G.7):
//! - [`create`] makes the Meetup group with the knocker (external PSK, roster check), registers
//!   it on the server and posts the first commit; [`join`] joins from a Welcome;
//! - a task per Meetup, owned by its [`MeetupHandle`], holds the chunked OHTTP live channel
//!   (re-opened with backoff when it drops, then the group log is caught up) and ticks
//!   pingers-meetup's state machine at its `next_deadline()`, so location or dummy frames go
//!   out on the cadence each device sets for itself ([`MeetupScreen`]);
//! - every frame is padded to the Meetup size classes and sealed into the MLS group; frames
//!   that must wake absent members, and stored frames while the channel is down, go through
//!   the group log ([`GroupNetwork`]);
//! - consent decisions are applied by pingers-meetup; "Always allow" is written to the
//!   Party's allow-list, chat to the store;
//! - silent members are removed with a commit; a leaver's proposal is committed;
//! - leaving, removal or the end delete the group, keys and content (`Meetups::end`, which
//!   also deletes the MLS state), and the last member deletes the group on the server.
//!
//! # Decisions
//!
//! - **Store access on the blocking pool.** Each step opens a store session (the
//!   cross-process lock), runs in one transaction, and closes it, so the background wake can
//!   use the database between frames. The MLS group is loaded fresh in every step.
//! - **No resume.** pingers-meetup's state machine has no snapshot, so a Meetup lives only as
//!   long as its task. [`discard_stale`] deletes what a killed process left behind.
//! - **Positions only in memory and in the view.** The latest position of each peer whose key
//!   is held is kept for [`MeetupView`] (with its age, for "Updated 40 s ago"); nothing is
//!   written to `meetup_peer`, so there is still no location history.
//! - **Errors.** A frame that does not open, a stale commit or a lost commit race drops that
//!   input; a storage failure stops the task ([`SessionError`]).
#![forbid(unsafe_code)]

mod error;
mod group;
mod live;
mod network;
mod state;
mod view;

#[cfg(test)]
mod tests;

pub use error::SessionError;
pub use live::{
    CreateMeetup, Created, Ended, JoinMeetup, Joined, KnockKeyPackage, MAX_RECONNECT_DELAY,
    MeetupHandle, MeetupScreen, NETWORK_TIMEOUT, Runtime, SessionRng, create, discard_stale, join,
    knock_key_package, meetup_log_auth_key,
};
pub use network::{GroupNetwork, NetworkError};
pub use state::AddMember;
pub use view::{ChannelState, MeetupView, PeerView};
