// SPDX-License-Identifier: AGPL-3.0-only
//! Party lifecycle: create, invite, join, roster, Spots.
//!
//! Everything about Parties as long-lived things: create a Party; invites (mailbox plus
//! URL-fragment secret) and their status; join with safety-code verification; roster changes
//! and removal; leave. Spots: create, edit (circle or polygon), delete; the per-Party settings
//! the design allows (server, relays, post-quantum).
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
