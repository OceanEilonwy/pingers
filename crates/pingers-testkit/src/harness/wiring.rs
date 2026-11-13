// SPDX-License-Identifier: AGPL-3.0-only
//! Starting and stopping the three services on a runtime of their own.

use std::net::{SocketAddr, TcpListener};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pingers_crypto::{KeyedHashKey, KeyedHashPurpose, PushRelayPrivateKey, keyed_hash};
use pingers_mailbox::{ListedRelays, RelayCredential, RelayTarget};
use pingers_ohttp::GatewayKey;
use pingers_proto::{
    PushClass, PushPlatform, PushRelayKeyId, PushToken, PushTokenPlaintext, SealedPushBlob,
    TimeToLive,
};
use pingers_proto::{RelayUrl, ServerId};
use pingers_push_relay::credentials::{ServerEntry, ServerRegistry};
use pingers_push_relay::delivery::{DeliveryOutcome, PushMessage, PushSender};
use pingers_push_relay::keys::{KeyLifetime, KeyRing, RelayKey};
use pingers_push_relay::opener::{CryptoOpener, OpenError, TokenOpener};
use pingers_push_relay::rate::RateLimit;
use pingers_push_relay::service::{Relay, RelayParts};
use pingers_server::{
    AddressList, CertificateSource, DeliveryLive, OhttpKeys, Outside, RelayAllowList, Required,
    RngSeed, ServerConfig,
};
use pingers_time::{Clock as _, ManualClock, UnixSeconds};
use tokio::sync::watch;
use zeroize::Zeroizing;

use super::{CapturedPush, Endpoints, HARNESS_PUSH_RELAY_ID, HarnessConfig, HarnessError, Service};
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
        key: &RelayKey,
        bind: &pingers_proto::PushBind,
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

/// Where the push relay's sender records pushes, and the tokens it answers 410 for.
pub(super) struct Capture {
    pub(super) pushes: Arc<Mutex<Vec<CapturedPush>>>,
    pub(super) gone: Arc<Mutex<Vec<PushToken>>>,
    pub(super) gone_pushes: Arc<Mutex<Vec<CapturedPush>>>,
}

/// Captures every push instead of sending it.
struct CapturingSender {
    classes: Classes,
    capture: Capture,
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
        let gone = self
            .capture
            .gone
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(token);
        let class = self
            .classes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|noted| noted.token == *token)
            .map_or(PushClass::Standard, |noted| noted.class);
        let push = CapturedPush {
            platform,
            class,
            token: token.clone(),
            sent_at: self.clock.now(),
        };
        let (list, outcome) = if gone {
            (&self.capture.gone_pushes, DeliveryOutcome::TokenGone)
        } else {
            (&self.capture.pushes, DeliveryOutcome::Delivered)
        };
        list.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(push);
        std::future::ready(outcome)
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

/// What the push relay is built from, all derived from the harness seed.
struct PushRelayKeys {
    private: PushRelayPrivateKey,
    credential_hash_key: KeyedHashKey,
    token_hash_key: KeyedHashKey,
}

fn push_relay(
    keys: PushRelayKeys,
    clock: &ManualClock,
    capture: Capture,
) -> Result<Relay<NotingOpener, CapturingSender, ManualClock>, HarnessError> {
    let failed = |_| HarnessError::Keys {
        service: Service::PushRelay,
    };
    let PushRelayKeys {
        private,
        credential_hash_key,
        token_hash_key,
    } = keys;
    let relay_key = RelayKey::new(
        PushRelayKeyId::new(1),
        private,
        KeyLifetime {
            activated_at: UnixSeconds::EPOCH,
            retired_at: None,
        },
    )
    .map_err(|_| HarnessError::Keys {
        service: Service::PushRelay,
    })?;
    let ring = KeyRing::new(vec![relay_key]).map_err(failed)?;
    // The relay keeps only the keyed hash of the credential the server presents.
    let credential_hash = keyed_hash(
        &credential_hash_key,
        KeyedHashPurpose::ServerCredential,
        PUSH_CREDENTIAL,
    )
    .map_err(|_| HarnessError::Keys {
        service: Service::PushRelay,
    })?;
    let servers = ServerRegistry::new(
        credential_hash_key,
        vec![ServerEntry {
            server_id: "harness".to_owned(),
            credential_hash,
            limit: per_minute(10_000),
        }],
    )
    .map_err(failed)?;
    let max_ttl =
        TimeToLive::new(Duration::from_hours(30 * 24)).map_err(|_| HarnessError::Keys {
            service: Service::PushRelay,
        })?;
    let classes = Classes::default();
    Ok(Relay::new(RelayParts {
        opener: NotingOpener {
            classes: Arc::clone(&classes),
        },
        sender: CapturingSender {
            classes,
            capture,
            clock: clock.clone(),
        },
        clock: clock.clone(),
        keys: ring,
        servers,
        token_hash_key,
        token_limit: per_minute(600),
        max_ttl,
        operator: "pingers-testkit harness".to_owned(),
    }))
}

