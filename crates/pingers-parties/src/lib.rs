// SPDX-License-Identifier: AGPL-3.0-only
//! Party lifecycle: create, invite, join, roster, Spots.
//!
//! Everything about Parties as long-lived things: create a Party; invites (mailbox plus
//! URL-fragment secret) and their status; join with the inviter-fingerprint check and
//! safety-code verification; roster changes, removal and leave. Spots: create, edit (circle or
//! polygon), delete; the per-Party settings the design allows (server, relays, post-quantum,
//! location-visibility override).
//!
//! # Shape
//!
//! [`PartyLifecycle`] works over one pingers-store [`Session`](pingers_store::Session). Each
//! operation is one store transaction, so MLS state and the rows describing it commit
//! together. Nothing here does network I/O: operations that must reach the server return
//! [`Outbound`] values (group registration, commits with their Welcome, group messages,
//! mailbox posts and deletions, leave requests) for pingers-sync to send, and what arrives
//! (join requests, Welcomes, snapshots, group log messages, the server's answer to a commit)
//! comes back in as values.
//!
//! A commit stays pending until pingers-sync reports the server's answer:
//! [`PartyLifecycle::commit_accepted`] merges it, [`PartyLifecycle::commit_rejected`] drops
//! it. [`RosterView`] implements `pingers_presence::RosterLookup` for knock verification;
//! its `current_epoch` follows every merged commit.
//!
//! The crate's CONTRACT.md lists the decisions and the interim parts that wait on lower
//! crates.
#![forbid(unsafe_code)]

mod codec;
mod error;
mod invite;
mod joining;
mod lifecycle;
mod link;
mod ops;
mod outbound;
mod records;
mod roster;
mod safety;
mod settings;
mod spots;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use error::PartiesError;
pub use invite::{
    INVITE_LIFETIME, InviteId, InviteStatus, InviteSummary, JoinId, PendingJoin, Refusal,
};
pub use joining::{
    ArrivedWelcome, CreatedInvite, JoinDetails, JoinRequestOutcome, Joined, PreparedJoin,
};
pub use lifecycle::{CommitApplied, CreatedParty, Incoming, NewParty, PartyLifecycle};
pub use link::InviteLink;
pub use outbound::{LeaveRequest, Outbound, OutgoingCommit, WelcomeDelivery};
pub use roster::{Myself, RosterMember, RosterView};
pub use safety::{SafetyCode, inviter_fingerprint};
pub use settings::{PartySettings, PostQuantum, RelayChoice};
pub use spots::{NewSpot, SpotChange, SpotEdit, SpotShape};
