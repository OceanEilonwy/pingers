// SPDX-License-Identifier: AGPL-3.0-only
//! UniFFI bindings.
//!
//! UniFFI only. It converts types, maps errors, turns async calls into Kotlin `suspend`, and
//! exposes `Platform` and `Events` as callback interfaces. No logic. A `wipe-freed` feature
//! with a zeroing allocator will be added as in fob-ffi (the one crate allowed `unsafe`,
//! with a written reason).
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
