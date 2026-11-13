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
//! - **Board and mailboxes** are wired to pingers-board-store and pingers-mailbox (bodies and
//!   textual encodings in `wire.rs`); mailbox wakes are forwarded to the configured push relays
//!   by owned background tasks, and expired segments and mailboxes are swept on a timer.
//! - **Credentials** (`credential.rs`): every route that takes an Anon credential checks an
//!   RLN proof (pingers-quota's nullifier set) or spends a Privacy Pass token first; group
//!   routes also check the Grp signature against pingers-delivery's current log-auth key.
//! - **Groups** (`groups.rs`, `live_groups.rs`) are wired to pingers-delivery: logs, the one
//!   commit per epoch, subscriptions, snapshots, deletion, lifetime classes, wakes to the push
//!   relays (410 deletes the subscription) and live fan-out.
//! - **Accounts, the membership tree and Privacy Pass** (`accounts.rs`, `quota_routes.rs`)
//!   are wired to pingers-quota; `/v1/params` publishes the configuration.
//! - **Background tasks**: the expiry sweep (board, mailboxes, idle groups, sessions,
//!   bearers), the hourly membership root and the daily issuer key rotation, all owned and
//!   cancelled on shutdown.
#![forbid(unsafe_code)]

mod accounts;
mod admin;
mod board_routes;
mod config;
mod credential;
mod dispatch;
mod error;
mod forwarded;
mod gateway;
mod groups;
mod http;
mod live;
mod live_groups;
mod mailbox_routes;
mod metrics;
mod quota_routes;
mod route;
#[cfg(unix)]
mod signal;
mod tasks;
mod wire;

use std::future::Future;
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pingers_board_store::{Board, CursorKey, PageSize};
use pingers_delivery::{Delivery, SqliteDeliveryStore};
use pingers_mailbox::{MailboxParts, MailboxRules, Mailboxes, PostsPerBucket, SqliteStorage};
use pingers_ohttp::{Gateway, KeyConfig};
use pingers_proto::{PushRelayInfo, RelayUrl};
use pingers_quota::{Membership, Nullifiers, PrivacyPass, SqliteQuotaStore};
use pingers_time::{Clock, Timer};
use pingers_transport::TokioTimer;
use rand_chacha::ChaCha20Rng;
use rand_core::{Rng as _, SeedableRng as _};
use zeroize::Zeroizing;

pub use crate::admin::{Command, OhttpSecret, new_ohttp_secret, os_ohttp_secret, pins};
pub use crate::config::{
    AddressList, CertificateSource, ConfigError, Database, Logging, OhttpKeys, RelayAllowList,
    Required, ServedKey, ServerConfig,
};
pub use crate::error::ServerError;
pub use crate::forwarded::TrustedProxies;
pub use crate::live::{
    DeliveryLive, KEEP_ALIVE_CHUNK, KEEP_ALIVE_INTERVAL, LIVE_BUFFER, LiveBackend, LiveChannel,
    LiveFrame, LiveRefused,
};
#[cfg(unix)]
pub use crate::signal::StopSignals;

use crate::accounts::Accounts;
use crate::board_routes::BoardService;
use crate::credential::Sessions;
use crate::groups::GroupService;
use crate::live_groups::LiveChannels;
use crate::mailbox_routes::MailboxService;
use crate::metrics::Metrics;
use crate::tasks::{HttpPushRelay, TaskParts, Tasks};

/// How often a new membership root is started (design B.4.8: hourly root epochs).
const MEMBERSHIP_ROLL_INTERVAL: Duration = pingers_quota::ROOT_EPOCH_LENGTH;
/// How often expired issuer keys, spent tokens and counts are dropped and a new key started.
const ISSUER_ROTATE_INTERVAL: Duration = Duration::from_hours(24);

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

/// pingers-quota's services over one store.
pub(crate) struct Quota {
    pub(crate) membership: Membership<Arc<SqliteQuotaStore>>,
    pub(crate) nullifiers: Nullifiers,
    pub(crate) privacy_pass: PrivacyPass<Arc<SqliteQuotaStore>>,
}

