// SPDX-License-Identifier: AGPL-3.0-only
//! The world the tests run in: the in-process harness (real server, OHTTP relay and push
//! relay), and per device an engine over a real OHTTP transport through that relay.
//!
//! Two stand-ins fill routes the server on main still answers `501`:
//!
//! - group routes are answered by pingers-delivery in process, but only when the server
//!   answered `501`, so the tests run against the real server once it serves them;
//! - Privacy Pass issuance (identified, direct) goes to the real server with the harness's
//!   `send_direct`, since the harness server speaks plain HTTP and `DirectTransport` requires
//!   `https://`; the account behind it is registered when the world starts.
//!
//! Every anonymous request still crosses the real relay and gateway first.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use pingers_anon::TokenCount;
use pingers_crypto::{AccountSigningKey, sign_auth_challenge, sign_registration};
use pingers_delivery::{Delivery, MemoryDeliveryStore};
use pingers_ohttp::{BinaryRequest, BinaryResponse, Header, StatusCode};
use pingers_proto::{
    AnonymousCredential, AppMessage, AuthRequest, AuthResponse, CommitRequest, CreateGroup,
    GroupId, MessageAccepted, PrivacyPassKeys, RegisterRequest, RegisterResponse, SecretText,
    SequenceNumber, ServerId, Subscription, SubscriptionId, WireDecode, WireEncode,
};
use pingers_store::{Database, DatabaseKey, Session, TokenKeyId};
use pingers_sync::{
    CommitAttempts, CredentialPolicy, ProcessRole, ProverStatus, QueueCapacity, SelfTestPlan,
    SyncEngine, SyncError, SyncParts, SyncSettings, TokenIssuer, TokenStock, WakeBudget,
};
use pingers_testkit::{HarnessConfig, InProcessHarness, TestRng, seeded_rng, send_direct};
use pingers_time::{Clock as _, ManualClock};
use pingers_transport::{
    DirectConfig, DirectTransport, DuplexStream, KeyPin, OhttpConfig, OhttpTransport, PrivacyClass,
    Relay, Router, Routes, TlsRoots, Transport, TransportError, TransportKind,
    certificates_from_pem,
};
use zeroize::Zeroizing;

/// How long any single step may take in real time; a hang fails the test instead of
/// stalling it.
pub(crate) const STEP: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub(crate) enum TestError {
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Harness(#[from] pingers_testkit::HarnessError),
    #[error(transparent)]
    Store(#[from] pingers_store::StoreError),
    #[error(transparent)]
    Anon(#[from] pingers_anon::AnonError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error(transparent)]
    Crypto(#[from] pingers_crypto::CryptoError),
    #[error(transparent)]
    Ohttp(#[from] pingers_ohttp::OhttpError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("timed out")]
    Timeout,
    #[error("unexpected: {0}")]
    Unexpected(&'static str),
}

impl From<tokio::time::error::Elapsed> for TestError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Self::Timeout
    }
}

pub(crate) type TestResult = Result<(), TestError>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lets real time pass, so loopback I/O can happen between manual clock steps.
#[expect(
    clippy::disallowed_methods,
    reason = "the network under test is real; only the engine's timers are manual"
)]
pub(crate) async fn real_pause() {
    tokio::time::sleep(Duration::from_millis(10)).await;
}

/// Runs `future`, advancing `clock` by `step` between short real pauses, so timers the code
/// under test sleeps on fire when it gets to them.
pub(crate) async fn advancing<F: Future>(
    clock: &ManualClock,
    step: Duration,
    future: F,
) -> Result<F::Output, TestError> {
    let mut future = std::pin::pin!(future);
    Ok(tokio::time::timeout(STEP, async {
        loop {
            tokio::select! {
                output = &mut future => return output,
                () = real_pause() => clock.advance(step),
            }
        }
    })
    .await?)
}

/// A fresh directory under the system temporary directory, removed on drop.
pub(crate) struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("pingers-sync-test-{}-{unique}", std::process::id()));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.path));
    }
}

