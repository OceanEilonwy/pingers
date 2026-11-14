// SPDX-License-Identifier: AGPL-3.0-only
//! View models.
//!
//! What each screen shows, computed from the client's state and the clock, as in fob-screens:
//! a [`ClientSnapshot`](snapshot::ClientSnapshot) (and, for editors, what the user has typed or
//! drawn so far) goes in; a plain value per screen comes out. Pure: no I/O, no clock reads, and
//! no words for people (those are pingers-words, written from these values).
//!
//! One module per screen: [`home`], [`party`] (New party, Advanced, Party), [`invite`] (Invite,
//! Accept invite), [`spot`] (Spot editor), [`draw`] (Draw area, the point-by-point polygon
//! editor), [`zones`] (Exclusion zones), [`settings`] (Privacy settings), [`server`] (What your
//! server sees), [`security`] (Security details), [`meetup`] (Meetup, Meetup ended), [`chat`],
//! [`meet_point`], [`compass`] and [`location_request`].
#![forbid(unsafe_code)]

mod error;

pub mod chat;
pub mod clock;
pub mod common;
pub mod compass;
pub mod draw;
#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;
pub mod home;
pub mod invite;
pub mod location_request;
pub mod meet_point;
pub mod meetup;
pub mod notifications;
pub mod party;
pub mod security;
pub mod server;
pub mod settings;
pub mod snapshot;
pub mod spot;
pub mod zones;

pub use error::ScreenError;
