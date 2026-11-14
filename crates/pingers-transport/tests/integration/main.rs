// SPDX-License-Identifier: AGPL-3.0-only
//! End-to-end tests over real HTTP/2 on loopback: the client transports against the real
//! reference relay (pingers-ohttp-relay's library, the code its binary runs) and an
//! in-process test gateway built on pingers-ohttp.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod direct;
mod live;
mod one_shot;
mod support;
