// SPDX-License-Identifier: AGPL-3.0-only
//! View models.
//!
//! What each screen shows, computed from core state and the clock: Home, Party, Spot editor
//! (including the point-by-point polygon editing model), Exclusion zones, Settings, Meetup,
//! Chat, Meet point, Compass, Location request, Invite, Advanced, What your server sees.
//! Pure: no I/O and no strings for people (those are pingers-words).
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
