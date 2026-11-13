// SPDX-License-Identifier: AGPL-3.0-only
//! Round trips, rejection of malformed input and known-answer vectors for every wire type.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod debug;
mod rejects;
mod round_trip;
mod strategies;
mod vectors;
