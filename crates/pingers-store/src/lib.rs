// SPDX-License-Identifier: AGPL-3.0-only
//! SQLCipher storage.
//!
//! Encrypted local storage: SQLCipher through `rusqlite` with `bundled-sqlcipher`; migrations;
//! typed repositories per table; implements the pingers-mls storage trait. Cross-process lock
//! for the app and its background handlers; crash-safe transactions. No location history
//! table, by construction.
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