fn response(status: u16, body: Vec<u8>) -> Result<BinaryResponse, TransportError> {
    Ok(BinaryResponse {
        status: StatusCode::new(status)?,
        headers: Vec::new(),
        body,
    })
}

fn encoded(status: u16, value: &impl WireEncode) -> Result<BinaryResponse, TransportError> {
    response(
        status,
        value.encode().map_err(|_| TransportError::InvalidRequest)?,
    )
}

/// The group routes, answered by pingers-delivery while the server answers `501`.
pub(crate) struct GroupStandIn {
    delivery: Delivery<MemoryDeliveryStore, ()>,
    rng: Mutex<TestRng>,
}

impl GroupStandIn {
    fn new(clock: &ManualClock) -> Self {
        Self {
            delivery: Delivery::new(MemoryDeliveryStore::new(), Arc::new(clock.clone())),
            rng: Mutex::new(seeded_rng(99)),
        }
    }

    fn answer(&self, request: &BinaryRequest) -> Option<Result<BinaryResponse, TransportError>> {
        let (path, query) = request
            .path
            .split_once('?')
            .unwrap_or((request.path.as_str(), ""));
        let segments: Vec<&str> = path.strip_prefix("/v1/")?.split('/').collect();
        let group = |hex_id: &str| {
            let mut bytes = [0_u8; 32];
            hex::decode_to_slice(hex_id, &mut bytes)
                .ok()
                .map(|()| GroupId::from_bytes(bytes))
        };
        let answer = match (request.method.as_str(), segments.as_slice()) {
            ("POST", ["groups"]) => {
                let create = CreateGroup::decode(&request.body).ok()?;
                // The stand-in keeps the server's old behaviour: a Meetup at epoch 0.
                let start = pingers_delivery::GroupStart {
                    lifetime: pingers_delivery::GroupLifetime::Meetup,
                    epoch: pingers_proto::Epoch::new(0),
                };
                match self.delivery.create_group(create, start) {
                    Ok(()) => response(201, Vec::new()),
                    Err(_) => response(409, Vec::new()),
                }
            }
            ("POST", ["groups", id, "commit"]) => {
                let commit = CommitRequest::decode(&request.body).ok()?;
                let outcome = self
                    .delivery
                    .commit(&group(id)?, commit, &mut *lock(&self.rng))
                    .ok()?;
                let answer = outcome.response();
                let status = match answer {
                    pingers_proto::CommitResponse::Accepted { .. } => 200,
                    pingers_proto::CommitResponse::Conflict { .. } => 409,
                };
                encoded(status, &answer)
            }
            ("GET", ["groups", id, "msgs"]) => {
                let after = query
                    .strip_prefix("after=")
                    .and_then(|after| after.parse().ok())
                    .unwrap_or(0);
                let messages = self
                    .delivery
                    .messages(
                        &group(id)?,
                        SequenceNumber::new(after),
                        pingers_delivery::Reader::Current,
                    )
                    .ok()?;
                encoded(200, &messages)
            }
            ("POST", ["groups", id, "msgs"]) => {
                let message = AppMessage::decode(&request.body).ok()?;
                let posted = self
                    .delivery
                    .post(&group(id)?, message, &mut *lock(&self.rng))
                    .ok()?;
                match posted.sequence {
                    Some(sequence) => encoded(202, &MessageAccepted { sequence }),
                    None => response(202, Vec::new()),
                }
            }
            ("PUT", ["groups", id, "subs", _]) => {
                let subscription = Subscription::decode(&request.body).ok()?;
                match self.delivery.put_subscription(&group(id)?, subscription) {
                    Ok(()) => response(204, Vec::new()),
                    Err(_) => response(404, Vec::new()),
                }
            }
            ("DELETE", ["groups", id, "subs", sub]) => {
                let mut bytes = [0_u8; 16];
                hex::decode_to_slice(sub, &mut bytes).ok()?;
                match self
                    .delivery
                    .remove_subscription(&group(id)?, &SubscriptionId::from_bytes(bytes))
                {
                    Ok(()) => response(204, Vec::new()),
                    Err(_) => response(404, Vec::new()),
                }
            }
            ("GET", ["groups", id, "state"]) => match self
                .delivery
                .state(&group(id)?, pingers_delivery::Reader::Current)
            {
                Ok(Some(state)) => encoded(200, &state),
                Ok(None) | Err(_) => response(404, Vec::new()),
            },
            ("DELETE", ["groups", id]) => match self.delivery.delete_group(&group(id)?) {
                Ok(()) => response(204, Vec::new()),
                Err(_) => response(404, Vec::new()),
            },
            _ => return None,
        };
        Some(answer)
    }
}

