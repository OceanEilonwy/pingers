// SPDX-License-Identifier: AGPL-3.0-only
//! The OHTTP transport: anonymous one-shot requests and the chunked live channel, always
//! through a relay (design B.12). There is no path from here to the server that skips one.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hyper::header::CONTENT_TYPE;
use hyper::{Method, Request, StatusCode};
use pingers_ohttp::{BinaryRequest, BinaryResponse, KeyConfig, encapsulate_request};
use pingers_proto::KeyConfigHash;
use pingers_time::{MonotonicTime, Timer};

use crate::endpoint::Endpoint;
use crate::http::{HttpClient, OutBody, SendError, read_body};
use crate::relay::{Backoff, KeyGuard, KeyPin, Relay, RelayPool};
use crate::timer::within;
use crate::tls::{TlsRoots, client_config};
use crate::{
    DuplexStream, PrivacyClass, Transport, TransportError, TransportKind, TransportRng, live,
};

/// RFC 9458 and draft-08 media types.
pub(crate) const OHTTP_REQUEST: &str = "message/ohttp-req";
pub(crate) const OHTTP_CHUNKED_REQUEST: &str = "message/ohttp-chunked-req";

/// The most a key config response may hold. A config with one key and a few suites is under
/// 100 bytes.
const MAX_KEY_CONFIG_BYTES: usize = 1024;

/// How long each step of a request may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// TCP, TLS and the HTTP/2 handshake.
    pub connect: Duration,
    /// From sending a request to having its whole response.
    pub request: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            request: Duration::from_secs(30),
        }
    }
}

/// Live channel tuning (design B.4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveSettings {
    /// A keep-alive chunk goes out when nothing else has for this long (20 s, so
    /// intermediaries do not close an idle exchange).
    pub keep_alive: Duration,
    /// With nothing received for this long (the gateway sends keep-alives too), the exchange
    /// counts as dropped and is reconnected.
    pub idle: Duration,
    /// How long to wait for the response head of a full-duplex exchange before deciding the
    /// relay holds request bodies back and switching to the split channel.
    pub duplex_probe: Duration,
    /// Consecutive failed reconnects after which the channel gives up and reports the error.
    pub max_reconnects: u32,
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self {
            keep_alive: Duration::from_secs(20),
            idle: Duration::from_secs(65),
            duplex_probe: Duration::from_secs(10),
            max_reconnects: 8,
        }
    }
}

/// Everything an [`OhttpTransport`] is configured with. Built with [`OhttpConfig::new`]; the
/// tuning fields have defaults that may be changed.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OhttpConfig {
    /// The relays, most preferred first. At least one.
    pub relays: Vec<Relay>,
    /// What the gateway's key config must match.
    pub pin: KeyPin,
    /// Trusted roots for `https://` relays.
    pub tls: TlsRoots,
    /// Per-step timeouts.
    pub timeouts: Timeouts,
    /// Relay rest after failures, and the live channel's reconnect waits.
    pub backoff: Backoff,
    /// Live channel tuning.
    pub live: LiveSettings,
    /// The largest one-shot response accepted (a board page is up to 256 KiB, design B.4.3).
    pub max_response_bytes: usize,
}

impl OhttpConfig {
    /// A configuration with default tuning.
    #[must_use]
    pub fn new(relays: Vec<Relay>, pin: KeyPin, tls: TlsRoots) -> Self {
        Self {
            relays,
            pin,
            tls,
            timeouts: Timeouts::default(),
            backoff: Backoff::default(),
            live: LiveSettings::default(),
            max_response_bytes: 1024 * 1024,
        }
    }
}

/// What the transport and its live channels share.
pub(crate) struct Shared {
    pub(crate) pool: RelayPool,
    key: Mutex<KeyGuard>,
    pub(crate) http: HttpClient,
    pub(crate) timer: Arc<dyn Timer>,
    rng: Mutex<TransportRng>,
    pub(crate) timeouts: Timeouts,
    pub(crate) backoff: Backoff,
    pub(crate) live: LiveSettings,
    max_response_bytes: usize,
}

