// SPDX-License-Identifier: AGPL-3.0-only
//! Server binary: routes to the crates above, nothing else.
//!
//! Usage: `pingers-server <config.toml>` (format in the library's `config.rs`). Runs until
//! interrupted. Writes nothing unless the configuration turns counts logging on; a startup
//! error is printed by `main`'s return.
#![forbid(unsafe_code)]

use anyhow::Context as _;

fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .context("usage: pingers-server <config.toml>")?;
    let config = pingers_server::load_config(path.as_ref())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the runtime")?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(config.listen)
            .await
            .context("binding the listen address")?;
        pingers_server::serve(config, listener, pingers_time::SystemClock, async {
            // If signals cannot be watched, run until killed.
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        })
        .await
        .context("serving")
    })
}