/// What a device's anonymous link saw.
#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub(crate) class: PrivacyClass,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) authorization: Option<String>,
    pub(crate) body: Vec<u8>,
}

/// A device's anonymous and stream route: the real OHTTP transport, with switches the tests
/// flip (relay unreachable, gateway stalls) and the group stand-in behind it.
pub(crate) struct Link {
    relay: OhttpTransport,
    groups: Arc<GroupStandIn>,
    offline: AtomicBool,
    stalled: AtomicBool,
    refuse_commits: AtomicBool,
    rivals: Mutex<VecDeque<BinaryRequest>>,
    seen: Mutex<Vec<Seen>>,
}

impl Link {
    /// Requests fail with `NoRelay`, as when no relay can be reached; nothing is sent.
    pub(crate) fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::SeqCst);
    }

    /// Requests are sent but never answered.
    pub(crate) fn set_stalled(&self, stalled: bool) {
        self.stalled.store(stalled, Ordering::SeqCst);
    }

    /// Commits are answered `503`, as by an overloaded server.
    pub(crate) fn set_refusing_commits(&self, refusing: bool) {
        self.refuse_commits.store(refusing, Ordering::SeqCst);
    }

    /// Sends `rival` just before a later commit this link carries (one rival per commit, in
    /// order): another member's commit for the same epoch winning the race.
    pub(crate) fn race_next_commit(&self, rival: BinaryRequest) {
        lock(&self.rivals).push_back(rival);
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        lock(&self.seen).clone()
    }

    async fn forward(&self, request: BinaryRequest) -> Result<BinaryResponse, TransportError> {
        let answer = self
            .relay
            .request(PrivacyClass::Anonymous, request.clone())
            .await?;
        if answer.status.get() == 501
            && let Some(stand_in) = self.groups.answer(&request)
        {
            return stand_in;
        }
        Ok(answer)
    }
}

#[async_trait::async_trait]
impl Transport for Link {
    async fn request(
        &self,
        class: PrivacyClass,
        request: BinaryRequest,
    ) -> Result<BinaryResponse, TransportError> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(TransportError::NoRelay);
        }
        lock(&self.seen).push(Seen {
            class,
            method: request.method.clone(),
            path: request.path.clone(),
            authorization: request
                .headers
                .iter()
                .find(|header| header.name == "authorization")
                .map(|header| String::from_utf8_lossy(&header.value).into_owned()),
            body: request.body.clone(),
        });
        if self.stalled.load(Ordering::SeqCst) {
            return std::future::pending().await;
        }
        if request.path.ends_with("/commit") {
            if self.refuse_commits.load(Ordering::SeqCst) {
                return response(503, Vec::new());
            }
            let rival = lock(&self.rivals).pop_front();
            if let Some(rival) = rival {
                self.forward(rival).await?;
            }
        }
        self.forward(request).await
    }

    async fn stream(
        &self,
        request: BinaryRequest,
    ) -> Result<Box<dyn DuplexStream>, TransportError> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(TransportError::NoRelay);
        }
        self.relay.stream(request).await
    }

    async fn check_gateway_key(&self) -> Result<(), TransportError> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(TransportError::NoRelay);
        }
        self.relay.check_gateway_key().await
    }

    fn gateway_key_hash(&self) -> Option<pingers_proto::KeyConfigHash> {
        self.relay.key_config_hash()
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Ohttp
    }
}

