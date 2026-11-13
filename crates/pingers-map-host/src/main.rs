// SPDX-License-Identifier: AGPL-3.0-only
//! Map host binary: `pingers-map-host <config.toml>`.
//!
//! Serves whole-region PMTiles packs, manifests, search indexes and a generated catalog over
//! HTTP/1.1 and h2c behind a TLS-terminating reverse proxy. Logging off by default. See the
//! library documentation.
#![forbid(unsafe_code)]

use std::error::Error as _;
use std::path::PathBuf;
use std::process::ExitCode;

use pingers_map_host::error::MapHostError;
use pingers_time::SystemClock;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&error);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), MapHostError> {
    let mut arguments = std::env::args_os().skip(1);
    let (Some(path), None) = (arguments.next(), arguments.next()) else {
        return Err(MapHostError::Usage);
    };
    let config = pingers_map_host::load_config(&PathBuf::from(path))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(MapHostError::Serve)?;
    runtime.block_on(async {
        let listener = pingers_map_host::listen(&config).await?;
        pingers_map_host::serve(config, listener, SystemClock, shutdown_signal()).await
    })
}

/// Completes on Ctrl-C or SIGTERM. If a signal handler cannot be installed, that signal is
/// simply not waited for.
async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

#[expect(
    clippy::print_stderr,
    reason = "a binary reports why it could not start"
)]
fn report(error: &MapHostError) {
    eprintln!("pingers-map-host: {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}
