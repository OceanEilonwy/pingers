// SPDX-License-Identifier: AGPL-3.0-only
//! Server: mailboxes and the wake rule.
//!
//! Anonymous mailboxes, posting rules, expiry, and the wake rule (forward the sealed push to
//! the relay, never open it). Owns its storage trait, with a SQLite implementation behind a
//! `sqlite` feature added later.
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
