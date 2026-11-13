// SPDX-License-Identifier: AGPL-3.0-only
//! All user-facing text.
//!
//! Every user-facing string, from typed scenarios the other crates describe. This includes the
//! "What your server sees" lists generated from the actual configuration: which relays, which
//! fallbacks are active, and whether operators coincide. Localisation lives here.
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
