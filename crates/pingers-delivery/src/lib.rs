// SPDX-License-Identifier: AGPL-3.0-only
//! Server: MLS delivery log, subscriptions, live fan-out.
//!
//! The MLS delivery service for Party and Meetup groups alike (design B.2, B.4.9, B.5, D):
//!
//! - **Group logs.** Application messages are appended in arrival order; exactly one commit
//!   is accepted per epoch, and a second commit for the same epoch gets
//!   [`CommitOutcome::Conflict`] with the head epoch (HTTP 409). The check and the append are
//!   one atomic store operation, so concurrent committers are serialised.
//! - **Sealed-push subscriptions.** Each group keeps a list of sealed pushes, never accounts.
//!   A commit, or a stored message with `wake` set, yields one [`Wake`] per unexpired
//!   subscription not attached to a live channel; the server hands them to the push relay
//!   (pingers-mailbox's relay client). `Quiet` wakes carry a random 0 to 30 s delay.
//! - **Live fan-out.** Which open live channels belong to a group, with the caller's handle for
//!   each; no IP addresses. Frames and stored messages are forwarded to the other channels.
//! - **Expiry and deletion.** A group with no reads or writes for its lifetime class's idle
//!   time ([`GroupLifetime`]: two hours for a Meetup, 180 days for a Party, stated at creation)
//!   is deleted, as is one a member deletes; its live channels are closed with it.
//!
//! Welcomes are not handled here: they travel as mailbox posts (pingers-mailbox).
//!
//! Storage is the [`DeliveryStore`] trait, with [`MemoryDeliveryStore`] and, behind the
//! `sqlite` feature, `SqliteDeliveryStore`.
#![forbid(unsafe_code)]

mod error;
mod lifetime;
mod live;
mod memory;
mod service;
#[cfg(feature = "sqlite")]
mod sqlite;
mod store;

pub use error::DeliveryError;
pub use lifetime::{GroupLifetime, MEETUP_IDLE_LIFETIME, PARTY_IDLE_LIFETIME};
pub use live::ChannelId;
pub use memory::MemoryDeliveryStore;
pub use service::{
    CommitOutcome, Delivery, Forwarded, MAX_QUIET_DELAY, MAX_SUBSCRIPTION_LIFETIME, Posted, Wake,
};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteDeliveryStore;
pub use store::{
    CommitWrite, DeliveryStore, GroupHead, NewCommit, NewEntry, NewGroup, PAGE_ENTRIES,
    StoredSubscription,
};
