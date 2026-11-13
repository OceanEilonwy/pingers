// SPDX-License-Identifier: AGPL-3.0-only
//! Server binary: routes to the crates above, nothing else.
//!
//! Usage: `pingers-server <config.toml>` (format in the library's `config.rs`). Runs until
//! interrupted. Writes nothing unless the configuration turns counts logging on; a startup
//! error is printed, with its causes, and the process exits with a failure status.
#![forbid(unsafe_code)]

use std::error::Error as _;
use std::process::ExitCode;

use pingers_server::ServerError;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&error);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), ServerError> {
    let path = std::env::args_os().nth(1).ok_or(ServerError::Usage)?;
    let config = pingers_server::load_config(path.as_ref())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(ServerError::Runtime)?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(config.listen)
            .await
            .map_err(ServerError::Listen)?;
        pingers_server::serve(config, listener, pingers_time::SystemClock, async {
            // If signals cannot be watched, run until killed.
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        })
        .await
    })
}

#[expect(
    clippy::print_stderr,
    reason = "a startup error is the binary's only output besides opt-in counts"
)]
fn report(error: &ServerError) {
    eprintln!("pingers-server: {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}
