// SPDX-License-Identifier: AGPL-3.0-only
//! All user-facing text.
//!
//! Every string Pingers shows, written from the typed values pingers-screens computes, as
//! fob-words does for Fob. This includes the "What your server sees" lists, generated from the
//! live configuration: which services share an operator, whether wake-up pushes are on, which
//! fallbacks are active and whether the privacy relay is working ([`server`]).
//!
//! Localisation: every function takes a [`Language`]. Only English exists; adding a language
//! is adding a variant, and the compiler then names every sentence that needs translating.
//! Plurals go through [`common::plural`], which applies each language's own plural rule.
//!
//! One module per screen, named as in pingers-screens.
#![forbid(unsafe_code)]

pub mod chat;
pub mod common;
pub mod compass;
pub mod device;
pub mod errors;
pub mod home;
pub mod invite;
pub mod location_request;
pub mod maps;
pub mod meet_point;
pub mod meetup;
pub mod notices;
pub mod party;
pub mod party_settings;
pub mod security;
pub mod server;
pub mod settings;
pub mod setup;
pub mod spot;
pub mod wakeups;
pub mod zones;

/// A language Pingers speaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Language {
    /// British English, as the design canvas is written.
    #[default]
    English,
}