/// What `/v1/params` publishes besides the fixed policy.
pub(crate) struct PublishedParams {
    pub(crate) server_operator: String,
    pub(crate) ohttp_relays: Vec<RelayUrl>,
    pub(crate) push_relays: Vec<PushRelayInfo>,
    pub(crate) map_host: Option<pingers_proto::MapHostUrl>,
}

/// Where live exchanges go.
pub(crate) enum LiveRoute {
    /// pingers-delivery's fan-out registry.
    Groups,
    /// An injected channel (tests).
    Custom(Arc<dyn LiveChannel>),
}

/// What every request handler shares.
pub(crate) struct Shared {
    pub(crate) gateway: Gateway,
    pub(crate) served_config: KeyConfig,
    pub(crate) relays: RelayAllowList,
    pub(crate) trusted_proxies: TrustedProxies,
    pub(crate) metrics_access: AddressList,
    pub(crate) metrics: Metrics,
    pub(crate) request_timeout: Duration,
    pub(crate) timer: Arc<dyn Timer>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) live: LiveRoute,
    pub(crate) board: Mutex<BoardService>,
    pub(crate) mailboxes: Arc<Mutex<MailboxService>>,
    pub(crate) groups: Arc<GroupService>,
    pub(crate) live_channels: LiveChannels,
    pub(crate) quota: Quota,
    pub(crate) accounts: Accounts,
    pub(crate) sessions: Sessions,
    pub(crate) params: PublishedParams,
    pub(crate) tasks: Tasks,
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

    /// A generator of its own, seeded from the server's, for work that should not hold the
    /// shared one (blocking-pool calls, batch issuance).
    pub(crate) fn forked_rng(&self) -> ChaCha20Rng {
        self.with_rng(|rng| {
            let mut seed = Zeroizing::new([0_u8; 32]);
            rng.fill_bytes(seed.as_mut_slice());
            ChaCha20Rng::from_seed(*seed)
        })
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
pub(crate) fn os_seed() -> Result<RngSeed, ServerError> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(bytes.as_mut_slice()))
        .map_err(ServerError::Entropy)?;
    Ok(RngSeed::from_bytes(bytes))
}

/// The server's two bound listeners.
pub struct Listeners {
    /// The public API (`listen`): identified and public routes. `POST /ohttp` is 404 here.
    pub public: tokio::net::TcpListener,
    /// The gateway (`[ohttp] listen`): `POST /ohttp` and `GET /v1/ohttp/config`, nothing else.
    /// Only the OHTTP relay should be able to connect to it.
    pub gateway: tokio::net::TcpListener,
}

impl Listeners {
    /// Binds `config.listen` and `config.gateway_listen`.
    ///
    /// # Errors
    ///
    /// [`ServerError::Listen`] or [`ServerError::ListenGateway`] if an address cannot be bound.
    pub async fn bind(config: &ServerConfig) -> Result<Self, ServerError> {
        Ok(Self {
            public: tokio::net::TcpListener::bind(config.listen)
                .await
                .map_err(ServerError::Listen)?,
            gateway: tokio::net::TcpListener::bind(config.gateway_listen)
                .await
                .map_err(ServerError::ListenGateway)?,
        })
    }
}

/// Runs the server on `listeners` until `shutdown` completes, with the production timer, a
/// generator seeded by the operating system, and [`DeliveryLive`]. The binary passes
/// [`pingers_time::SystemClock`]. Must be called inside a tokio runtime.
///
/// # Errors
///
/// [`ServerError::Entropy`] if the operating system's generator cannot be read; otherwise as
/// [`serve_with`].
pub async fn serve(
    config: ServerConfig,
    listeners: Listeners,
    clock: impl Clock + 'static,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let outside = Outside {
        clock,
        timer: TokioTimer::new(),
        seed: os_seed()?,
        live: DeliveryLive,
    };
    serve_with(config, outside, listeners, shutdown).await
}

