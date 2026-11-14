// SPDX-License-Identifier: AGPL-3.0-only
//! Reference relay: the library behind the `pingers-ohttp-relay` binary.
//!
//! A small reference OHTTP relay (forward only, full-duplex streaming, no logs) so relay
//! operators have something to run. It never decrypts anything. The binary
//! (`pingers-ohttp-relay <config.toml>`) only reads the file, binds and calls [`serve`]; tests
//! (pingers-testkit's harness, pingers-transport's integration tests) call the same [`serve`],
//! so they exercise the relay operators run rather than a copy.
//!
//! - **One gateway.** Every encapsulated request goes to the configured gateway URL, whatever
//!   the client asks for; the optional key config route goes to the configured key config URL.
//! - **Strips the client.** Requests are rebuilt with only the body and its media type
//!   (`message/ohttp-req` or `message/ohttp-chunked-req`); responses with only the status,
//!   the OHTTP media type and `cache-control: no-store`. The client's address is never passed
//!   on or stored.
//! - **Streams.** Bodies are forwarded frame by frame in both directions, so a chunked OHTTP
//!   exchange (design B.4.9) is full duplex over HTTP/2.
//! - **No logs.** Nothing is written about requests, addresses or failures; only a startup
//!   error is printed, by the binary.
//! - **HTTP/2 only, without TLS** (prior knowledge, both sides): terminate TLS for clients in
//!   front of the relay, for example with a TCP-level TLS proxy, and keep the gateway leg on a
//!   private network or tunnel. The gateway accepts only its configured relays (design B.12).
#![forbid(unsafe_code)]

mod config;
mod relay;
#[cfg(test)]
mod tests;

pub use config::{Config, ConfigError};
pub use relay::{Relay, serve};
