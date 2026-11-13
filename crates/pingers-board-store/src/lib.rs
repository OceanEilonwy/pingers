// SPDX-License-Identifier: AGPL-3.0-only
//! Server: board records, expiry, paging, segment hashes.
//!
//! Board records in memory: append, expire after the lease horizon, paging with authenticated
//! cursors, segment hashes for equivocation checks. Owns its storage trait, with a SQLite
//! implementation behind a `sqlite` feature added later.
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
