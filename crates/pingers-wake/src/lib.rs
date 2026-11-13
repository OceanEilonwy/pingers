// SPDX-License-Identifier: AGPL-3.0-only
//! The background pipeline: one wake, start to finish.
//!
//! One background wake, start to finish. Entry points: a fix, an OS geofence transition, a
//! scheduled wake, a push, a Focus change. Fixed sequence: ask pingers-gate; update Spot
//! presence (pingers-geo); build and upload the lease, sync the board and trial-match
//! (pingers-presence); knock or answer a knock (liveness check); hand a successful match to
//! pingers-meetup-session (through pingers-client); choose the next OS geofences and the next
//! wake. Owns the wake budget and stops cleanly when it runs out.
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
