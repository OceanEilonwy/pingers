// SPDX-License-Identifier: AGPL-3.0-only
//! Starting and stopping the three services on a runtime of their own.

use std::net::{SocketAddr, TcpListener};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pingers_crypto::{PushRelayPrivateKey, PushRelayPublicKey};
use pingers_ohttp::GatewayKey;
use pingers_proto::{PushClass, PushPlatform, PushToken, PushTokenPlaintext, SealedPushBlob};
use pingers_push_relay::credentials::{ServerCredential, ServerEntry, ServerRegistry};
use pingers_push_relay::delivery::{DeliveryOutcome, PushMessage, PushSender};
use pingers_push_relay::keys::{KeyLifetime, KeyRing, RelayKey};
use pingers_push_relay::opener::{CryptoOpener, OpenError, TokenOpener};
use pingers_push_relay::rate::RateLimit;
use pingers_push_relay::service::{Relay, RelayParts};
use pingers_server::{
    AddressList, DeliveryLive, Logging, OhttpKeys, Outside, RelayAllowList, RngSeed, ServerConfig,
};
use pingers_time::{Clock as _, ManualClock, UnixSeconds};
use tokio::sync::watch;
use zeroize::Zeroizing;

use super::{CapturedPush, Endpoints, HarnessConfig, HarnessError, Service};
use crate::rng::{random_array, seeded_rng};

/// How long a request body may take to arrive, on the harness clock.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The bearer credential pingers-server will present to the push relay.
pub(super) const PUSH_CREDENTIAL: &[u8] = b"pingers-testkit-harness-push-credential";

/// The running services: a thread driving their runtime, and the signal that stops them.
#[derive(Debug)]
pub(super) struct Running {
    stop: watch::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Running {
    /// Signals every service to stop and waits for the runtime thread to finish.
    pub(super) fn stop(&mut self) {
        // Receivers see the send, or the sender's drop below if the send finds none.
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            // A service that panicked has already failed the test that used it.
            let _ = thread.join();
        }
    }
}

/// Everything [`start`] returns.
pub(super) struct Started {
    pub(super) endpoints: Endpoints,
    pub(super) running: Running,
}

/// Which pushes went to which class: the relay's sender is told the platform and token but
/// not the class, so the opener notes it.
type Classes = Arc<Mutex<Vec<NotedClass>>>;

struct NotedClass {
    token: PushToken,
    class: PushClass,
}

/// The real HPKE opener, noting each opened token's class for the capturing sender.
struct NotingOpener {
    classes: Classes,
}

impl TokenOpener for NotingOpener {
    fn open(
        &self,
        key: &PushRelayPrivateKey,
        bind: &pingers_crypto::PushBind,
        blob: &SealedPushBlob,
    ) -> Result<PushTokenPlaintext, OpenError> {
        let opened = CryptoOpener.open(key, bind, blob)?;
        let mut classes = self.classes.lock().unwrap_or_else(PoisonError::into_inner);
        classes.retain(|noted| noted.token != opened.token);
        classes.push(NotedClass {
            token: opened.token.clone(),
            class: opened.class,
        });
        Ok(opened)
    }
}

/// Captures every push instead of sending it.
struct CapturingSender {
    classes: Classes,
    pushes: Arc<Mutex<Vec<CapturedPush>>>,
    clock: ManualClock,
}

impl PushSender for CapturingSender {
    fn supports(&self, _platform: PushPlatform) -> bool {
        true
    }

    fn send(
        &self,
        platform: PushPlatform,
        token: &PushToken,
        _message: &PushMessage,
    ) -> impl Future<Output = DeliveryOutcome> + Send {
        let class = self
            .classes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|noted| noted.token == *token)
            .map_or(PushClass::Standard, |noted| noted.class);
        self.pushes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(CapturedPush {
                platform,
                class,
                token: token.clone(),
                sent_at: self.clock.now(),
            });
        std::future::ready(DeliveryOutcome::Delivered)
    }
}

fn bind(config: &HarnessConfig, service: Service) -> Result<TcpListener, HarnessError> {
    let listener = TcpListener::bind(SocketAddr::new(config.bind, 0))
        .map_err(|source| HarnessError::Bind { service, source })?;
    listener
        .set_nonblocking(true)
        .map_err(|source| HarnessError::Bind { service, source })?;
    Ok(listener)
}

fn address(listener: &TcpListener, service: Service) -> Result<SocketAddr, HarnessError> {
    listener
        .local_addr()
        .map_err(|source| HarnessError::Bind { service, source })
}

fn per_minute(count: u32) -> RateLimit {
    RateLimit::per_minute(NonZeroU32::new(count).unwrap_or(NonZeroU32::MIN))
}

