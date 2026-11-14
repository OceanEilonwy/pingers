// SPDX-License-Identifier: AGPL-3.0-only
//! The binary's stop signals: Ctrl-C (SIGINT) and SIGTERM, which service supervisors send
//! (runit's `sv stop`, systemd, nodo). Either stops the server gracefully.

use tokio::signal::unix::{Signal, SignalKind, signal};

/// Watches for SIGINT and SIGTERM. Install it before serving: from then on neither signal
/// kills the process outright, and [`StopSignals::received`] completes on the first.
pub struct StopSignals {
    interrupt: Option<Signal>,
    terminate: Option<Signal>,
}

impl StopSignals {
    /// Starts watching. A signal that cannot be watched keeps its default action (the process
    /// is killed, as before); if neither can, [`StopSignals::received`] never completes. Must
    /// be called inside a tokio runtime.
    #[must_use]
    pub fn install() -> Self {
        Self {
            interrupt: signal(SignalKind::interrupt()).ok(),
            terminate: signal(SignalKind::terminate()).ok(),
        }
    }

    /// Completes when either signal arrives. Cancellation-safe.
    pub async fn received(&mut self) {
        tokio::select! {
            Some(()) = next(self.interrupt.as_mut()) => {}
            Some(()) = next(self.terminate.as_mut()) => {}
            else => std::future::pending().await,
        }
    }
}

/// The next delivery of `watched`, or never if it is not watched.
async fn next(watched: Option<&mut Signal>) -> Option<()> {
    match watched {
        Some(watched) => watched.recv().await,
        None => std::future::pending().await,
    }
}
