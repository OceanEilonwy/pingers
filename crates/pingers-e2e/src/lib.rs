// SPDX-License-Identifier: AGPL-3.0-only
//! End-to-end protocol scenarios and privacy checks against the real server behind a relay.
//!
//! The design's sequences G.3 to G.7, played by simulated devices built from the merged
//! protocol crates (pingers-presence, pingers-mls, pingers-geo, pingers-crypto, pingers-proto)
//! talking to the real pingers-server through an OHTTP relay with pingers-transport's
//! [`OhttpTransport`](pingers_transport::OhttpTransport). After each scenario the tests look at
//! what the server actually stored and counted and check it against design A.2.
//!
//! - [`Stack`]: pingers-server (mailboxes in an SQLite file the tests can open), the testkit's
//!   OHTTP relay, and a [`Tap`] between relay and server that counts bytes and connects from
//!   its own loopback address ([`RELAY_EGRESS`]), which is the only address the server accepts
//!   OHTTP from. Devices sit on [`DEVICE_ADDRESS`], so a device request that bypassed the relay
//!   would be refused.
//! - [`Device`]: one phone's protocol state: account key, MLS storage, a Spot presence machine
//!   fed with fixes, the mailbox pool, its lease, and its Meetup.
//! - [`Recorder`]: every inner request each device sent (what the server decapsulates) and
//!   every answer, for the privacy scans in [`inspect`].
//! - [`load`]: N simulated devices posting leases, for board size and sync bandwidth.
//!
//! # Known gaps
//!
//! - **Group logs still go to a pingers-delivery instance the stack owns** ([`Stack::delivery`]),
//!   with its store in an SQLite file the tests inspect. pingers-server's group routes are
//!   wired now (`?lifetime=meetup&epoch=n`, Grp signatures and sessions, see its
//!   `groups.rs`); moving the [`Device`] group calls to the relay is the next step, and the
//!   scenarios do not change.
//! - **Pushes are not delivered** in this stack: its listed push relay does not resolve. The
//!   testkit harness serves its push relay over HTTPS with a test CA the server trusts, so a
//!   stack built on it would see wakes. Devices read their mailboxes as they would when woken.
//! - **Anonymous requests carry Privacy Pass tokens** from a [`TokenPool`]; RLN proofs are
//!   exercised by pingers-server's own end-to-end test.
//! - G.1 (Party creation and invite) runs over MLS directly: it needs the group routes.
#![forbid(unsafe_code)]

mod credentials;
mod device;
mod error;
pub mod inspect;
pub mod load;
mod recorder;
mod stack;
mod tap;
mod wire;

pub use credentials::TokenPool;
pub use device::{Answer, Device, KnockOutcome, Place, SyncReport, form_party};
pub use error::E2eError;
pub use recorder::{Exchange, Recorder};
pub use stack::{DEVICE_ADDRESS, GroupDelivery, RELAY_EGRESS, Stack};
pub use tap::Tap;