/// Whether a failure says something about the relay (so it should rest), rather than about
/// the request or the gateway behind it.
pub(crate) fn is_relay_fault(error: &TransportError) -> bool {
    matches!(
        error,
        TransportError::Timeout | TransportError::Connection | TransportError::NoRelay
    )
}

impl Shared {
    fn key(&self) -> MutexGuard<'_, KeyGuard> {
        self.key.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `f` with the transport's RNG.
    pub(crate) fn with_rng<T>(&self, f: impl FnOnce(&mut TransportRng) -> T) -> T {
        // An RNG left mid-draw by a panic is still an RNG.
        f(&mut self.rng.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn deadline(&self, after: Duration) -> MonotonicTime {
        self.timer.now().saturating_add(after)
    }

    pub(crate) fn relay(&self, index: usize) -> Result<&Relay, SendError> {
        self.pool
            .relay(index)
            .ok_or(SendError::Unreachable(TransportError::NoRelay))
    }

    /// Fails closed if a key mismatch is latched.
    pub(crate) fn check_key(&self) -> Result<(), TransportError> {
        self.key().current().map(|_| ())
    }

    /// The verified key config, fetched through relay `index` if not yet known.
    pub(crate) async fn key_config(&self, index: usize) -> Result<KeyConfig, SendError> {
        if let Some(config) = self.key().current().map_err(SendError::Failed)? {
            return Ok(config);
        }
        let endpoint = self.relay(index)?.key_config_endpoint();
        let request = Request::builder()
            .method(Method::GET)
            .uri(endpoint.uri().map_err(SendError::Failed)?)
            .body(OutBody::empty())
            .map_err(|_| SendError::Failed(TransportError::InvalidRequest))?;
        let encoded = self
            .exchange(endpoint, request, MAX_KEY_CONFIG_BYTES)
            .await?;
        let fetched =
            KeyConfig::decode(&encoded).map_err(|error| SendError::Failed(error.into()))?;
        self.key().accept(fetched).map_err(SendError::Failed)
    }

    /// One request and its whole response body, within the request timeout.
    async fn exchange(
        &self,
        endpoint: &Endpoint,
        request: Request<OutBody>,
        limit: usize,
    ) -> Result<Vec<u8>, SendError> {
        let deadline = self.deadline(self.timeouts.request);
        let response = self
            .http
            .send(&endpoint.origin(), request, deadline)
            .await?;
        if response.status() != StatusCode::OK {
            return Err(SendError::Failed(TransportError::Status {
                status: response.status().as_u16(),
            }));
        }
        let mut body = response.into_body();
        within(&*self.timer, deadline, read_body(&mut body, limit))
            .await
            .and_then(|result| result)
            .map_err(SendError::Failed)
    }

    /// One encapsulated request through relay `index`, with a fresh HPKE context.
    pub(crate) async fn one_shot(
        &self,
        index: usize,
        request: &BinaryRequest,
    ) -> Result<BinaryResponse, SendError> {
        let config = self.key_config(index).await?;
        let encapsulated = self
            .with_rng(|rng| encapsulate_request(&config, request, rng))
            .map_err(|error| SendError::Failed(error.into()))?;
        let endpoint = self.relay(index)?.request_endpoint();
        let post = post(endpoint, OHTTP_REQUEST, OutBody::full(encapsulated.message))
            .map_err(SendError::Failed)?;
        let body = self
            .exchange(endpoint, post, self.max_response_bytes)
            .await?;
        encapsulated
            .response_context
            .open(&body)
            .map_err(|error| SendError::Failed(error.into()))
    }

    /// An anonymous request through the first relay that can be reached. A request is only
    /// tried on the next relay if no connection to the last could be made, so it is never
    /// delivered twice. With no relay reachable it is not sent at all.
    async fn anonymous(&self, request: &BinaryRequest) -> Result<BinaryResponse, TransportError> {
        self.check_key()?;
        for index in self.pool.available(self.timer.now()) {
            match self.one_shot(index, request).await {
                Ok(response) => {
                    self.pool.record_success(index);
                    return Ok(response);
                }
                Err(SendError::Unreachable(_)) => {
                    self.pool.record_failure(index, self.timer.now());
                }
                Err(SendError::Failed(error)) => {
                    if is_relay_fault(&error) {
                        self.pool.record_failure(index, self.timer.now());
                    }
                    return Err(error);
                }
            }
        }
        Err(TransportError::NoRelay)
    }
}

/// A POST of an encapsulated body to a relay. Only the media type goes with it.
pub(crate) fn post(
    endpoint: &Endpoint,
    media_type: &'static str,
    body: OutBody,
) -> Result<Request<OutBody>, TransportError> {
    Request::builder()
        .method(Method::POST)
        .uri(endpoint.uri()?)
        .header(CONTENT_TYPE, media_type)
        .body(body)
        .map_err(|_| TransportError::InvalidRequest)
}

/// Anonymous and stream traffic over OHTTP relays (design B.12).
///
/// Each anonymous request is encapsulated with a fresh HPKE context, so the gateway cannot
/// link two of them. The gateway's key config is fetched through a relay and checked against
/// the pin; a mismatch stops all traffic until [`OhttpTransport::repin`]. With no relay
/// reachable, requests fail with [`TransportError::NoRelay`] and nothing is sent: there is no
/// direct fallback.
pub struct OhttpTransport {
    shared: Arc<Shared>,
}

impl OhttpTransport {
    /// A transport over `config`'s relays. `timer` bounds every network step; `rng` draws
    /// every HPKE key and nonce. Must be used inside a tokio runtime.
    ///
    /// # Errors
    ///
    /// [`TransportError::NoRelay`] if no relay is configured;
    /// [`TransportError::InvalidCertificate`] if TLS cannot be set up.
    pub fn new(
        config: OhttpConfig,
        timer: Arc<dyn Timer>,
        rng: TransportRng,
    ) -> Result<Self, TransportError> {
        let tls = client_config(&config.tls)?;
        let shared = Shared {
            pool: RelayPool::new(config.relays, config.backoff)?,
            key: Mutex::new(KeyGuard::new(config.pin)),
            http: HttpClient::new(tls, Arc::clone(&timer), config.timeouts.connect),
            timer,
            rng: Mutex::new(rng),
            timeouts: config.timeouts,
            backoff: config.backoff,
            live: config.live,
            max_response_bytes: config.max_response_bytes,
        };
        Ok(Self {
            shared: Arc::new(shared),
        })
    }

    /// The pinned key config hash: the configured pin, or after [`KeyPin::FirstUse`] the hash
    /// of the first config fetched, for the caller to store in Party state.
    #[must_use]
    pub fn key_config_hash(&self) -> Option<KeyConfigHash> {
        self.shared.key().pinned_hash()
    }

    /// Replaces the pin once a `RelayKeyMismatch` has been resolved; the key config is
    /// fetched again with the next request.
    pub fn repin(&self, pin: KeyPin) {
        self.shared.key().repin(pin);
    }
}

#[async_trait::async_trait]
impl Transport for OhttpTransport {
    /// Only [`PrivacyClass::Anonymous`]: identified requests name the account and must not
    /// share a connection with anonymous ones, and streams go through
    /// [`Transport::stream`].
    async fn request(
        &self,
        class: PrivacyClass,
        request: BinaryRequest,
    ) -> Result<BinaryResponse, TransportError> {
        match class {
            PrivacyClass::Anonymous => self.shared.anonymous(&request).await,
            PrivacyClass::Identified | PrivacyClass::Stream => Err(TransportError::ClassNotAllowed),
        }
    }

    async fn stream(
        &self,
        request: BinaryRequest,
    ) -> Result<Box<dyn DuplexStream>, TransportError> {
        live::open(Arc::clone(&self.shared), request).await
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Ohttp
    }
}
