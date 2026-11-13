// SPDX-License-Identifier: AGPL-3.0-only
//! Push relay binary: `pingers-push-relay <config.toml>`.
//!
//! The publisher-run push relay. Stateless: HPKE open, check the mailbox binding, send to FCM
//! HTTP v1 or a UnifiedPush endpoint; per-token and per-server rate limits; key config
//! endpoint; logging off by default. No database. See the library documentation.
#![forbid(unsafe_code)]

use std::error::Error as _;
use std::path::PathBuf;
use std::process::ExitCode;

use pingers_push_relay::error::RelayError;
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

fn run() -> Result<(), RelayError> {
    let mut arguments = std::env::args_os().skip(1);
    let (Some(path), None) = (arguments.next(), arguments.next()) else {
        return Err(RelayError::Usage);
    };
    let config = pingers_push_relay::load_config(&PathBuf::from(path))?;
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
