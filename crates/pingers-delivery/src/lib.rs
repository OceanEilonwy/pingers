// SPDX-License-Identifier: AGPL-3.0-only
//! Server: MLS delivery log, subscriptions, live fan-out.
//!
//! The MLS delivery service: group logs with one commit per epoch, sealed-push subscriptions,
//! Welcome routing via mailboxes, the live-channel fan-out registry, group expiry. Owns its
//! storage trait, with a SQLite implementation behind a `sqlite` feature added later.
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
