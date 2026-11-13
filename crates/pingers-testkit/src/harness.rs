// SPDX-License-Identifier: AGPL-3.0-only
//! The in-process server harness: pingers-server, an OHTTP relay and a push relay on
//! ephemeral loopback ports, for end-to-end tests and pingers-cli.
//!
//! # Status: API fixed, wiring pending
//!
//! The three binaries are stubs today and expose no library entry point, so
//! [`InProcessHarness::start`] returns [`HarnessError::NotWired`]. The API below is what the
//! end-to-end tests and pingers-cli program against; the wiring replaces the body of `start`
//! without changing it.
//!
//! ## TODO (issue): wire the in-process harness
//!
//! - **Blocked on:** each of pingers-server, pingers-ohttp-relay and pingers-push-relay
//!   exposing a library target with an entry point shaped like
//!   `pub async fn serve(config: Config, listener: tokio::net::TcpListener, clock: Arc<dyn Clock>, shutdown: impl Future)`,
//!   so the harness can bind `127.0.0.1:0` itself and pass the listener in (no port races)
//!   and inject the [`ManualClock`]. The push relay additionally needs its outbound sender
//!   (FCM, UnifiedPush) behind a trait so the harness can capture pushes instead of sending.
//! - **Work:** start a dedicated multi-thread tokio runtime on its own thread (so the harness
//!   works from both sync and async tests and dropping it never happens inside a runtime);
//!   bind three listeners; start the server with the OHTTP relay as its only allowed relay;
//!   start the OHTTP relay pointing at the server's gateway; start the push relay with a
//!   capturing sender; generate the server's OHTTP key config and the push relay's HPKE key
//!   from [`HarnessConfig::seed`] and publish both in [`Endpoints`] so clients can pin them;
//!   shut everything down on [`InProcessHarness::shutdown`] and on drop.
//! - **Done when:** G.3 to G.7 run headless through pingers-cli against the harness
//!   (architecture §6 step 4), and `start` no longer returns `NotWired`.
//! - **Adds testkit dependencies:** tokio (dev crate, allowed), the three binaries as
//!   libraries. Keep them behind a `harness` feature so crates that only need the fake platform
//!   do not compile the server.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use pingers_proto::{PushClass, PushPlatform, PushToken};
use pingers_time::{ManualClock, UnixSeconds};

use crate::time::test_clock;

/// How to start the harness.
#[derive(Clone, Debug)]
pub struct HarnessConfig {
    /// Wall and monotonic time for all three services. Share it with the clients' fakes so
    /// leases, buckets and expiries agree.
    pub clock: ManualClock,
    /// Seed for every key the harness generates (OHTTP key config, push relay key, Privacy
    /// Pass issuer key), so runs are reproducible.
    pub seed: u64,
    /// The address to bind; each service gets an ephemeral port on it.
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
    /// The OHTTP relay clients send anonymous requests to.
    pub ohttp_relay: SocketAddr,
    /// The push relay the server forwards sealed pushes to.
    pub push_relay: SocketAddr,
    /// The server's OHTTP key configuration (RFC 9458 `application/ohttp-keys`).
    pub ohttp_key_config: Vec<u8>,
    /// The push relay's HPKE public key, which clients seal push tokens to.
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
    /// The service has no library entry point yet (see the module docs' TODO).
    #[error("{service:?} is not wired into the in-process harness yet")]
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
}

/// The running server and relays. Dropping it shuts them down.
#[derive(Debug)]
pub struct InProcessHarness {
    endpoints: Endpoints,
    pushes: Vec<CapturedPush>,
}

impl InProcessHarness {
    /// Starts the server, the OHTTP relay and the push relay on ephemeral ports.
    ///
    /// # Errors
    ///
    /// Today always [`HarnessError::NotWired`] for [`Service::Server`]; once wired,
    /// [`HarnessError::Bind`] if a port cannot be bound.
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

    /// Pushes the push relay sent so far, oldest first.
    #[must_use]
    pub fn pushes(&self) -> &[CapturedPush] {
        &self.pushes
    }

    /// Stops all three services and waits for them to finish.
    pub fn shutdown(self) {
        drop(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_reports_the_first_unwired_service() {
        assert!(matches!(
            InProcessHarness::start(&HarnessConfig::default()),
            Err(HarnessError::NotWired {
                service: Service::Server
            })
        ));
    }

    #[test]
    fn default_config_binds_loopback_at_the_test_start() {
        let config = HarnessConfig::default();
        assert_eq!(config.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(pingers_time::Clock::now(&config.clock), crate::TEST_START);
        assert_eq!(config.seed, 0);
    }
}
