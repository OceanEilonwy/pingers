// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-sync against the in-process harness: the real server, OHTTP relay and push relay.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod board;
mod budget;
mod group;
mod group_extras;
mod lifecycle;
mod mailbox;
mod queue;
mod restart;
mod rln;
mod server;
mod support;
mod tokens;
mod wake_port;
