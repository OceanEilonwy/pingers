// SPDX-License-Identifier: AGPL-3.0-only
//! Push relay binary: `pingers-push-relay <config.toml>`, and the operator subcommands `pins`,
//! `keygen` and `credential-hash` (see the library's `command` module).
//!
//! The publisher-run push relay. Stateless: HPKE open, check the mailbox binding, send to FCM
//! HTTP v1 or a UnifiedPush endpoint; per-token and per-server rate limits; key config
//! endpoint; logging off by default. No database. See the library documentation.
#![forbid(unsafe_code)]

use std::error::Error as _;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::ExitCode;

use pingers_push_relay::command::{self, Command};
use pingers_push_relay::error::RelayError;
use pingers_time::SystemClock;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;
use zeroize::Zeroizing;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&error);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), RelayError> {
    match Command::parse(std::env::args_os().skip(1))? {
        Command::Pins { config } => {
            let config = pingers_push_relay::load_config(&config)?;
            print(&command::pins_output(&config))
        }
        Command::Keygen => {
            let mut seed = Zeroizing::new([0_u8; 32]);
            getrandom::fill(seed.as_mut_slice()).map_err(|_error| RelayError::Entropy)?;
            let mut rng = ChaCha20Rng::from_seed(*seed);
            print(&command::keygen_output(&mut rng))
        }
        Command::CredentialHash => {
            let mut input = Zeroizing::new(String::new());
            std::io::stdin()
                .take(command::MAX_CREDENTIAL_INPUT)
                .read_to_string(&mut input)
                .map_err(RelayError::ReadInput)?;
            print(&command::credential_hash_output(&input)?)
        }
        Command::Serve { config } => serve(&config),
    }
}

fn print(text: &str) -> Result<(), RelayError> {
    let mut output = std::io::stdout().lock();
    output
        .write_all(text.as_bytes())
        .and_then(|()| output.flush())
        .map_err(RelayError::WriteOutput)
}

fn serve(path: &Path) -> Result<(), RelayError> {
    let config = pingers_push_relay::load_config(path)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(RelayError::Serve)?;
    runtime.block_on(async {
        let listener = pingers_push_relay::listen(&config).await?;
        pingers_push_relay::serve(config, listener, SystemClock, shutdown_signal()).await
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
    reason = "a binary reports why it could not start; errors carry no secrets"
)]
fn report(error: &RelayError) {
    eprintln!("pingers-push-relay: {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}
