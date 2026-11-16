// SPDX-License-Identifier: AGPL-3.0-only
//! `pingers <setup.toml>`: the owner's setup page on a monokulo-nodo appliance (see the
//! `pingers_setup` library). Reads the file, binds its loopback address and serves until
//! stopped.
#![forbid(unsafe_code)]

use std::io::{self, Write as _};
use std::process::ExitCode;
use std::sync::Arc;

use pingers_setup::{App, Config, ConfigError, serve};

/// Why the page did not start, or stopped.
#[derive(Debug, thiserror::Error)]
enum StartError {
    #[error("usage: pingers <setup.toml>")]
    Usage,
    #[error("reading the configuration file: {0}")]
    Read(#[source] io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("setting up the checks: {0}")]
    Clients(String),
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
            let _ignored = writeln!(io::stderr(), "pingers: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), StartError> {
    let mut arguments = std::env::args_os().skip(1);
    let (Some(path), None) = (arguments.next(), arguments.next()) else {
        return Err(StartError::Usage);
    };
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(&path).map_err(StartError::Read)?);
    let config = Config::parse(&text)?;
    let listen = config.listen;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartError::Runtime)?;
    runtime.block_on(async move {
        let app = Arc::new(App::new(config).map_err(StartError::Clients)?);
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(StartError::Bind)?;
        serve(listener, app, async {
            // If signals cannot be watched, run until killed.
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        })
        .await
        .map_err(StartError::Serve)
    })
}