fn push_relay(
    private: PushRelayPrivateKey,
    public: PushRelayPublicKey,
    clock: &ManualClock,
    pushes: Arc<Mutex<Vec<CapturedPush>>>,
) -> Result<Relay<NotingOpener, CapturingSender, ManualClock>, HarnessError> {
    let keys = KeyRing::new(vec![RelayKey::new(
        private,
        public,
        KeyLifetime {
            activated_at: UnixSeconds::EPOCH,
            retired_at: None,
        },
    )])
    .map_err(|_| HarnessError::Keys {
        service: Service::PushRelay,
    })?;
    let servers = ServerRegistry::new(vec![ServerEntry {
        server_id: "harness".to_owned(),
        credential: ServerCredential::new(Zeroizing::new(PUSH_CREDENTIAL.to_vec())),
        limit: per_minute(10_000),
    }])
    .map_err(|_| HarnessError::Keys {
        service: Service::PushRelay,
    })?;
    let classes = Classes::default();
    Ok(Relay::new(RelayParts {
        opener: NotingOpener {
            classes: Arc::clone(&classes),
        },
        sender: CapturingSender {
            classes,
            pushes,
            clock: clock.clone(),
        },
        clock: clock.clone(),
        keys,
        servers,
        token_limit: per_minute(600),
        max_ttl: Duration::from_hours(30 * 24),
        operator: "pingers-testkit harness".to_owned(),
    }))
}

/// Binds, generates keys, and starts the three services on their own runtime thread.
pub(super) fn start(
    config: &HarnessConfig,
    pushes: Arc<Mutex<Vec<CapturedPush>>>,
) -> Result<Started, HarnessError> {
    let server_listener = bind(config, Service::Server)?;
    let relay_listener = bind(config, Service::OhttpRelay)?;
    let push_listener = bind(config, Service::PushRelay)?;
    let server = address(&server_listener, Service::Server)?;

    let mut rng = seeded_rng(config.seed);
    let gateway_key = GatewayKey::generate(1, &mut rng).map_err(|_| HarnessError::Keys {
        service: Service::Server,
    })?;
    let ohttp_key_config = gateway_key.config().encoded().to_vec();
    let push_private = PushRelayPrivateKey::from_bytes(Zeroizing::new(random_array(&mut rng)));
    let push_public = push_private.public_key(1).map_err(|_| HarnessError::Keys {
        service: Service::PushRelay,
    })?;
    let seed = RngSeed::from_bytes(Zeroizing::new(random_array(&mut rng)));

    let endpoints = Endpoints {
        server,
        ohttp_relay: address(&relay_listener, Service::OhttpRelay)?,
        push_relay: address(&push_listener, Service::PushRelay)?,
        ohttp_key_config,
        push_relay_key: push_public.public_key().to_vec(),
    };
    let keys_error = |_| HarnessError::Keys {
        service: Service::Server,
    };
    let server_config = ServerConfig {
        listen: server,
        logging: Logging::Off,
        request_timeout: REQUEST_TIMEOUT,
        ohttp_keys: OhttpKeys::new(vec![gateway_key]).map_err(keys_error)?,
        // On loopback the relay's connections come from the bind address, which is also where
        // test clients come from: the relay-only rule cannot tell them apart here.
        relays: RelayAllowList::new(vec![config.bind]).map_err(keys_error)?,
        metrics: AddressList::new(vec![config.bind]),
    };
    let push = push_relay(push_private, push_public, &config.clock, pushes)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("pingers-harness")
        .enable_all()
        .build()
        .map_err(|source| HarnessError::Runtime { source })?;
    let (server_listener, relay_listener, push_listener) = {
        let _entered = runtime.enter();
        let tokio_listener = |listener, service| {
            tokio::net::TcpListener::from_std(listener)
                .map_err(|source| HarnessError::Bind { service, source })
        };
        (
            tokio_listener(server_listener, Service::Server)?,
            tokio_listener(relay_listener, Service::OhttpRelay)?,
            tokio_listener(push_listener, Service::PushRelay)?,
        )
    };

    let (stop, stopped) = watch::channel(());
    let until_stopped = move || {
        let mut stopped = stopped.clone();
        async move {
            // Either a stop signal or the harness being gone.
            let _ = stopped.changed().await;
        }
    };
    let outside = Outside {
        clock: config.clock.clone(),
        timer: config.clock.clone(),
        seed,
        live: DeliveryLive,
    };
    let server_done = until_stopped();
    let relay_done = until_stopped();
    let push_done = until_stopped();
    let thread = std::thread::Builder::new()
        .name("pingers-harness".to_owned())
        .spawn(move || {
            runtime.block_on(async move {
                let server_task = pingers_server::serve_with(
                    server_config,
                    outside,
                    server_listener,
                    server_done,
                );
                let relay = super::relay::serve(relay_listener, server, relay_done);
                let push = async move {
                    let router = pingers_push_relay::http::router(Arc::new(push));
                    // A push relay that fails has already failed the test that used it.
                    let _ = axum::serve(push_listener, router)
                        .with_graceful_shutdown(push_done)
                        .await;
                };
                tokio::join!(server_task, relay, push);
            });
        })
        .map_err(|source| HarnessError::Runtime { source })?;
    Ok(Started {
        endpoints,
        running: Running {
            stop,
            thread: Some(thread),
        },
    })
}
