// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-sync against the in-process harness: the real server, OHTTP relay and push relay.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod budget;
mod group;
mod mailbox;
mod queue;
mod rln;
mod support;
mod tokens;