/// The push relay client's extra trust roots.
fn push_relay_roots(
    source: Option<&CertificateSource>,
) -> Result<Vec<reqwest::Certificate>, ServerError> {
    let pem = match source {
        None => return Ok(Vec::new()),
        Some(CertificateSource::Pem(pem)) => pem.clone(),
        Some(CertificateSource::File(path)) => {
            std::fs::read(path).map_err(ServerError::ReadCertificates)?
        }
    };
    reqwest::Certificate::from_pem_bundle(&pem).map_err(ServerError::HttpClient)
}

/// The mailboxes, in their configured store.
fn mailboxes(
    config: &ServerConfig,
    clock: &Arc<dyn Clock>,
) -> Result<Arc<Mutex<MailboxService>>, ServerError> {
    let storage = match &config.mailbox_store {
        Database::Memory => SqliteStorage::open_in_memory(),
        Database::File(path) => SqliteStorage::open(path),
    }
    .map_err(ServerError::MailboxStore)?;
    Ok(Arc::new(Mutex::new(Mailboxes::new(MailboxParts {
        storage,
        clock: Arc::clone(clock),
        directory: config.push_relays.clone(),
        limit: PostsPerBucket::new(config.mailbox_posts_per_bucket),
        rules: MailboxRules::DEFAULT,
    }))))
}

/// The delivery service, in its configured store.
fn groups(config: &ServerConfig, clock: &Arc<dyn Clock>) -> Result<Arc<GroupService>, ServerError> {
    let store = match &config.delivery_store {
        Database::Memory => SqliteDeliveryStore::open_in_memory(),
        Database::File(path) => SqliteDeliveryStore::open(path),
    }
    .map_err(ServerError::DeliveryStore)?;
    Ok(Arc::new(Delivery::new(store, Arc::clone(clock))))
}

/// The quota services over one store.
fn quota(config: &ServerConfig, clock: &Arc<dyn Clock>) -> Result<Quota, ServerError> {
    let store = Arc::new(
        match &config.quota_store {
            Database::Memory => SqliteQuotaStore::open_in_memory(),
            Database::File(path) => SqliteQuotaStore::open(path),
        }
        .map_err(ServerError::QuotaStore)?,
    );
    Ok(Quota {
        membership: Membership::new(Arc::clone(&store), Arc::clone(clock))
            .map_err(ServerError::QuotaStore)?,
        nullifiers: Nullifiers::new(config.server_id.clone(), Arc::clone(clock)),
        privacy_pass: PrivacyPass::new(store, Arc::clone(clock))
            .map_err(ServerError::QuotaStore)?,
    })
}