/// A device's own identified route: the real direct transport over HTTPS to the harness
/// server's TLS front, trusting only the test CA. Records each path it carried.
pub(crate) struct Direct {
    direct: DirectTransport,
    seen: Mutex<Vec<String>>,
}

impl Direct {
    fn new(world: &World) -> Result<Self, TestError> {
        let endpoints = world.harness.endpoints();
        let roots = TlsRoots::from_der(&certificates_from_pem(&endpoints.push_relay_ca)?)?;
        Ok(Self {
            direct: DirectTransport::new(
                &DirectConfig::new(endpoints.server_id.clone(), roots),
                Arc::new(world.clock.clone()),
            )?,
            seen: Mutex::new(Vec::new()),
        })
    }

    /// The paths sent so far, oldest first, and forgets them.
    pub(crate) fn take_seen(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.seen))
    }
}

#[async_trait::async_trait]
impl Transport for Direct {
    async fn request(
        &self,
        class: PrivacyClass,
        request: BinaryRequest,
    ) -> Result<BinaryResponse, TransportError> {
        lock(&self.seen).push(request.path.clone());
        self.direct.request(class, request).await
    }

    async fn stream(
        &self,
        request: BinaryRequest,
    ) -> Result<Box<dyn DuplexStream>, TransportError> {
        self.direct.stream(request).await
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Direct
    }
}

/// The account the tests' identified requests use on the harness server.
#[derive(Clone)]
pub(crate) struct Account {
    server: std::net::SocketAddr,
    server_id: ServerId,
    key: Arc<AccountSigningKey>,
    clock: ManualClock,
}

fn identified(method: &str, path: &str, headers: Vec<Header>, body: Vec<u8>) -> BinaryRequest {
    BinaryRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        body,
    }
}

impl Account {
    /// Registers the account (idempotent) and returns a fresh `Bearer ...` value.
    pub(crate) async fn bearer(&self) -> Result<String, TestError> {
        let registration = RegisterRequest {
            signature_key: *self.key.public_key().as_bytes(),
            proof: *sign_registration(&self.key, &self.server_id)?.as_bytes(),
            registration_code: None,
        };
        let registered = send_direct(
            self.server,
            &identified("POST", "/v1/accounts", Vec::new(), registration.encode()?),
        )
        .await?;
        let auth = AuthRequest {
            account_id: RegisterResponse::decode(&registered.body)?.account_id,
            nonce_signature: *sign_auth_challenge(
                &self.key,
                &self.server_id,
                self.clock.now().bucket(),
            )?
            .as_bytes(),
        };
        let authed = send_direct(
            self.server,
            &identified("POST", "/v1/auth", Vec::new(), auth.encode()?),
        )
        .await?;
        Ok(format!(
            "Bearer {}",
            AuthResponse::decode(&authed.body)?.bearer.expose()
        ))
    }

    /// The server's issuing Privacy Pass key.
    async fn issuer(&self) -> Result<TokenIssuer, TestError> {
        let keys = send_direct(
            self.server,
            &identified("GET", "/v1/pp/keys", Vec::new(), Vec::new()),
        )
        .await?;
        let keys = PrivacyPassKeys::decode(&keys.body)?;
        let key = keys
            .keys
            .last()
            .ok_or(TestError::Unexpected("the server has no issuer key"))?;
        Ok(TokenIssuer {
            public_key: key.public_key.clone(),
            key_id: TokenKeyId::from_bytes(key.key_id),
        })
    }

    /// Sends an identified request to the server.
    pub(crate) async fn send(&self, request: &BinaryRequest) -> Result<BinaryResponse, TestError> {
        Ok(send_direct(self.server, request).await?)
    }
}

/// The identified route: Privacy Pass issuance at the real server, as the world's account
/// (whatever bearer the engine presents, the request goes out with a fresh one).
pub(crate) struct Issuer {
    account: Account,
    info: TokenIssuer,
    requests: AtomicUsize,
    unavailable: AtomicBool,
}

impl Issuer {
    fn new(account: Account, info: TokenIssuer) -> Self {
        Self {
            account,
            info,
            requests: AtomicUsize::new(0),
            unavailable: AtomicBool::new(false),
        }
    }

