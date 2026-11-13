// SPDX-License-Identifier: AGPL-3.0-only
//! Reference relay binary.
//!
//! A small reference OHTTP relay (forward only, full-duplex streaming, no logs) so relay
//! operators have something to run. It never decrypts anything.
//!
//! Usage: `pingers-ohttp-relay <config.toml>` (format in `config.rs`).
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
//!   error is printed, by `main`'s return.
//! - **HTTP/2 only, without TLS** (prior knowledge, both sides): terminate TLS for clients in
//!   front of the relay, for example with a TCP-level TLS proxy, and keep the gateway leg on a
//!   private network or tunnel. The gateway accepts only its configured relays (design B.12).
#![forbid(unsafe_code)]

mod config;
mod relay;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use anyhow::Context as _;

fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .context("usage: pingers-ohttp-relay <config.toml>")?;
    let text = std::fs::read_to_string(&path).context("reading the configuration file")?;
    let config = config::Config::parse(&text)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the runtime")?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(config.listen)
            .await
            .context("binding the listen address")?;
        let relay = Arc::new(relay::Relay::new(config));
        relay::serve(listener, relay, async {
            // If signals cannot be watched, run until killed.
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        })
        .await
        .context("accepting connections")
    })
}
