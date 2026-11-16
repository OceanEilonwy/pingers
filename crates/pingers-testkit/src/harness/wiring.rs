// SPDX-License-Identifier: AGPL-3.0-only
//! Starting and stopping the three services on a runtime of their own.

use std::net::{SocketAddr, TcpListener};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pingers_crypto::{
    KeyedHashKey, KeyedHashPurpose, PushRelayPrivateKey, PushRelayPublicKey, keyed_hash,
};
use pingers_mailbox::{ListedRelays, RelayCredential, RelayTarget};
use pingers_ohttp::GatewayKey;
use pingers_proto::{
    PushClass, PushPlatform, PushRelayKeyId, PushToken, PushTokenPlaintext, SealedPushBlob,
    TimeToLive,
};
use pingers_proto::{RelayUrl, SecretText, ServerId};
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

use super::{
    CapturedPush, Endpoints, HARNESS_MAP_HOST, HARNESS_PUSH_RELAY_ID, HarnessConfig, HarnessError,
    Service,
};
use crate::rng::{random_array, seeded_rng};

/// How long a request body may take to arrive, on the harness clock.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The bearer credential pingers-server will present to the push relay.
pub(super) const PUSH_CREDENTIAL: &str = "pingers-testkit-harness-push-credential";

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
    /// A second key and when it is published.
    next: Option<(PushRelayPrivateKey, UnixSeconds)>,
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
        next,
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
    let mut keys = vec![relay_key];
    if let Some((next_private, activated_at)) = next {
        keys.push(
            RelayKey::new(
                PushRelayKeyId::new(2),
                next_private,
                KeyLifetime {
                    activated_at,
                    retired_at: None,
                },
            )
            .map_err(|_| HarnessError::Keys {
                service: Service::PushRelay,
            })?,
        );
    }
    let ring = KeyRing::new(keys).map_err(failed)?;
    // The relay keeps only the keyed hash of the credential the server presents.
    let credential_hash = keyed_hash(
        &credential_hash_key,
        KeyedHashPurpose::ServerCredential,
        PUSH_CREDENTIAL.as_bytes(),
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

/// The push relay as `/v1/params` advertises it, as an operator's configuration would, so
/// clients find its id by its origin.
fn advertised(url: &RelayUrl, key: &PushRelayPublicKey) -> pingers_proto::PushRelayInfo {
    pingers_proto::PushRelayInfo {
        relay_id: HARNESS_PUSH_RELAY_ID,
        url: url.clone(),
        key_config_hash: pingers_crypto::key_config_hash(
            &pingers_proto::PushRelayKeyConfig {
                key_id: pingers_proto::PushRelayKeyId::new(key.key_id()),
                public_key: *key.public_key(),
            }
            .to_bytes(),
        ),
        operator: "pingers-testkit harness".to_owned(),
    }
}

/// The map host `/v1/params` advertises.
fn map_host() -> Result<pingers_proto::MapHostUrl, HarnessError> {
    pingers_proto::MapHostUrl::new(HARNESS_MAP_HOST.to_owned()).map_err(|_| HarnessError::Keys {
        service: Service::Server,
    })
}

/// The push relay's keys could not be made.
fn push_relay_keys<E>(_: E) -> HarnessError {
    HarnessError::Keys {
        service: Service::PushRelay,
    }
}

/// The scheduled second push relay key, if the config asks for one, and its public key.
/// From a generator of its own, so every other key stays as it is without a rotation.
fn next_push_key(config: &HarnessConfig) -> Result<NextPushKey, HarnessError> {
    let Some(activated_at) = config.push_relay_next_key_at else {
        return Ok((None, None));
    };
    let private = PushRelayPrivateKey::from_bytes(Zeroizing::new(random_array(&mut seeded_rng(
        config.seed.wrapping_add(1),
    ))));
    let public = private.public_key(2).map_err(push_relay_keys)?;
    Ok((
        Some((private, activated_at)),
        Some(public.public_key().to_vec()),
    ))
}

/// A scheduled push relay key with its activation, and its public key.
type NextPushKey = (Option<(PushRelayPrivateKey, UnixSeconds)>, Option<Vec<u8>>);

/// Where the server forwards map downloads through the relay.
fn map_host_gateway(
    config: &HarnessConfig,
) -> Result<Option<pingers_server::MapHostGateway>, HarnessError> {
    pingers_server::MapHostGateway::from_settings(
        Some(HARNESS_MAP_HOST),
        config.map_host_gateway.clone(),
    )
    .map_err(|_| HarnessError::Keys {
        service: Service::Server,
    })
}

/// Binds, generates keys, and starts the three services on their own runtime thread.
pub(super) fn start(config: &HarnessConfig, capture: Capture) -> Result<Started, HarnessError> {
    let listeners = Listeners {
        server: bind(config, Service::Server)?,
        https: bind(config, Service::Server)?,
        gateway: bind(config, Service::Server)?,
        relay: bind(config, Service::OhttpRelay)?,
        relay_https: bind(config, Service::OhttpRelay)?,
        push: bind(config, Service::PushRelay)?,
    };
    let server = address(&listeners.server, Service::Server)?;
    let server_https = address(&listeners.https, Service::Server)?;
    let ohttp_relay_https = address(&listeners.relay_https, Service::OhttpRelay)?;

    let mut rng = seeded_rng(config.seed);
    let gateway_key = GatewayKey::generate(1, &mut rng).map_err(|_| HarnessError::Keys {
        service: Service::Server,
    })?;
    let ohttp_key_config = gateway_key.config().encoded().to_vec();
    let push_private = PushRelayPrivateKey::from_bytes(Zeroizing::new(random_array(&mut rng)));
    let push_public = push_private.public_key(1).map_err(push_relay_keys)?;
    let seed = RngSeed::from_bytes(Zeroizing::new(random_array(&mut rng)));
    let credential_hash_key = KeyedHashKey::from_bytes(Zeroizing::new(random_array(&mut rng)));
    let token_hash_key = KeyedHashKey::from_bytes(Zeroizing::new(random_array(&mut rng)));
    let (next, next_public) = next_push_key(config)?;

    let push_address = address(&listeners.push, Service::PushRelay)?;
    let tls = super::tls::test_tls(config.bind).map_err(push_relay_keys)?;
    let server_id = format!("https://{server_https}");
    let endpoints = Endpoints {
        server,
        server_https,
        gateway: address(&listeners.gateway, Service::Server)?,
        ohttp_relay: address(&listeners.relay, Service::OhttpRelay)?,
        ohttp_relay_https,
        push_relay: push_address,
        push_relay_ca: tls.ca_pem.clone(),
        server_id: server_id.clone(),
        ohttp_key_config,
        push_relay_key: push_public.public_key().to_vec(),
        push_relay_next_key: next_public,
    };
    let keys_error = |_| HarnessError::Keys {
        service: Service::Server,
    };
    let push_url = https_url(push_address, "", Service::PushRelay)?;
    let credential = PUSH_CREDENTIAL.to_owned();
    // Clients that add this server by its address learn the relay from `/v1/params`, and a
    // `RelayUrl` takes only `https://`: the relay's TLS front.
    let relay_url = https_url(ohttp_relay_https, "/ohttp", Service::OhttpRelay)?;
    // The relay forwards to the gateway's own listener, which test clients have no reason to
    // reach; on loopback the relay's connections come from the bind address, so the address
    // allow-list cannot tell it from a client there. The push relay is HTTPS with the test
    // CA's certificate, which the server is told to trust.
    let server_config = ServerConfig {
        server_operator: config.operator.clone().unwrap_or_else(|| server_id.clone()),
        request_timeout: REQUEST_TIMEOUT,
        metrics: AddressList::new(vec![config.bind]),
        ohttp_relay_urls: vec![relay_url],
        advertised_push_relays: vec![advertised(&push_url, &push_public)],
        map_host: Some(map_host()?),
        map_host_gateway: map_host_gateway(config)?,
        push_relays: ListedRelays::new([(
            HARNESS_PUSH_RELAY_ID,
            RelayTarget {
                url: push_url.into(),
                credential: RelayCredential::new(credential),
            },
        )]),
        push_relay_ca: Some(CertificateSource::Pem(tls.ca_pem.clone().into_bytes())),
        registration_codes: registration_codes(config),
        membership_proofs: config.membership_proofs,
        ..ServerConfig::new(Required {
            listen: server,
            server_id: ServerId::new(server_id).map_err(|_| HarnessError::Keys {
                service: Service::Server,
            })?,
            ohttp_keys: OhttpKeys::new(vec![gateway_key]).map_err(keys_error)?,
            gateway_listen: address(&listeners.gateway, Service::Server)?,
            relays: RelayAllowList::new(vec![config.bind]).map_err(keys_error)?,
        })
    };
    let push = push_relay(
        PushRelayKeys {
            private: push_private,
            next,
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
        listeners,
        server_config,
        outside,
        PushService {
            relay: push,
            tls: tls.acceptor,
        },
    )?;
    Ok(Started { endpoints, running })
}

/// `https://<address><path>` as a relay URL.
fn https_url(address: SocketAddr, path: &str, service: Service) -> Result<RelayUrl, HarnessError> {
    RelayUrl::new(format!("https://{address}{path}")).map_err(|_| HarnessError::Keys { service })
}

/// The operator's registration codes, as the server holds them.
fn registration_codes(config: &HarnessConfig) -> Vec<SecretText> {
    config
        .registration_codes
        .iter()
        .map(|code| SecretText::new(code.clone()))
        .collect()
}

/// The bound listeners: the server's two and its TLS front, the relay's and its TLS front, and
/// the push relay's.
struct Listeners {
    server: TcpListener,
    https: TcpListener,
    gateway: TcpListener,
    relay: TcpListener,
    relay_https: TcpListener,
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
    let gateway = address(&listeners.gateway, Service::Server)?;
    let server = address(&listeners.server, Service::Server)?;
    let relay = address(&listeners.relay, Service::OhttpRelay)?;
    let Listeners {
        server: server_listener,
        https: https_listener,
        gateway: gateway_listener,
        relay: relay_listener,
        relay_https: relay_https_listener,
        push: push_listener,
    } = listeners;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("pingers-harness")
        .enable_all()
        .build()
        .map_err(|source| HarnessError::Runtime { source })?;
    let (server_listeners, https_listener, relay_listener, relay_https_listener, push_listener) = {
        let _entered = runtime.enter();
        let tokio_listener = |listener, service| {
            tokio::net::TcpListener::from_std(listener)
                .map_err(|source| HarnessError::Bind { service, source })
        };
        (
            pingers_server::Listeners {
                public: tokio_listener(server_listener, Service::Server)?,
                gateway: tokio_listener(gateway_listener, Service::Server)?,
            },
            tokio_listener(https_listener, Service::Server)?,
            tokio_listener(relay_listener, Service::OhttpRelay)?,
            tokio_listener(relay_https_listener, Service::OhttpRelay)?,
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
    let https_done = until_stopped();
    let relay_https_done = until_stopped();
    let thread = std::thread::Builder::new()
        .name("pingers-harness".to_owned())
        .spawn(move || {
            runtime.block_on(async move {
                let server_task = pingers_server::serve_with(
                    server_config,
                    outside,
                    server_listeners,
                    server_done,
                );
                let relay_task = super::relay::serve(relay_listener, gateway, relay_done);
                let front =
                    super::tls::forward(https_listener, push.tls.clone(), server, https_done);
                let relay_front = super::tls::forward(
                    relay_https_listener,
                    push.tls.clone(),
                    relay,
                    relay_https_done,
                );
                let push = async move {
                    let router = pingers_push_relay::http::router(Arc::new(push.relay));
                    super::tls::serve(push_listener, push.tls, router, push_done).await;
                };
                // A server that fails to start has already failed the test that used it.
                let (_served, (), (), (), ()) =
                    tokio::join!(server_task, front, relay_task, relay_front, push);
            });
        })
        .map_err(|source| HarnessError::Runtime { source })?;
    Ok(Running {
        stop,
        thread: Some(thread),
    })
}