    pub(crate) fn info(&self) -> TokenIssuer {
        self.info.clone()
    }

    pub(crate) fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// The issuer answers `503`.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Transport for Issuer {
    async fn request(
        &self,
        class: PrivacyClass,
        request: BinaryRequest,
    ) -> Result<BinaryResponse, TransportError> {
        if class != PrivacyClass::Identified {
            return Err(TransportError::ClassNotAllowed);
        }
        self.requests.fetch_add(1, Ordering::SeqCst);
        let bearer = request.headers.iter().any(|header: &Header| {
            header.name == "authorization" && header.value.starts_with(b"Bearer ")
        });
        if !bearer || (request.method.as_str(), request.path.as_str()) != ("POST", "/v1/pp/issue") {
            return response(404, Vec::new());
        }
        if self.unavailable.load(Ordering::SeqCst) {
            return response(503, Vec::new());
        }
        let bearer = self
            .account
            .bearer()
            .await
            .map_err(|_| TransportError::InvalidRequest)?;
        let mut forwarded = request.clone();
        forwarded
            .headers
            .retain(|header| header.name != "authorization");
        forwarded.headers.push(Header {
            name: "authorization".to_owned(),
            value: bearer.into_bytes(),
        });
        self.account
            .send(&forwarded)
            .await
            .map_err(|_| TransportError::InvalidRequest)
    }

    async fn stream(
        &self,
        _request: BinaryRequest,
    ) -> Result<Box<dyn DuplexStream>, TransportError> {
        Err(TransportError::ClassNotAllowed)
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Direct
    }
}

/// The harness and what all devices share.
pub(crate) struct World {
    pub(crate) harness: InProcessHarness,
    pub(crate) clock: ManualClock,
    /// The harness server's origin, which proofs are bound to.
    pub(crate) server_id: ServerId,
    /// The account identified requests use.
    pub(crate) account: Account,
    issuer_info: TokenIssuer,
    groups: Arc<GroupStandIn>,
    dir: TestDir,
    next_device: AtomicU64,
}

/// One device: an engine over its own store and link.
pub(crate) struct Device {
    pub(crate) engine: SyncEngine,
    pub(crate) database: Arc<Database>,
    pub(crate) link: Arc<Link>,
    pub(crate) issuer: Arc<Issuer>,
    /// The device's own identified route, if it has one: then identified requests go to the
    /// server as this device's account instead of through `issuer`.
    pub(crate) direct: Option<Arc<Direct>>,
    /// Names the database file and key, so a restart reopens the same store.
    number: u64,
    role: ProcessRole,
    stock: TokenStock,
}

pub(crate) fn bearer() -> SecretText {
    SecretText::new("harness-bearer".to_owned())
}

/// A self-test of one timed proof: on the manual clock it takes no time, so it passes as soon
/// as its two proofs are made, and devices prove once their leaf is live.
pub(crate) fn quick_self_test() -> SelfTestPlan {
    SelfTestPlan::new(core::num::NonZeroU8::MIN, Duration::from_millis(1_500))
}

/// Proofs and tokens accepted; the prover enabled. Devices in these tests have no RLN
/// identity, so the main app falls back to tokens (design B.4.8 trigger 1).
pub(crate) fn policy() -> CredentialPolicy {
    CredentialPolicy {
        accepted: vec![
            AnonymousCredential::MembershipProof,
            AnonymousCredential::PrivacyPass,
        ],
        prover: ProverStatus::Enabled,
    }
}

/// A small stock so issuance stays quick: refill below 3, up to 5.
pub(crate) fn stock() -> Result<TokenStock, TestError> {
    Ok(TokenStock::new(3, TokenCount::new(5)?)?)
}

