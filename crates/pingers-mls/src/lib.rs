// SPDX-License-Identifier: AGPL-3.0-only
//! openmls wrapper: Party and meetup groups.
//!
//! A narrow wrapper over OpenMLS (architecture §3):
//! - create, join and leave Party groups; create Meetup groups bound by the external PSK;
//!   add and remove with the roster check (every added leaf key must be in the Party roster);
//! - the ciphersuite policy and the `pingers_grant_key` leaf extension;
//! - exporter access for pingers-crypto's key schedule;
//! - storage through the [`MlsStorage`] trait, which pingers-store implements, so this crate
//!   has no database.
//!
//! Groups are typestate: `Group<'_, S, Party>` and `Group<'_, S, Meetup>` share the common
//! operations and each adds its own (the presence exporter only on Party groups, grant keys
//! only on Meetup groups). Configuration is fixed by design B.2: handshake messages as
//! `PrivateMessage`, no external commits, proposals or senders.
#![forbid(unsafe_code)]

mod adapter;
mod group;
#[cfg(any(test, feature = "test-util"))]
mod memory;
mod policy;
mod provider;
mod roster;
mod storage;
#[cfg(test)]
mod tests;

pub use group::{
    CipherSuitePolicy, Commit, CommitSummary, CreatedMeetup, Group, KeyPackagePurpose, Meetup,
    MeetupCreation, MeetupPskBinding, MeetupPskId, MemberGrantKey, MlsContext, MlsError, Party,
    ProcessedMessage, SelfRemoval,
};
pub use roster::{Roster, RosterEntry};
pub use storage::{MlsStorage, StorageError};

#[cfg(any(test, feature = "test-util"))]
pub use memory::MemoryStorage;
