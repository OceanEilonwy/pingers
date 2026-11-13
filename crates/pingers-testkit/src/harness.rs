// SPDX-License-Identifier: AGPL-3.0-only
//! The in-process server harness: pingers-server, an OHTTP relay and a push relay on
//! ephemeral loopback ports, for end-to-end tests and pingers-cli.
//!
//! # Feature `harness`
//!
//! With the feature, [`InProcessHarness::start`] binds three listeners on
//! [`HarnessConfig::bind`], generates the server's OHTTP key and the push relay's HPKE key from
//! [`HarnessConfig::seed`], and runs the three services on a multi-thread tokio runtime of their
//! own thread (so the harness works from sync and async tests alike, and its runtime is never
//! dropped inside another). The server allows the OHTTP relay as its only relay; the relay
//! forwards to the server's gateway; the push relay opens sealed tokens with its real key and
//! captures each push ([`InProcessHarness::pushes`]) instead of sending it. All three read
//! [`HarnessConfig::clock`]. [`InProcessHarness::shutdown`] (or dropping the harness) stops
//! them and waits for the thread.
//!
//! Without the feature `start` returns [`HarnessError::NotWired`], so crates that only need
//! the fake platform do not compile the server.
//!
//! # Known gaps
//!
//! - **The OHTTP relay is a stand-in.** pingers-ohttp-relay has no library target yet, so the
//!   harness runs a forward-only relay of its own that behaves the same way (strips the client,
//!   streams both directions). TODO(wave 2 follow-up): call `pingers_ohttp_relay::serve` once
//!   it exists.
//! - **The relay-only rule cannot bite on loopback**: relay and test clients connect from the
//!   same address. pingers-server's own tests check the rule with two loopback addresses.
//! - **The server does not forward pushes yet**: the wake rule belongs to pingers-mailbox and
//!   pingers-delivery, which have no API on main. Once they do, the server is configured with
//!   [`Endpoints::push_relay`] and the harness's credential.
//! - **Most routes answer 501** until the four server crates are wired (pingers-server docs).

#[cfg(feature = "harness")]
mod relay;
#[cfg(feature = "harness")]
mod wiring;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use pingers_proto::{PushClass, PushPlatform, PushToken};
use pingers_time::{ManualClock, UnixSeconds};

use crate::time::test_clock;

/// How to start the harness.
#[derive(Clone, Debug)]
pub struct HarnessConfig {
    /// Wall and monotonic time for all three services. Share it with the clients' fakes so
    /// leases, buckets and expiries agree. Request timeouts run on it too, so a test that
    /// advances it past 30 seconds while a request body is still arriving times that request
    /// out.
    pub clock: ManualClock,
    /// Seed for every key the harness generates (OHTTP key config, push relay key, Privacy
    /// Pass issuer key), so runs are reproducible.
    pub seed: u64,
    /// The address to bind; each service gets an ephemeral port on it. Use a concrete address
    /// (the services connect to each other on it), not `0.0.0.0`.
    pub bind: IpAddr,
}

impl Default for HarnessConfig {
    /// The [test clock](crate::test_clock), seed 0, loopback.
    fn default() -> Self {
        Self {
            clock: test_clock(),
            seed: 0,
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        }
    }
}

/// Where the running services listen and the keys a client must pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// pingers-server: identified endpoints directly, anonymous ones only via the relay.
    pub server: SocketAddr,
    /// The OHTTP relay clients send anonymous requests to (`POST /ohttp`, and
    /// `GET /v1/ohttp/config` for the key config).
    pub ohttp_relay: SocketAddr,
    /// The push relay the server forwards sealed pushes to.
    pub push_relay: SocketAddr,
    /// The server's OHTTP key configuration (RFC 9458 `application/ohttp-keys`, one config).
    pub ohttp_key_config: Vec<u8>,
    /// The push relay's X25519 HPKE public key (key id 1), which clients seal push tokens to.
    pub push_relay_key: Vec<u8>,
}