impl World {
    /// One Privacy Pass token from the server, as an `authorization` header: for requests the
    /// tests build by hand (another member's racing commit).
    pub(crate) async fn spare_token(&self) -> Result<Header, TestError> {
        let pending = pingers_anon::begin_token_issuance(
            &self.issuer_info.public_key,
            TokenCount::new(1)?,
            &mut seeded_rng(self.next_device.fetch_add(1, Ordering::SeqCst)),
        )?;
        let bearer = self.account.bearer().await?;
        let issued = self
            .account
            .send(&identified(
                "POST",
                "/v1/pp/issue",
                vec![Header {
                    name: "authorization".to_owned(),
                    value: bearer.into_bytes(),
                }],
                pingers_proto::PrivacyPassIssueRequest::new(pending.request().to_vec())?
                    .encode()?,
            ))
            .await?;
        let response = pingers_proto::PrivacyPassIssueResponse::decode(&issued.body)?;
        let token = pending
            .finish(response.token_response())?
            .pop()
            .ok_or(TestError::Unexpected("no token issued"))?;
        Ok(Header {
            name: "authorization".to_owned(),
            value: token.authorization().as_bytes().to_vec(),
        })
    }

    pub(crate) fn start() -> Result<Self, TestError> {
        Self::start_with(&HarnessConfig::default())
    }

