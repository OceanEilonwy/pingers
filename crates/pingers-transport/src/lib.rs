// SPDX-License-Identifier: AGPL-3.0-only
//! Router and OHTTP client.
//!
//! Getting bytes to the server (architecture §3, design B.12):
//! - the [`Transport`] trait and the [`Router`] by [`PrivacyClass`]; anonymous and stream
//!   traffic never goes direct, and with no relay reachable it is not sent at all
//!   ([`TransportError::NoRelay`]);
//! - [`OhttpTransport`]: OHTTP over HTTP/2 (`hyper` + `rustls`, TLS 1.3 offering the
//!   X25519MLKEM768 hybrid key exchange first), a fresh HPKE context per request, relay
//!   selection and health, and the gateway key config fetched through the relay and checked
//!   against the pin;
//! - the chunked live channel ([`Transport::stream`]): keep-alives, reconnects with backoff and
//!   a fresh HPKE context, and the split-channel fallback for relays that cannot stream full
//!   duplex (design B.4.9);
//! - [`DirectTransport`]: HTTPS for the `Identified` class only.
//!
//! Every network step has a deadline from the injected [`Timer`](pingers_time::Timer), and
//! every spawned task has an owner that aborts it when dropped. Tor and mixnets would be new
//! `Transport` implementations here, nowhere else.
//!
//! This is also the lowest crate with tokio, so it owns the production [`TokioTimer`].
#![forbid(unsafe_code)]

mod direct;
mod endpoint;
mod http;
mod live;
mod ohttp;
mod relay;
mod router;
mod timer;
mod tls;

use pingers_ohttp::{BinaryRequest, BinaryResponse, OhttpError};

pub use direct::{DirectConfig, DirectTransport};
pub use live::{KEEP_ALIVE_CHUNK, MAX_FRAME_BYTES};
pub use ohttp::{LiveSettings, OhttpConfig, OhttpTransport, Timeouts};
pub use relay::{Backoff, KeyPin, Relay};
pub use router::{Router, Routes};
pub use timer::TokioTimer;
pub use tls::{TlsRoots, certificates_from_pem};

/// The randomness an [`OhttpTransport`] draws HPKE keys and nonces from. Production passes an
/// OS-seeded CSPRNG; tests pass a seeded one (style guide §6).
pub type TransportRng = Box<dyn rand_core::CryptoRng + Send>;

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
    /// Encapsulation failed, a chunk did not open, or the relay's key config does not match
    /// the pin ([`OhttpError::KeyMismatch`]: raise `RelayKeyMismatch`; the transport refuses
    /// all anonymous traffic until [`OhttpTransport::repin`]).
    #[error("OHTTP failed")]
    Ohttp(#[from] OhttpError),
    /// The connection failed.
    #[error("connection failed")]
    Connection,
    /// The live stream closed.
    #[error("stream closed")]
    StreamClosed,
    /// A configured URL is not usable (not absolute, wrong scheme, or not TLS where TLS is
    /// required).
    #[error("invalid URL")]
    InvalidUrl,
    /// A request or frame cannot be carried: a malformed method or header, a stream request
    /// with a body, or a frame that is empty, too large or a keep-alive.
    #[error("invalid request")]
    InvalidRequest,
    /// A trusted root certificate could not be parsed, or TLS could not be configured.
    #[error("invalid certificate")]
    InvalidCertificate,
    /// A response was larger than allowed.
    #[error("response too large")]
    TooLarge,
}

/// One long-lived bidirectional exchange: the Meetup live channel. Each frame is one padded
/// live frame. Cancellation-safe: dropping a pending `receive` loses no frame.
///
/// Reconnects are handled inside: a dropped exchange is re-opened with a fresh HPKE context
/// after a backoff, and frames in flight at that moment may be lost (they are ephemeral, or
/// stored in the group log). Dropping the stream aborts it at once; [`DuplexStream::close`]
/// ends it cleanly with a final chunk.
#[async_trait::async_trait]
pub trait DuplexStream: Send {
    /// Sends one frame.
    ///
    /// # Errors
    ///
    /// [`TransportError::StreamClosed`] once the channel has ended;
    /// [`TransportError::InvalidRequest`] for an empty frame, one above [`MAX_FRAME_BYTES`],
    /// or one equal to [`KEEP_ALIVE_CHUNK`].
    async fn send(&mut self, frame: Vec<u8>) -> Result<(), TransportError>;

    /// The next incoming frame, or `None` when the gateway ended the stream.
    ///
    /// # Errors
    ///
    /// [`TransportError::Ohttp`] if a chunk fails to open; when reconnecting has failed
    /// [`LiveSettings::max_reconnects`] times in a row, the last reconnect error (for example
    /// [`TransportError::NoRelay`]), or [`OhttpError::KeyMismatch`] at once. After an error,
    /// [`TransportError::StreamClosed`].
    async fn receive(&mut self) -> Result<Option<Vec<u8>>, TransportError>;

    /// Ends the stream: sends the final chunk and waits for the exchange to finish.
    ///
    /// # Errors
    ///
    /// [`TransportError::Timeout`] if it did not finish in time (it is aborted then).
    async fn close(&mut self) -> Result<(), TransportError>;
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

    /// Fetches the gateway's key config afresh through a relay and checks it against the pin:
    /// the reachability check ("What your server sees"). It sends nothing to the gateway and
    /// carries no account, Party or mailbox. A transport without a gateway key (direct)
    /// answers [`TransportError::ClassNotAllowed`].
    ///
    /// # Errors
    ///
    /// [`TransportError::NoRelay`] if no relay answered;
    /// [`TransportError::Ohttp`] with [`OhttpError::KeyMismatch`] if the config does not match
    /// the pin (latched as for a request); any other [`TransportError`].
    async fn check_gateway_key(&self) -> Result<(), TransportError> {
        Err(TransportError::ClassNotAllowed)
    }

    /// The hash of the gateway key config this transport verified (after a request or
    /// [`Transport::check_gateway_key`]), for the account record (`identity.server_pin`) and
    /// Party state. `None` before one was verified, and for a transport without a gateway key.
    fn gateway_key_hash(&self) -> Option<pingers_proto::KeyConfigHash> {
        None
    }

    /// What this transport is.
    fn kind(&self) -> TransportKind;
}
