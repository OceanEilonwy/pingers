// SPDX-License-Identifier: AGPL-3.0-only
//! Reference relay binary: `pingers-ohttp-relay <config.toml>` (format in
//! `pingers_ohttp_relay::Config`). Reads the file, binds, and serves with
//! `pingers_ohttp_relay::serve` until Ctrl-C; everything else is in the library.
#![forbid(unsafe_code)]

use std::io::{self, Write as _};
use std::process::ExitCode;
use std::sync::Arc;

use pingers_ohttp_relay::{Config, ConfigError, Relay, serve};

/// Why the relay did not start, or stopped.
#[derive(Debug, thiserror::Error)]
enum StartError {
    #[error("usage: pingers-ohttp-relay <config.toml>")]
    Usage,
    #[error("reading the configuration file: {0}")]
    Read(#[source] io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("starting the runtime: {0}")]
    Runtime(#[source] io::Error),
    #[error("binding the listen address: {0}")]
    Bind(#[source] io::Error),
    #[error("accepting connections: {0}")]
    Serve(#[source] io::Error),
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ignored = writeln!(io::stderr(), "pingers-ohttp-relay: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), StartError> {
    let path = std::env::args_os().nth(1).ok_or(StartError::Usage)?;
    let text = std::fs::read_to_string(&path).map_err(StartError::Read)?;
    let config = Config::parse(&text)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartError::Runtime)?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(config.listen())
            .await
            .map_err(StartError::Bind)?;
        serve(listener, Arc::new(Relay::new(config)), async {
            // If signals cannot be watched, run until killed.
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        })
        .await
        .map_err(StartError::Serve)
    })
}