    /// A world whose harness starts with `config`.
    pub(crate) fn start_with(config: &HarnessConfig) -> Result<Self, TestError> {
        let clock = config.clock.clone();
        let harness = InProcessHarness::start(config)?;
        let server_id = ServerId::new(harness.endpoints().server_id.clone())?;
        let account = Account {
            server: harness.endpoints().server,
            server_id: server_id.clone(),
            key: Arc::new(AccountSigningKey::from_bytes(Zeroizing::new([7; 32]))),
            clock: clock.clone(),
        };
        // The issuer key is read once, on a runtime of its own: tests start worlds from
        // inside their own runtime, which cannot be blocked on.
        let reader = account.clone();
        let issuer_info = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| TestError::Unexpected("no runtime for the setup"))?
                .block_on(reader.issuer())
        })
        .join()
        .map_err(|_| TestError::Unexpected("the setup thread panicked"))??;
        Ok(Self {
            harness,
            server_id,
            account,
            issuer_info,
            groups: Arc::new(GroupStandIn::new(&clock)),
            clock,
            dir: TestDir::new()?,
            next_device: AtomicU64::new(1),
        })
    }

    fn relay(&self, address: &str) -> Result<OhttpTransport, TestError> {
        let config = OhttpConfig::new(
            vec![Relay::new(
                &format!("http://{address}/ohttp"),
                &format!("http://{address}/v1/ohttp/config"),
            )?],
            KeyPin::FirstUse,
            TlsRoots::from_der(&[])?,
        );
        Ok(OhttpTransport::new(
            config,
            Arc::new(self.clock.clone()),
            Box::new(seeded_rng(self.next_device.load(Ordering::SeqCst))),
        )?)
    }

    /// A main-app device talking through the harness relay.
    pub(crate) fn device(&self) -> Result<Device, TestError> {
        let address = self.harness.endpoints().ohttp_relay.to_string();
        self.device_with(&address, ProcessRole::MainApp)
    }

    /// A main-app device with an account of its own: identified requests go over HTTPS to
    /// the harness server's TLS front, as the client's direct transport sends them.
    pub(crate) fn account_device(&self) -> Result<Device, TestError> {
        let address = self.harness.endpoints().ohttp_relay.to_string();
        self.device_on(
            &address,
            ProcessRole::MainApp,
            Some(Arc::new(Direct::new(self)?)),
        )
    }

    /// A device whose relay is at `address` (which may be unreachable) in `role`.
    pub(crate) fn device_with(
        &self,
        address: &str,
        role: ProcessRole,
    ) -> Result<Device, TestError> {
        self.device_on(address, role, None)
    }

    fn device_on(
        &self,
        address: &str,
        role: ProcessRole,
        direct: Option<Arc<Direct>>,
    ) -> Result<Device, TestError> {
        self.device_stocked(
            address,
            DeviceSetup {
                role,
                direct,
                stock: stock()?,
            },
        )
    }

    /// A main-app device through the harness relay that keeps `stock` tokens.
    pub(crate) fn device_with_stock(&self, stock: TokenStock) -> Result<Device, TestError> {
        let address = self.harness.endpoints().ohttp_relay.to_string();
        self.device_stocked(
            &address,
            DeviceSetup {
                role: ProcessRole::MainApp,
                direct: None,
                stock,
            },
        )
    }

    fn device_stocked(&self, address: &str, setup: DeviceSetup) -> Result<Device, TestError> {
        let DeviceSetup {
            role,
            direct,
            stock,
        } = setup;
        let number = self.next_device.fetch_add(1, Ordering::SeqCst);
        let link = Arc::new(Link {
            relay: self.relay(address)?,
            groups: Arc::clone(&self.groups),
            offline: AtomicBool::new(false),
            stalled: AtomicBool::new(false),
            refuse_commits: AtomicBool::new(false),
            rivals: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
        });
        let issuer = Arc::new(Issuer::new(self.account.clone(), self.issuer_info.clone()));
        self.open_device(Boot {
            number,
            role,
            stock,
            link,
            issuer,
            direct,
            seed: number.saturating_add(1000),
        })
    }

    /// The same device after an app restart: a fresh engine and session over the same
    /// database file. Only the network (link, issuer) carries over; nothing in memory does.
    pub(crate) fn restart(&self, device: Device) -> Result<Device, TestError> {
        let Device {
            engine,
            database,
            link,
            issuer,
            direct,
            number,
            role,
            stock,
        } = device;
        // Nothing in memory carries over: the engine and the store handle go first.
        drop(engine);
        drop(database);
        self.open_device(Boot {
            number,
            role,
            stock,
            link,
            issuer,
            direct,
            seed: number.saturating_add(2000),
        })
    }

    fn open_device(&self, boot: Boot) -> Result<Device, TestError> {
        let Boot {
            number,
            role,
            stock,
            link,
            issuer,
            direct,
            seed,
        } = boot;
        let database = Database::open(
            &self.dir.path.join(format!("device-{number}.db")),
            DatabaseKey::from_bytes(Zeroizing::new(
                [u8::try_from(number % 200).unwrap_or(1); 32],
            )),
        )?;
        let database = Arc::new(database);
        let router = Router::new(Routes {
            anonymous: link.clone(),
            identified: match &direct {
                Some(direct) => direct.clone(),
                None => issuer.clone(),
            },
            stream: link.clone(),
        })?;
        let engine = SyncEngine::new(
            SyncParts {
                transport: Arc::new(router),
                database: Arc::clone(&database),
                clock: Arc::new(self.clock.clone()),
                timer: Arc::new(self.clock.clone()),
                rng: Box::new(seeded_rng(seed)),
            },
            SyncSettings {
                server: self.server_id.clone(),
                role,
                credentials: policy(),
                token_stock: stock,
                commit_attempts: CommitAttempts::default(),
                queue_capacity: QueueCapacity::default(),
                self_test: quick_self_test(),
            },
        );
        Ok(Device {
            engine,
            database,
            link,
            issuer,
            direct,
            number,
            role,
            stock,
        })
    }
}

/// What a new device is made with besides its relay.
struct DeviceSetup {
    role: ProcessRole,
    direct: Option<Arc<Direct>>,
    stock: TokenStock,
}

/// What a device is opened with, first or after a restart.
struct Boot {
    number: u64,
    role: ProcessRole,
    stock: TokenStock,
    link: Arc<Link>,
    issuer: Arc<Issuer>,
    direct: Option<Arc<Direct>>,
    seed: u64,
}

impl Device {
    /// A store session of the test's own, for a check between engine steps.
    pub(crate) fn session(&self) -> Result<Session, TestError> {
        Ok(self.database.session()?)
    }

    /// Fills the token stock.
    pub(crate) async fn stock_tokens(&self) -> Result<pingers_sync::Refill, TestError> {
        Ok(self
            .engine
            .refill_tokens(&self.issuer.info(), &bearer(), WakeBudget::UNLIMITED)
            .await?)
    }
}