/// Binds, generates keys, and starts the three services on their own runtime thread.
pub(super) fn start(config: &HarnessConfig, capture: Capture) -> Result<Started, HarnessError> {
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
    let credential_hash_key = KeyedHashKey::from_bytes(Zeroizing::new(random_array(&mut rng)));
    let token_hash_key = KeyedHashKey::from_bytes(Zeroizing::new(random_array(&mut rng)));

    let push_address = address(&push_listener, Service::PushRelay)?;
    let tls = super::tls::test_tls(config.bind).map_err(|_| HarnessError::Keys {
        service: Service::PushRelay,
    })?;
    let server_id = format!("https://{server}");
    let endpoints = Endpoints {
        server,
        ohttp_relay: address(&relay_listener, Service::OhttpRelay)?,
        push_relay: push_address,
        push_relay_ca: tls.ca_pem.clone(),
        server_id: server_id.clone(),
        ohttp_key_config,
        push_relay_key: push_public.public_key().to_vec(),
    };
    let keys_error = |_| HarnessError::Keys {
        service: Service::Server,
    };
    let push_url =
        RelayUrl::new(format!("https://{push_address}")).map_err(|_| HarnessError::Keys {
            service: Service::PushRelay,
        })?;
    let credential =
        String::from_utf8(PUSH_CREDENTIAL.to_vec()).map_err(|_| HarnessError::Keys {
            service: Service::PushRelay,
        })?;
    // On loopback the relay's connections come from the bind address, which is also where
    // test clients come from: the relay-only rule cannot tell them apart here. The push relay
    // is HTTPS with the test CA's certificate, which the server is told to trust.
    let server_config = ServerConfig {
        request_timeout: REQUEST_TIMEOUT,
        metrics: AddressList::new(vec![config.bind]),
        push_relays: ListedRelays::new([(
            HARNESS_PUSH_RELAY_ID,
            RelayTarget {
                url: push_url,
                credential: RelayCredential::new(credential),
            },
        )]),
        push_relay_ca: Some(CertificateSource::Pem(tls.ca_pem.clone().into_bytes())),
        ..ServerConfig::new(Required {
            listen: server,
            server_id: ServerId::new(server_id).map_err(|_| HarnessError::Keys {
                service: Service::Server,
            })?,
            ohttp_keys: OhttpKeys::new(vec![gateway_key]).map_err(keys_error)?,
            relays: RelayAllowList::new(vec![config.bind]).map_err(keys_error)?,
        })
    };
    let push = push_relay(
        PushRelayKeys {
            private: push_private,
            credential_hash_key,
            token_hash_key,
        },
        &config.clock,
        capture,
    )?;
    let outside = Outside {
        clock: config.clock.clone(),
        timer: config.clock.clone(),
        seed,
        live: DeliveryLive,
    };
    let running = spawn(
        Listeners {
            server: server_listener,
            relay: relay_listener,
            push: push_listener,
        },
        server_config,
        outside,
        PushService {
            relay: push,
            tls: tls.acceptor,
        },
    )?;
    Ok(Started { endpoints, running })
}

/// The three bound listeners.
struct Listeners {
    server: TcpListener,
    relay: TcpListener,
    push: TcpListener,
}

type HarnessOutside = Outside<ManualClock, ManualClock, DeliveryLive>;
type HarnessPushRelay = Relay<NotingOpener, CapturingSender, ManualClock>;

/// The push relay and its TLS acceptor.
struct PushService {
    relay: HarnessPushRelay,
    tls: tokio_rustls::TlsAcceptor,
}

/// Runs the three services on a runtime of their own thread.
fn spawn(
    listeners: Listeners,
    server_config: ServerConfig,
    outside: HarnessOutside,
    push: PushService,
) -> Result<Running, HarnessError> {
    let server = address(&listeners.server, Service::Server)?;
    let Listeners {
        server: server_listener,
        relay: relay_listener,
        push: push_listener,
    } = listeners;
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
                    let router = pingers_push_relay::http::router(Arc::new(push.relay));
                    super::tls::serve(push_listener, push.tls, router, push_done).await;
                };
                // A server that fails to start has already failed the test that used it.
                let (_served, (), ()) = tokio::join!(server_task, relay, push);
            });
        })
        .map_err(|source| HarnessError::Runtime { source })?;
    Ok(Running {
        stop,
        thread: Some(thread),
    })
}
