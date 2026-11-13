// SPDX-License-Identifier: AGPL-3.0-only
//! Server binary: routes to the crates above, nothing else.
//!
//! Usage (configuration format in the library's `config.rs`):
//!
//! - `pingers-server <config.toml>` runs the server until SIGTERM or Ctrl-C, then stops
//!   gracefully. It writes nothing unless the configuration turns counts logging on.
//! - `pingers-server pins <config.toml>` prints the public pin set of the configured gateway
//!   keys, ending with the line `PINGERS_GATEWAY_KEY_PINS=<hash>,<hash>` for the app build.
//! - `pingers-server keygen ohttp` prints a new gateway key secret (64 hex digits) for an
//!   `[[ohttp.keys]] secret`.
//!
//! An error is printed, with its causes, and the process exits with a failure status.
#![forbid(unsafe_code)]

use std::error::Error as _;
use std::process::ExitCode;

use pingers_server::{Command, Listeners, ServerError, StopSignals};

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
    match Command::parse(std::env::args_os().skip(1))? {
        Command::Pins { config } => {
            let config = pingers_server::load_config(&config)?;
            print(&pingers_server::pins(&config.ohttp_keys));
            Ok(())
        }
        Command::KeygenOhttp => {
            let secret = pingers_server::os_ohttp_secret()?;
            print(&format!("{}\n", secret.expose()));
            Ok(())
        }
        Command::Serve { config } => serve(&config),
    }
}

fn serve(path: &std::path::Path) -> Result<(), ServerError> {
    let config = pingers_server::load_config(path)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(ServerError::Runtime)?;
    runtime.block_on(async move {
        // Watched before anything is bound, so a supervisor's SIGTERM always stops gracefully.
        let mut signals = StopSignals::install();
        let listeners = Listeners::bind(&config).await?;
        pingers_server::serve(config, listeners, pingers_time::SystemClock, async move {
            signals.received().await;
        })
        .await
    })
}

#[expect(
    clippy::print_stdout,
    reason = "pins and keygen print their result for the operator to copy"
)]
fn print(text: &str) {
    print!("{text}");
}

#[expect(
    clippy::print_stderr,
    reason = "an error is the binary's only output besides opt-in counts and its commands' results"
)]
fn report(error: &ServerError) {
    eprintln!("pingers-server: {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}
