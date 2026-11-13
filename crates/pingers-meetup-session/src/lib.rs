// SPDX-License-Identifier: AGPL-3.0-only
//! A live meetup: channel, members, lifecycle.
//!
//! A Meetup while it is live: creates or joins the Meetup group; holds the chunked OHTTP live
//! channel; sends location or dummy frames on the cadence; applies consent decisions from
//! pingers-meetup; removes silent members; ends the Meetup and deletes its keys and content.
//!
//! Wave 1 scaffold: only the crate's job is fixed here. Its public API is designed by the crate's
//! owner against the contracts of the crates it depends on.
#![forbid(unsafe_code)]
