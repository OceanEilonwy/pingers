// SPDX-License-Identifier: AGPL-3.0-only
//! Push relay.
//!
//! The publisher-run push relay (design B.4.9, B.7, C.1a, D). Stateless: open the sealed token
//! with the relay's HPKE key under the mailbox bind, check expiry, and send one content-free
//! push (`{"v":1,"h":"<handle>"}`) by UnifiedPush or FCM HTTP v1. Per-token and per-server
//! rate limits, a bearer credential per Pingers server, a key config endpoint, key rotation
//! with an overlap window, and logging off by default (counts only when on). No database.
//!
//! What it never holds: mailbox or group ids (only the bind hash), accounts, device IP
//! addresses, message content. What it learns, as design A.2 says: that token T was woken at
//! time t, with what class, on behalf of which server.
#![forbid(unsafe_code)]

pub mod config;
pub mod counts;
pub mod credentials;
pub mod delivery;
pub mod error;
pub mod http;
pub mod keys;
pub mod opener;
pub mod rate;
pub mod request;
pub mod service;

#[cfg(test)]
mod mock_http;

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use pingers_time::Clock;
use pingers_transport::TokioTimer;
use zeroize::Zeroizing;

use crate::config::{Logging, RelayConfig};
use crate::counts::{CountsSnapshot, report_counts};
use crate::delivery::{Dispatcher, DispatcherSettings, FcmAccessToken, FcmSettings, PushSender};
use crate::error::{ConfigError, RelayError};
use crate::opener::CryptoOpener;
use crate::service::{Relay, RelayParts};

/// The HTTP client for push services. It never follows redirects: a redirect from a
/// device-chosen UnifiedPush endpoint would get around the endpoint policy.
///
/// # Errors
///
/// [`RelayError::HttpClient`] if the TLS backend cannot be set up.
pub fn http_client() -> Result<reqwest::Client, RelayError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(RelayError::HttpClient)
}

fn read_secret(path: &Path) -> Result<Zeroizing<String>, RelayError> {
    std::fs::read_to_string(path)
        .map(Zeroizing::new)
        .map_err(|source| RelayError::ReadFile {
            path: path.to_owned(),
            source,
        })
}

/// Reads and checks the configuration file at `path`.
///
/// # Errors
///
/// [`RelayError::ReadFile`] or [`RelayError::Config`].
pub fn load_config(path: &Path) -> Result<RelayConfig, RelayError> {
    let text = read_secret(path)?;
    RelayConfig::parse(&text).map_err(RelayError::Config)
}

/// Opens the listening socket named in the configuration.
///
/// # Errors
///
/// [`RelayError::Listen`] if the address cannot be bound.
pub async fn listen(config: &RelayConfig) -> Result<tokio::net::TcpListener, RelayError> {
    tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|source| RelayError::Listen {
            address: config.listen,
            source,
        })
}

/// The production [`PushSender`]: UnifiedPush and FCM HTTP v1 over a non-redirecting client,
/// as the configuration enables them. Must be called inside a tokio runtime.
///
/// # Errors
///
/// [`RelayError::ReadFile`] if the FCM access token file cannot be read,
/// [`RelayError::HttpClient`] if the client cannot be built.
pub fn dispatcher(config: &RelayConfig) -> Result<Dispatcher<TokioTimer>, RelayError> {
    let fcm = match &config.fcm {
        Some(fcm) => {
            let token = read_secret(&fcm.access_token_file)?;
            Some(
                FcmSettings::new(&fcm.project_id, &fcm.endpoint, FcmAccessToken::new(&token))
                    .ok_or(RelayError::Config(ConfigError::BadFcmEndpoint))?,
            )
        }
        None => None,
    };
    Ok(Dispatcher::new(
        http_client()?,
        TokioTimer::new(),
        DispatcherSettings {
            timeout: config.upstream_timeout,
            unifiedpush: config.unifiedpush,
            fcm,
        },
    ))
}

/// Runs the relay on `listener` with the real HPKE opener and the HTTP [`dispatcher`] until
/// `shutdown` completes. The binary passes [`pingers_time::SystemClock`]; tests pass a manual
/// clock. Must be called inside a tokio runtime.
///
/// # Errors
///
/// As [`dispatcher`] and [`serve_with`].
pub async fn serve(
    config: RelayConfig,
    listener: tokio::net::TcpListener,
    clock: impl Clock + 'static,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), RelayError> {
    let sender = dispatcher(&config)?;
    serve_with(config, Outside { sender, clock }, listener, shutdown).await
}

/// The relay's view of the outside world, injected into [`serve_with`].
pub struct Outside<S, C> {
    /// Delivers pushes: the HTTP [`dispatcher`], or a capturing sender in tests.
    pub sender: S,
    /// Wall time for expiry, key rotation and rate limits.
    pub clock: C,
}

/// As [`serve`], with an injected [`PushSender`] in place of real HTTP delivery: for in-process
/// harnesses that capture pushes instead of sending them.
///
/// # Errors
///
/// [`RelayError::NoActiveKey`] if no key is active at the injected clock's time, or
/// [`RelayError::Serve`] if the HTTP server fails.
pub async fn serve_with<S: PushSender, C: Clock + 'static>(
    config: RelayConfig,
    outside: Outside<S, C>,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), RelayError> {
    let Outside { sender, clock } = outside;
    if config.keys.published(clock.now()).is_none() {
        return Err(RelayError::NoActiveKey);
    }
    let relay = Arc::new(Relay::new(RelayParts {
        opener: CryptoOpener,
        sender,
        clock,
        keys: config.keys,
        servers: config.servers,
        token_hash_key: config.token_hash_key,
        token_limit: config.token_limit,
        max_ttl: config.max_ttl,
        operator: config.operator,
    }));
    let reporter = match config.logging {
        Logging::Off => None,
        Logging::Counts { interval } => {
            let relay = Arc::clone(&relay);
            Some(tokio::spawn(async move {
                let timer = TokioTimer::new();
                report_counts(relay.counts(), &timer, interval, write_counts).await;
            }))
        }
    };
    let served = axum::serve(listener, http::router(relay))
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(RelayError::Serve);
    if let Some(reporter) = reporter {
        reporter.abort();
    }
    served
}

#[expect(
    clippy::print_stderr,
    reason = "counts logging is the relay's only output, and only when the operator enables it"
)]
fn write_counts(snapshot: CountsSnapshot) {
    eprintln!("pingers-push-relay counts: {snapshot}");
}

#[cfg(test)]
mod tests;
