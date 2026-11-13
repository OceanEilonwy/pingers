// SPDX-License-Identifier: AGPL-3.0-only
//! Server sync: group logs, mailboxes, request queue.
//!
//! Talking to the server, reliably, for everyone else: fetch and apply group log messages;
//! post commits (one per epoch, retry on conflict); mailbox registration and reads;
//! subscriptions with sealed push; the queue for anonymous requests that could not be sent
//! in this wake, with drop rules; token supply (pre-fetched Privacy Pass, RLN proofs made only
//! in the main app). Knows nothing about what the messages mean; it hands them to their owner.
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
