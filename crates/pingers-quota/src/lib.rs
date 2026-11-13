// SPDX-License-Identifier: AGPL-3.0-only
//! Server: RLN tree, nullifiers, Privacy Pass issuer, limits.
//!
//! Who may post: the RLN membership tree and roots, the nullifier set, Privacy Pass issuance
//! and the spent set, per-bucket limits. Owns its storage trait, with a SQLite
//! implementation behind a `sqlite` feature added later.
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
