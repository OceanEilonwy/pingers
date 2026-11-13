// SPDX-License-Identifier: AGPL-3.0-only
//! Server: routes to the crates above, nothing else.
//!
//! The self-hosted server. It routes requests to pingers-board-store, pingers-mailbox,
//! pingers-delivery and pingers-quota and holds no logic of its own (architecture §3). `axum`
//! over `hyper`, HTTP/2 with prior knowledge (TLS is terminated in front, as for the reference
//! relay).
//!
//! - **OHTTP gateway** at `POST /ohttp`, accepted only from configured relays (design B.12,
//!   an address allow-list): one-shot requests are decapsulated, decoded from Binary HTTP and
//!   answered like direct ones; chunked exchanges carry the live channel
//!   (`POST /v1/groups/{id}/live`, design B.4.9) through a [`LiveChannel`].
//! - **Channels by privacy class**: identified routes are served directly, anonymous ones only
//!   inside one-shot OHTTP, the live channel only inside chunked OHTTP; a route on the wrong
//!   channel is `421 Misdirected Request`. Public routes (parameters, key configs) either way.
//! - **Key config** at `GET /v1/ohttp/config` (`application/ohttp-keys`, one config).
//! - **Metrics** at `GET /metrics` for configured addresses: totals per channel and outcome,
//!   no identifiers. **Logging** is off by default; when on, it writes those totals only.
//!
//! Routes whose crate has no API yet answer `501 Not Implemented` (see `dispatch.rs`).
#![forbid(unsafe_code)]

mod config;
mod dispatch;
mod error;
mod gateway;
mod http;
mod live;
mod metrics;
mod route;

use std::future::Future;
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pingers_ohttp::{Gateway, KeyConfig};
use pingers_time::{Clock, Timer};
use pingers_transport::TokioTimer;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;
use zeroize::Zeroizing;

pub use crate::config::{
    AddressList, ConfigError, Logging, OhttpKeys, RelayAllowList, ServerConfig,
};
pub use crate::error::ServerError;
pub use crate::live::{DeliveryLive, LIVE_BUFFER, LiveChannel, LiveFrame, LiveRefused};

use crate::metrics::Metrics;

/// The peer address of a connection, for the relay and metrics checks only.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Peer(pub(crate) SocketAddr);

/// The seed of the server's random number generator (OHTTP response nonces). No `Debug`.
pub struct RngSeed {
    bytes: Zeroizing<[u8; 32]>,
}

impl RngSeed {
    /// Wraps a seed: from the operating system in production, fixed in tests.
    #[must_use]
    pub const fn from_bytes(bytes: Zeroizing<[u8; 32]>) -> Self {
        Self { bytes }
    }
}

/// The server's view of the outside world, injected into [`serve_with`].
pub struct Outside<C, T, L> {
    /// Wall time.
    pub clock: C,
    /// Monotonic time, for request timeouts and the counts report.
    pub timer: T,
    /// Seeds the random number generator.
    pub seed: RngSeed,
    /// Where live channels go.
    pub live: L,
}

/// What every request handler shares.
pub(crate) struct Shared {
    pub(crate) gateway: Gateway,
    pub(crate) served_config: KeyConfig,
    pub(crate) relays: RelayAllowList,
    pub(crate) metrics_access: AddressList,
    pub(crate) metrics: Metrics,
    pub(crate) request_timeout: Duration,
    pub(crate) timer: Arc<dyn Timer>,
    pub(crate) live: Arc<dyn LiveChannel>,
    rng: Mutex<ChaCha20Rng>,
}

impl Shared {
    /// Runs `use_rng` with the server's random number generator.
    pub(crate) fn with_rng<R>(&self, use_rng: impl FnOnce(&mut ChaCha20Rng) -> R) -> R {
        // A panic while holding the lock cannot leave a generator half-updated in a way that
        // matters: its next output is still unpredictable.
        let mut rng = self.rng.lock().unwrap_or_else(PoisonError::into_inner);
        use_rng(&mut rng)
    }
}

/// Reads and checks the configuration file at `path`.
///
/// # Errors
///
/// [`ServerError::ReadConfig`] or [`ServerError::Config`].
pub fn load_config(path: &Path) -> Result<ServerConfig, ServerError> {
    let text = Zeroizing::new(std::fs::read_to_string(path).map_err(ServerError::ReadConfig)?);
    ServerConfig::parse(&text).map_err(ServerError::Config)
}

/// A seed from the operating system's random number generator.
fn os_seed() -> Result<RngSeed, ServerError> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(bytes.as_mut_slice()))
        .map_err(ServerError::Entropy)?;
    Ok(RngSeed::from_bytes(bytes))
}

/// Runs the server on `listener` until `shutdown` completes, with the production timer, a
/// generator seeded by the operating system, and [`DeliveryLive`]. The binary passes
/// [`pingers_time::SystemClock`]. Must be called inside a tokio runtime.
///
/// # Errors
///
/// [`ServerError::Entropy`] if the operating system's generator cannot be read.
pub async fn serve(
    config: ServerConfig,
    listener: tokio::net::TcpListener,
    clock: impl Clock + 'static,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let outside = Outside {
        clock,
        timer: TokioTimer::new(),
        seed: os_seed()?,
        live: DeliveryLive,
    };
    serve_with(config, outside, listener, shutdown).await;
    Ok(())
}

/// As [`serve`], with time, randomness and the live channel injected: for tests and the
/// in-process harness. Returns once `shutdown` completes and every connection is closed.
pub async fn serve_with<C: Clock + 'static, T: Timer + 'static, L: LiveChannel>(
    config: ServerConfig,
    outside: Outside<C, T, L>,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) {
    let Outside {
        clock,
        timer,
        seed,
        live,
    } = outside;
    let served_config = config.ohttp_keys.served().clone();
    let timer: Arc<dyn Timer> = Arc::new(timer);
    let shared = Arc::new(Shared {
        gateway: Gateway::new(config.ohttp_keys.into_keys()),
        served_config,
        relays: config.relays,
        metrics_access: config.metrics,
        metrics: Metrics::default(),
        request_timeout: config.request_timeout,
        timer: Arc::clone(&timer),
        live: Arc::new(live),
        rng: Mutex::new(ChaCha20Rng::from_seed(*seed.bytes)),
    });
    let reporter = match config.logging {
        Logging::Off => None,
        Logging::Counts { interval } => {
            let shared = Arc::clone(&shared);
            Some(tokio::spawn(async move {
                let report = CountsReport {
                    metrics: &shared.metrics,
                    clock: &clock,
                    timer: &*timer,
                    interval,
                };
                report.run(write_counts).await;
            }))
        }
    };
    http::accept_loop(listener, http::router(shared), shutdown).await;
    if let Some(reporter) = reporter {
        reporter.abort();
    }
}

/// What the counts report needs.
struct CountsReport<'a, C> {
    metrics: &'a Metrics,
    clock: &'a C,
    timer: &'a dyn Timer,
    interval: Duration,
}

impl<C: Clock> CountsReport<'_, C> {
    /// Writes the totals every `interval`, forever.
    async fn run(self, mut write: impl FnMut(&str)) {
        loop {
            self.timer.sleep(self.interval).await;
            write(&format!(
                "pingers-server counts at {}\n{}",
                self.clock.now().get(),
                self.metrics.render()
            ));
        }
    }
}

#[expect(
    clippy::print_stderr,
    reason = "counts logging is the server's only output, and only when the operator enables it"
)]
fn write_counts(report: &str) {
    eprint!("{report}");
}

#[cfg(test)]
mod tests;
