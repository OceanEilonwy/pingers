// SPDX-License-Identifier: AGPL-3.0-only
//! End-to-end tests over real HTTP/2 on loopback: the client transports against the real
//! reference relay (pingers-ohttp-relay's own source, compiled in below, since that crate is
//! a binary with no library target) and an in-process test gateway built on pingers-ohttp.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

#[path = "../../../pingers-ohttp-relay/src/config.rs"]
mod config;
#[path = "../../../pingers-ohttp-relay/src/relay.rs"]
mod relay;

mod direct;
mod live;
mod one_shot;
mod support;