/// A push the push relay would have delivered to FCM or a UnifiedPush endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedPush {
    /// The push service.
    pub platform: PushPlatform,
    /// The class (Standard or Location).
    pub class: PushClass,
    /// The device token it was addressed to.
    pub token: PushToken,
    /// When the relay sent it, by the harness clock.
    pub sent_at: UnixSeconds,
}

/// Which service could not be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Service {
    /// pingers-server.
    Server,
    /// pingers-ohttp-relay.
    OhttpRelay,
    /// pingers-push-relay.
    PushRelay,
}

/// Why the harness could not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HarnessError {
    /// The testkit was built without the `harness` feature.
    #[error(
        "{service:?} is not wired into the in-process harness (enable the testkit's `harness` feature)"
    )]
    NotWired {
        /// The first service that could not be started.
        service: Service,
    },
    /// Binding an ephemeral port failed.
    #[error("cannot bind an ephemeral port for {service:?}")]
    Bind {
        /// The service.
        service: Service,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A service's keys or configuration could not be made.
    #[error("cannot set up the keys or configuration of {service:?}")]
    Keys {
        /// The service.
        service: Service,
    },
    /// The harness runtime or its thread could not be started.
    #[error("cannot start the harness runtime")]
    Runtime {
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

/// The running server and relays. Dropping it shuts them down.
#[derive(Debug)]
pub struct InProcessHarness {
    endpoints: Endpoints,
    pushes: Arc<Mutex<Vec<CapturedPush>>>,
    #[cfg(feature = "harness")]
    running: wiring::Running,
}

impl InProcessHarness {
    /// Starts the server, the OHTTP relay and the push relay on ephemeral ports.
    ///
    /// # Errors
    ///
    /// [`HarnessError::NotWired`] without the `harness` feature; otherwise
    /// [`HarnessError::Bind`] if a port cannot be bound, [`HarnessError::Keys`] or
    /// [`HarnessError::Runtime`].
    #[cfg(feature = "harness")]
    pub fn start(config: &HarnessConfig) -> Result<Self, HarnessError> {
        let pushes = Arc::new(Mutex::new(Vec::new()));
        let started = wiring::start(config, Arc::clone(&pushes))?;
        Ok(Self {
            endpoints: started.endpoints,
            pushes,
            running: started.running,
        })
    }

    /// Starts the server, the OHTTP relay and the push relay on ephemeral ports.
    ///
    /// # Errors
    ///
    /// Always [`HarnessError::NotWired`] for [`Service::Server`]: this build has no `harness`
    /// feature.
    #[cfg(not(feature = "harness"))]
    pub fn start(_config: &HarnessConfig) -> Result<Self, HarnessError> {
        Err(HarnessError::NotWired {
            service: Service::Server,
        })
    }

    /// Where the services listen and the keys to pin.
    #[must_use]
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// The pushes the push relay sent so far, oldest first: a snapshot, since pushes arrive
    /// on the harness's own thread.
    #[must_use]
    pub fn pushes(&self) -> Vec<CapturedPush> {
        self.pushes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Stops all three services and waits for them to finish.
    pub fn shutdown(self) {
        drop(self);
    }
}

#[cfg(feature = "harness")]
impl Drop for InProcessHarness {
    fn drop(&mut self) {
        self.running.stop();
    }
}

/// Runs the harness's OHTTP relay on `listener`, forwarding to the gateway at `gateway`, until
/// `shutdown` completes: for tests that need a relay in front of a server they started
/// themselves (for example with an injected live channel).
#[cfg(feature = "harness")]
pub async fn run_ohttp_relay(
    listener: tokio::net::TcpListener,
    gateway: SocketAddr,
    shutdown: impl Future<Output = ()>,
) {
    relay::serve(listener, gateway, shutdown).await;
}

#[cfg(test)]
mod tests;