/// As [`serve`], with time, randomness and the live channel injected: for tests and the
/// in-process harness. Returns once `shutdown` completes, every connection is closed and every
/// background task (push jobs, live channel readers, the sweeps, the counts report) is
/// cancelled.
///
/// # Errors
///
/// [`ServerError::MailboxStore`], [`ServerError::DeliveryStore`] or
/// [`ServerError::QuotaStore`] if a database cannot be opened;
/// [`ServerError::ReadCertificates`] or [`ServerError::HttpClient`] if the push relay client
/// cannot be built.
pub async fn serve_with<C: Clock + 'static, T: Timer + 'static, L: LiveChannel>(
    config: ServerConfig,
    outside: Outside<C, T, L>,
    listeners: Listeners,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let Outside {
        clock,
        timer,
        seed,
        live,
    } = outside;
    let served_config = config.ohttp_keys.served().clone();
    let timer: Arc<dyn Timer> = Arc::new(timer);
    let clock: Arc<dyn Clock> = Arc::new(clock);
    let mut rng = ChaCha20Rng::from_seed(*seed.bytes);
    let board = Board::new(
        Arc::clone(&clock),
        CursorKey::generate(&mut rng),
        PageSize::MAX,
    );
    let directory = config.push_relays.clone();
    let mailboxes = mailboxes(&config, &clock)?;
    let groups = groups(&config, &clock)?;
    let quota = quota(&config, &clock)?;
    // A root and an issuing key exist from the start, so the first device can prove or fetch
    // tokens without waiting for the first sweep.
    quota.membership.roll().map_err(ServerError::QuotaStore)?;
    quota
        .privacy_pass
        .rotate(&mut rng)
        .map_err(ServerError::QuotaStore)?;
    let relay = HttpPushRelay::new(
        Arc::clone(&timer),
        config.push_timeout,
        push_relay_roots(config.push_relay_ca.as_ref())?,
    )
    .map_err(ServerError::HttpClient)?;
    let live = match live.backend() {
        LiveBackend::Delivery => LiveRoute::Groups,
        LiveBackend::Custom => LiveRoute::Custom(Arc::new(live)),
    };
    let shared = Arc::new(Shared {
        gateway: Gateway::new(config.ohttp_keys.into_keys()),
        served_config,
        relays: config.relays,
        trusted_proxies: config.trusted_proxies,
        metrics_access: config.metrics,
        metrics: Metrics::default(),
        request_timeout: config.request_timeout,
        timer: Arc::clone(&timer),
        clock: Arc::clone(&clock),
        live,
        board: Mutex::new(board),
        tasks: Tasks::new(TaskParts {
            relay,
            timer: Arc::clone(&timer),
            mailboxes: Arc::clone(&mailboxes),
            groups: Arc::clone(&groups),
            directory,
        }),
        mailboxes,
        groups,
        live_channels: LiveChannels::default(),
        quota,
        accounts: Accounts::new(config.server_id, config.registration_codes),
        sessions: Sessions::default(),
        params: PublishedParams {
            server_operator: config.server_operator,
            ohttp_relays: config.ohttp_relay_urls,
            push_relays: config.advertised_push_relays,
            map_host: config.map_host,
        },
        rng: Mutex::new(rng),
    });
    start_sweeps(&shared, config.expire_interval);
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
    http::accept_loop(listeners, &shared, shutdown).await;
    shared.tasks.abort_all();
    if let Some(reporter) = reporter {
        reporter.abort();
    }
    Ok(())
}

/// Runs `work` on the blocking pool if the server is still running.
async fn on_blocking_pool(shared: &std::sync::Weak<Shared>, work: fn(&Shared)) {
    let Some(shared) = shared.upgrade() else {
        return;
    };
    // A failure is retried at the next sweep.
    let _ = tokio::task::spawn_blocking(move || work(&shared)).await;
}

/// Expires idle groups, group sessions and bearers.
fn expire_groups_and_sessions(shared: &Shared) {
    let now = shared.clock.now();
    let _ = shared.groups.expire_idle();
    shared.sessions.expire(now);
    shared.accounts.expire(now);
}

/// Starts the next membership root on time even when nobody asks.
fn roll_membership(shared: &Shared) {
    let _ = shared.quota.membership.roll();
}

/// Drops expired issuer keys and spent tokens, and starts a new key when one is due.
fn rotate_issuer(shared: &Shared) {
    let _ = shared.quota.privacy_pass.rotate(&mut shared.forked_rng());
}

/// The periodic sweeps, owned by the server's tasks.
fn start_sweeps(shared: &Arc<Shared>, expire_interval: Duration) {
    let weak = Arc::downgrade(shared);
    shared.tasks.every(expire_interval, move || {
        let weak = weak.clone();
        async move {
            if let Some(shared) = weak.upgrade() {
                board_routes::expire(&shared);
                mailbox_routes::expire(&shared.mailboxes).await;
            }
            on_blocking_pool(&weak, expire_groups_and_sessions).await;
        }
    });
    let weak = Arc::downgrade(shared);
    shared.tasks.every(MEMBERSHIP_ROLL_INTERVAL, move || {
        let weak = weak.clone();
        async move { on_blocking_pool(&weak, roll_membership).await }
    });
    let weak = Arc::downgrade(shared);
    shared.tasks.every(ISSUER_ROTATE_INTERVAL, move || {
        let weak = weak.clone();
        async move { on_blocking_pool(&weak, rotate_issuer).await }
    });
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
