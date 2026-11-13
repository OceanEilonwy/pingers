// SPDX-License-Identifier: AGPL-3.0-only
//! Router and OHTTP client.
//!
//! Getting bytes to the server (architecture §3, design B.12): the [`Transport`] trait and the
//! router by [`PrivacyClass`]; anonymous and stream traffic never goes direct. The OHTTP client
//! over HTTP/2 (`hyper` + `rustls` with X25519MLKEM768), the chunked live channel with reconnect,
//! the split-channel fallback, and relay selection and health are implemented here by the
//! crate's owner. Tor and mixnets would be new `Transport` implementations here, nowhere else.
//!
//! This is also the lowest crate with tokio, so it owns the production [`TokioTimer`].
#![forbid(unsafe_code)]

use std::time::Duration;

use pingers_ohttp::{BinaryRequest, BinaryResponse, OhttpError};
use pingers_time::{MonotonicTime, Sleep, Timer};

/// How a request may travel (design B.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrivacyClass {
    /// Board, mailboxes, groups, tree leaves, relay key config, token redemption: always OHTTP,
    /// each request unlinkable to every other. Never sent direct.
    Anonymous,
    /// Registration, membership leaf registration, token issuance: direct (the request names the
    /// account anyway). Never shares a connection with anonymous traffic.
    Identified,
    /// The Meetup live channel: chunked OHTTP through the same relay. Never sent direct.
    Stream,
}

/// What a transport is, for "What your server sees".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransportKind {
    /// Straight to the server.
    Direct,
    /// Through an OHTTP relay.
    Ohttp,
}

/// Why a request did not complete. Never carries request contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// No relay is reachable in this wake; anonymous traffic waits or is dropped (design B.12).
    #[error("no OHTTP relay reachable")]
    NoRelay,
    /// The class may not use this transport (for example `Anonymous` over `Direct`).
    #[error("privacy class not allowed on this transport")]
    ClassNotAllowed,
    /// The injected timer's deadline passed.
    #[error("timed out")]
    Timeout,
    /// The wake's time budget is spent.
    #[error("wake budget exhausted")]
    BudgetExhausted,
    /// The server or relay answered with an HTTP error status.
    #[error("HTTP status {status}")]
    Status {
        /// The status code.
        status: u16,
    },
    /// Encapsulation failed, or the relay's key config does not match the pin.
    #[error("OHTTP failed")]
    Ohttp(#[from] OhttpError),
    /// The connection failed.
    #[error("connection failed")]
    Connection,
    /// The live stream closed.
    #[error("stream closed")]
    StreamClosed,
}

/// One long-lived bidirectional exchange: the Meetup live channel. Each frame is one padded
/// live frame. Cancellation-safe: dropping a pending `receive` loses no frame.
#[async_trait::async_trait]
pub trait DuplexStream: Send {
    /// Sends one frame.
    ///
    /// # Errors
    ///
    /// [`TransportError::StreamClosed`] or [`TransportError::Connection`].
    async fn send(&mut self, frame: Vec<u8>) -> Result<(), TransportError>;

    /// The next incoming frame, or `None` when the gateway ended the stream.
    ///
    /// # Errors
    ///
    /// [`TransportError::Ohttp`] if a chunk fails to open (including truncation).
    async fn receive(&mut self) -> Result<Option<Vec<u8>>, TransportError>;
}

/// A way to reach the server (design B.12). The router holds one per privacy class.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// One exchange. `Anonymous` requests must be unlinkable to each other: a fresh HPKE context
    /// per request for OHTTP, a fresh circuit for Tor.
    ///
    /// # Errors
    ///
    /// Any [`TransportError`].
    async fn request(
        &self,
        class: PrivacyClass,
        request: BinaryRequest,
    ) -> Result<BinaryResponse, TransportError>;

    /// Opens the live channel described by `request` (method, path and headers; the body is the
    /// frame stream).
    ///
    /// # Errors
    ///
    /// Any [`TransportError`].
    async fn stream(&self, request: BinaryRequest)
    -> Result<Box<dyn DuplexStream>, TransportError>;

    /// What this transport is.
    fn kind(&self) -> TransportKind;
}

/// The production [`Timer`], over tokio's clock. Must be created and used inside a tokio
/// runtime. Tests use `pingers_time::ManualClock` instead.
#[derive(Clone, Copy, Debug)]
pub struct TokioTimer {
    origin: tokio::time::Instant,
}

impl TokioTimer {
    /// A timer whose origin is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: tokio_now(),
        }
    }
}

impl Default for TokioTimer {
    fn default() -> Self {
        Self::new()
    }
}

impl Timer for TokioTimer {
    fn now(&self) -> MonotonicTime {
        MonotonicTime::from_elapsed(tokio_now().saturating_duration_since(self.origin))
    }

    fn sleep_until(&self, deadline: MonotonicTime) -> Sleep<'_> {
        let instant = self
            .origin
            .checked_add(deadline.elapsed())
            .unwrap_or_else(|| far_future(self.origin));
        Box::pin(tokio::time::sleep_until(instant))
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "TokioTimer is the one production reader of tokio's monotonic clock"
)]
fn tokio_now() -> tokio::time::Instant {
    tokio::time::Instant::now()
}

/// About thirty years after `origin`: a deadline that will not arrive, for overflowing inputs.
fn far_future(origin: tokio::time::Instant) -> tokio::time::Instant {
    origin
        .checked_add(Duration::from_hours(24 * 365 * 30))
        .unwrap_or(origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn sleep_completes_when_tokio_time_advances() {
        let timer = TokioTimer::new();
        let deadline = timer.now().saturating_add(Duration::from_secs(5));
        timer.sleep_until(deadline).await;
        assert!(timer.now() >= deadline);
    }
}
