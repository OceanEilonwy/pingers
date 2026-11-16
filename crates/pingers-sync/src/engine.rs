// SPDX-License-Identifier: AGPL-3.0-only
//! The engine: sends one request with the right privacy class and credential, inside the wake
//! budget, and queues or drops what could not be sent.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pingers_anon::SelfTestPlan;
use pingers_ohttp::{BinaryResponse, Header, OhttpError};
use pingers_proto::{CommitResponse, MessageAccepted, Params, ServerId, WireDecode};
use pingers_store::{Database, NewQueuedRequest, Pushed, ServerSession};
use pingers_time::{Clock, Timer, UnixSeconds};
use pingers_transport::{DuplexStream, PrivacyClass, Transport, TransportError};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;

use crate::clock::{ClockOffset, ClockStatus};
use crate::credential::{
    Attached, CredentialChoice, CredentialPolicy, ProcessRole, ProofInput, ProvingState,
    choose_credential, proof_fits, proof_header, prove_reserved, reserve_proof, token_header,
};
use crate::membership::TreeCache;
use crate::pool::WakePush;
use crate::prover::SelfTestSlot;
use crate::queue::QueueCapacity;
use crate::{
    ApiRequest, CommitAttempts, QueuePolicy, Route, Sending, SyncError, TokenStock, WakeBudget,
};

/// The randomness the engine draws read keys, handles, subscription ids, push seals, proofs
/// and token blinds from.
pub type SyncRng = Box<dyn rand_core::CryptoRng + Send>;

/// What the engine talks through and keeps its state in.
pub struct SyncParts {
    /// The router: anonymous and stream classes over OHTTP, identified direct.
    pub transport: Arc<dyn Transport>,
    /// The store. Each store step opens its own session (and so holds the cross-process
    /// lock only for that step), so the engine can live for the whole process.
    pub database: Arc<Database>,
    /// Wall time, for buckets, expiries and group request signatures.
    pub clock: Arc<dyn Clock>,
    /// Monotonic time, for the wake budget.
    pub timer: Arc<dyn Timer>,
    /// Randomness.
    pub rng: SyncRng,
    /// Where the prover self-test runs: one per process, cloned into each of its engines,
    /// since the test is of this device's prover and not of a server.
    pub self_test: SelfTestSlot,
}

/// How the engine behaves.
#[derive(Clone, Debug)]
pub struct SyncSettings {
    /// The server's identity, bound into RLN proofs.
    pub server: ServerId,
    /// Main app or extension: only the main app proves.
    pub role: ProcessRole,
    /// The credentials the server accepts and whether the prover may be used.
    pub credentials: CredentialPolicy,
    /// How many Privacy Pass tokens to keep, and how many to fetch at once.
    pub token_stock: TokenStock,
    /// How often a commit is tried before giving up on conflicts.
    pub commit_attempts: CommitAttempts,
    /// How many unsent anonymous requests to keep.
    pub queue_capacity: QueueCapacity,
    /// The prover self-test to run before proving: the design's ([`SelfTestPlan::DESIGN`]) in
    /// an app.
    pub self_test: SelfTestPlan,
}

/// Why a request was not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NotSentReason {
    /// No OHTTP relay was reachable (`TransportError::NoRelay`); nothing left the device.
    NoRelay,
    /// The wake budget did not leave enough time to start it.
    BudgetExhausted,
}

/// What became of a request.
#[derive(Debug)]
#[must_use]
pub enum Outcome {
    /// The server answered (any status).
    Answered(Response),
    /// Not sent; kept for a later flush.
    Queued(NotSentReason),
    /// Not sent; dropped by its policy.
    Dropped(NotSentReason),
}

/// The server's answer to one request.
#[derive(Debug)]
pub struct Response {
    route: Route,
    status: u16,
    body: Vec<u8>,
}

impl Response {
    pub(crate) fn new(route: Route, response: BinaryResponse) -> Self {
        Self {
            route,
            status: response.status.get(),
            body: response.body,
        }
    }

    /// The route answered.
    #[must_use]
    pub const fn route(&self) -> Route {
        self.route
    }

    /// The HTTP status (a wire value).
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// The body (a wire boundary).
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Whether the status is 2xx.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self.status, 200..=299)
    }

    /// `Ok` for a 2xx status.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] otherwise.
    pub fn success(self) -> Result<Self, SyncError> {
        if self.is_success() {
            Ok(self)
        } else {
            Err(SyncError::Status {
                route: self.route,
                status: self.status,
            })
        }
    }

    /// `Ok` for a 2xx status, keeping the response.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] otherwise.
    pub const fn success_ref(&self) -> Result<(), SyncError> {
        if self.is_success() {
            Ok(())
        } else {
            Err(SyncError::Status {
                route: self.route,
                status: self.status,
            })
        }
    }

    /// The body decoded as `T`.
    ///
    /// # Errors
    ///
    /// [`SyncError::BadResponse`] if it does not decode.
    pub fn decode<T: WireDecode>(&self) -> Result<T, SyncError> {
        T::decode(&self.body).map_err(|source| SyncError::BadResponse {
            route: self.route,
            source,
        })
    }
}

impl Outcome {
    /// The answer, or the reason it was not sent as an error, for operations whose result
    /// the caller needs now.
    ///
    /// # Errors
    ///
    /// [`SyncError::NoRelay`] or [`SyncError::BudgetExhausted`] if the request was not sent.
    pub fn answered(self, route: Route) -> Result<Response, SyncError> {
        match self {
            Self::Answered(response) => Ok(response),
            Self::Queued(reason) | Self::Dropped(reason) => Err(not_sent_error(route, reason)),
        }
    }
}

const fn not_sent_error(route: Route, reason: NotSentReason) -> SyncError {
    match reason {
        NotSentReason::NoRelay => SyncError::NoRelay { route },
        NotSentReason::BudgetExhausted => SyncError::BudgetExhausted { route },
    }
}

/// One try at sending.
/// The status for a missing or stale anonymous credential.
const UNAUTHORIZED: u16 = 401;

enum Attempt {
    Answered(Response),
    NotSent(ApiRequest, NotSentReason),
}

/// What one flush of the queue did.
#[derive(Debug, Default)]
#[must_use]
pub struct FlushReport {
    /// Requests sent, with the server's answer.
    pub sent: Vec<Response>,
    /// Requests dropped because their time had passed.
    pub expired: usize,
    /// Requests still waiting (no relay, or the budget ran out).
    pub remaining: usize,
}

/// Talks to the server for everyone else: see the crate docs.
pub struct SyncEngine {
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) database: Arc<Database>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) timer: Arc<dyn Timer>,
    pub(crate) rng: Arc<Mutex<SyncRng>>,
    server: ServerId,
    pub(crate) settings: Mutex<SyncSettings>,
    clock_status: Mutex<ClockStatus>,
    pub(crate) wake_push: Mutex<Option<WakePush>>,
    /// The membership tree between refreshes (`membership.rs`).
    pub(crate) tree_cache: Mutex<Option<TreeCache>>,
    /// The process's background prover self-test, shared with its other engines
    /// (`prover.rs`).
    pub(crate) self_test: SelfTestSlot,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `work` against the store on the blocking pool (SQLCipher blocks), in a session of its
/// own scoped to `server`'s rows: the cross-process lock is held for this step only. A step
/// that must be atomic runs its writes in `session.transaction`.
pub(crate) async fn blocking<T: Send + 'static>(
    database: &Arc<Database>,
    server: &ServerId,
    work: impl FnOnce(&ServerSession<'_>) -> Result<T, SyncError> + Send + 'static,
) -> Result<T, SyncError> {
    let database = Arc::clone(database);
    let server = server.clone();
    tokio::task::spawn_blocking(move || {
        let session = database.session()?;
        work(&session.server(&server)?)
    })
    .await
    .map_err(|_| SyncError::Internal)?
}

/// Runs `work` on the blocking pool with a device-wide session: for the few rows no server owns
/// (the prover self-test, which tests this device's prover).
pub(crate) async fn device_blocking<T: Send + 'static>(
    database: &Arc<Database>,
    work: impl FnOnce(&pingers_store::Session) -> Result<T, SyncError> + Send + 'static,
) -> Result<T, SyncError> {
    let database = Arc::clone(database);
    tokio::task::spawn_blocking(move || work(&database.session()?))
        .await
        .map_err(|_| SyncError::Internal)?
}

impl SyncEngine {
    /// An engine over `parts`.
    #[must_use]
    pub fn new(parts: SyncParts, settings: SyncSettings) -> Self {
        Self {
            transport: parts.transport,
            database: parts.database,
            clock: parts.clock,
            timer: parts.timer,
            rng: Arc::new(Mutex::new(parts.rng)),
            clock_status: Mutex::new(ClockStatus::Unchecked),
            wake_push: Mutex::new(None),
            server: settings.server.clone(),
            tree_cache: Mutex::new(None),
            self_test: parts.self_test,
            settings: Mutex::new(settings),
        }
    }

    /// The server this engine talks to. Every store step of the engine reads and writes only
    /// this server's rows (pingers-store's `ServerSession`); the server must have a row before
    /// the engine is used.
    #[must_use]
    pub fn server(&self) -> ServerId {
        self.server.clone()
    }

    /// [`blocking`] on this engine's server.
    pub(crate) async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&ServerSession<'_>) -> Result<T, SyncError> + Send + 'static,
    ) -> Result<T, SyncError> {
        blocking(&self.database, &self.server, work).await
    }

    /// Replaces the credential policy, after fetching `Params` again or a prover self-test.
    pub fn set_credential_policy(&self, policy: CredentialPolicy) {
        lock(&self.settings).credentials = policy;
    }

    /// Adopts what the server's parameters say about credentials: which ones it accepts (an
    /// operator who turns proofs off, design B.4.8 fallback 3) and its daily token limit. Done
    /// by [`SyncEngine::params`]; parameters read another way (`params_direct`, when a server
    /// is first looked at) are handed here.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn apply_params(&self, params: &Params) -> Result<(), SyncError> {
        lock(&self.settings)
            .credentials
            .accepted
            .clone_from(&params.anonymous_credentials);
        self.note_daily_limit(params.privacy_pass_daily_limit).await
    }

    pub(crate) fn settings(&self) -> SyncSettings {
        lock(&self.settings).clone()
    }

    pub(crate) fn now(&self) -> UnixSeconds {
        self.corrected_now()
    }

    /// The device clock, corrected by the server's time when the last observed offset was
    /// within ±120 s (design F.1). Buckets, expiries and signatures use this; so should the
    /// caller's leases.
    #[must_use]
    pub fn corrected_now(&self) -> UnixSeconds {
        lock(&self.clock_status).correct(self.clock.now())
    }

    /// What the last server time said about the device clock.
    #[must_use]
    pub fn clock_status(&self) -> ClockStatus {
        *lock(&self.clock_status)
    }

    /// Records a `server_time` the server reported (params, auth, board sync) against the
    /// device clock. Returns the new status: [`ClockStatus::Skewed`] means raise
    /// `Notice::ClockSkew`; posting stays refused until a later server time is within range.
    pub fn observe_server_time(&self, server_time: UnixSeconds) -> ClockStatus {
        let status = ClockStatus::from_offset(ClockOffset::between(server_time, self.clock.now()));
        *lock(&self.clock_status) = status;
        status
    }

    /// Draws from the engine's randomness.
    pub(crate) fn with_rng<T>(&self, use_rng: impl FnOnce(&mut SyncRng) -> T) -> T {
        use_rng(&mut lock(&self.rng))
    }

    /// How many requests wait for a later wake (from this or an earlier run, or another
    /// process sharing the store).
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn queued(&self) -> Result<usize, SyncError> {
        self.blocking(|session| Ok(session.request_queue().count()?))
            .await
    }

    /// The credential the next anonymous request would carry, for "What your server sees"
    /// (a Privacy Pass choice in the main app is a fallback, design B.4.8).
    ///
    /// # Errors
    ///
    /// [`SyncError::NoCredential`] if none is possible; [`SyncError::Store`].
    pub async fn credential_choice(&self) -> Result<CredentialChoice, SyncError> {
        let settings = self.settings();
        let state = self.proving_state().await?;
        choose_credential(
            settings.role,
            &state.effective(&settings.credentials),
            state.readiness,
        )
    }

    /// Whether this device could prove now, as far as its identity and tree copy go.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn rln_readiness(&self) -> Result<crate::RlnReadiness, SyncError> {
        Ok(self.proving_state().await?.readiness)
    }

    async fn proving_state(&self) -> Result<ProvingState, SyncError> {
        let now = self.now();
        self.blocking(move |session| ProvingState::load(session, now))
            .await
    }

    /// The credential for `request`, made or taken now: a proof when the main app may prove and
    /// a proof (at the self-test's 95th percentile) still fits the wake, otherwise a token.
    async fn attach(
        &self,
        request: &ApiRequest,
        budget: WakeBudget,
    ) -> Result<Attached, SyncError> {
        let Some(scope) = request.route().credential() else {
            return Ok(Attached::Nothing);
        };
        let settings = self.settings();
        let state = self.proving_state().await?;
        let policy = state.effective(&settings.credentials);
        let choice = choose_credential(settings.role, &policy, state.readiness)?;
        let fits = state
            .proof_time()
            .is_some_and(|proof_time| proof_fits(budget.remaining(&*self.timer), proof_time));
        if choice == CredentialChoice::MembershipProof && fits {
            let input = ProofInput {
                server: settings.server,
                scope,
                bucket: self.now().bucket(),
                method: request.method().to_owned(),
                path: request.path().to_owned(),
                body: request.body().to_vec(),
            };
            if let Some(header) = self.prove_for(input).await? {
                return Ok(Attached::Proof { header });
            }
        }
        // Proofs are not possible here, the wake has no time for one, or the bucket's message
        // ids are spent: a token, if the server takes them.
        if !policy
            .accepted
            .contains(&pingers_proto::AnonymousCredential::PrivacyPass)
        {
            return Err(SyncError::NoCredential);
        }
        let token = self
            .blocking(|session| Ok(session.privacy_pass_tokens().take()?))
            .await?
            .ok_or(SyncError::NoCredential)?;
        Ok(Attached::Token {
            header: token_header(&token),
            token,
        })
    }

    /// Reserves a message id (persisted first) and proves on the blocking pool, with no store
    /// session open while proving. `None` if the bucket's message ids are spent or the device
    /// cannot prove.
    async fn prove_for(&self, input: ProofInput) -> Result<Option<Header>, SyncError> {
        let now = self.now();
        let (reservation, input) = self
            .blocking(move |session| Ok((reserve_proof(session, &input, now)?, input)))
            .await?;
        let Some(reservation) = reservation else {
            return Ok(None);
        };
        let mut seed = [0_u8; 32];
        self.with_rng(|rng| rng.fill_bytes(&mut seed));
        let auth = tokio::task::spawn_blocking(move || {
            prove_reserved(&reservation, &input, &mut ChaCha20Rng::from_seed(seed))
        })
        .await
        .map_err(|_| SyncError::Internal)??;
        Ok(Some(proof_header(&auth)?))
    }

    /// Returns an unspent token to the store: the request carrying it never left the device.
    async fn restore(&self, attached: Attached) -> Result<(), SyncError> {
        if let Attached::Token { token, .. } = attached {
            self.blocking(move |session| {
                Ok(session
                    .privacy_pass_tokens()
                    .add(&[token.token], &token.key_id)?)
            })
            .await?;
        }
        Ok(())
    }

    /// Runs `future` until the budget's deadline; `None` if the deadline came first.
    pub(crate) async fn within<F: Future>(
        &self,
        budget: WakeBudget,
        future: F,
    ) -> Option<F::Output> {
        match budget.deadline() {
            None => Some(future.await),
            Some(deadline) => {
                tokio::select! {
                    output = future => Some(output),
                    () = self.timer.sleep_until(deadline) => None,
                }
            }
        }
    }

    /// Queues or drops an anonymous request that was not sent; other classes fail.
    async fn hold(
        &self,
        request: ApiRequest,
        policy: QueuePolicy,
        reason: NotSentReason,
    ) -> Result<Outcome, SyncError> {
        let route = request.route();
        if route.class() != PrivacyClass::Anonymous {
            return Err(not_sent_error(route, reason));
        }
        match policy {
            QueuePolicy::KeepUntil(until) if until >= self.now() => {
                let bytes = request.to_queued()?;
                let group = request.group();
                let capacity = self.settings().queue_capacity;
                self.blocking(move |session| {
                    // A full queue drops its oldest entry: newer requests are more likely to
                    // still matter.
                    let _: Pushed = session.request_queue().push(
                        &NewQueuedRequest {
                            group,
                            keep_until: until,
                            request: &bytes,
                        },
                        capacity,
                    )?;
                    Ok(())
                })
                .await?;
                Ok(Outcome::Queued(reason))
            }
            QueuePolicy::KeepUntil(_) | QueuePolicy::Drop => Ok(Outcome::Dropped(reason)),
        }
    }

    /// Sends `request` once, or reports why it was not sent.
    async fn attempt(&self, request: ApiRequest, budget: WakeBudget) -> Result<Attempt, SyncError> {
        let route = request.route();
        if let ClockStatus::Skewed(offset) = self.clock_status()
            && route.credential().is_some()
        {
            return Err(SyncError::ClockSkew { offset });
        }
        if !budget.allows_request(&*self.timer) {
            return Ok(Attempt::NotSent(request, NotSentReason::BudgetExhausted));
        }
        let attached = self.attach(&request, budget).await?;
        let binary = request.to_binary(attached.header());
        let sent = self
            .within(budget, self.transport.request(route.class(), binary))
            .await;
        match sent {
            None => Err(SyncError::BudgetExhausted { route }),
            Some(Ok(response)) => {
                let response = Response::new(route, response);
                self.note_own_entry(&request, &response).await?;
                Ok(Attempt::Answered(response))
            }
            Some(Err(TransportError::NoRelay)) => {
                self.restore(attached).await?;
                Ok(Attempt::NotSent(request, NotSentReason::NoRelay))
            }
            Some(Err(TransportError::Ohttp(OhttpError::KeyMismatch))) => {
                // The mismatch is detected before anything is sent.
                self.restore(attached).await?;
                Err(SyncError::RelayKeyMismatch)
            }
            Some(Err(error)) => Err(SyncError::Transport(error)),
        }
    }

    /// Remembers the log position of a group entry this engine stored (a commit accepted or
    /// a stored message), so pulls pass over it, including posts sent from the queue and
    /// pulls after a restart.
    async fn note_own_entry(
        &self,
        request: &ApiRequest,
        response: &Response,
    ) -> Result<(), SyncError> {
        let Some(group) = request.group() else {
            return Ok(());
        };
        if !response.is_success() {
            return Ok(());
        }
        // Only commits and group messages name their group.
        let sequence = if request.route() == Route::Commit {
            match response.decode::<CommitResponse>() {
                Ok(CommitResponse::Accepted { sequence }) => Some(sequence),
                Ok(CommitResponse::Conflict { .. }) | Err(_) => None,
            }
        } else {
            response
                .decode::<MessageAccepted>()
                .ok()
                .map(|accepted| accepted.sequence)
        };
        let Some(sequence) = sequence else {
            return Ok(());
        };
        self.blocking(move |session| Ok(session.own_group_entries().remember(&group, sequence)?))
            .await
    }

    /// Sends `request` in its route's privacy class with the credential it needs, inside the
    /// budget. An anonymous request that cannot be sent (no relay, or not enough budget to
    /// start) is queued or dropped by the policy; nothing is ever sent direct instead.
    ///
    /// Not cancellation-safe: a token taken for the request is put back only if the
    /// transport reports that nothing was sent.
    ///
    /// # Errors
    ///
    /// [`SyncError::RelayKeyMismatch`] (raise the notice); [`SyncError::BudgetExhausted`] if
    /// the deadline came while the request was in flight, or before a non-anonymous request
    /// started; [`SyncError::NoRelay`] for a non-anonymous request with no relay;
    /// [`SyncError::NoCredential`]; [`SyncError::Transport`]; [`SyncError::Store`].
    pub async fn send(&self, request: ApiRequest, sending: Sending) -> Result<Outcome, SyncError> {
        let credentialed = request.route().credential().is_some();
        let retry = credentialed.then(|| request.clone());
        let attempt = match self.attempt(request, sending.budget).await? {
            // 401: the credential was missing or stale (a token of a rotated key, a proof of
            // a past bucket). One more try attaches a fresh one; a second 401, or any 403 (a
            // bad credential, a reused nullifier, a double spend), is answered as is and never
            // retried.
            Attempt::Answered(response) if response.status() == UNAUTHORIZED => match retry {
                Some(request) => self.attempt(request, sending.budget).await?,
                None => Attempt::Answered(response),
            },
            attempt @ (Attempt::Answered(_) | Attempt::NotSent(..)) => attempt,
        };
        match attempt {
            Attempt::Answered(response) => Ok(Outcome::Answered(response)),
            Attempt::NotSent(request, reason) => self.hold(request, sending.policy, reason).await,
        }
    }

    /// Sends a request whose answer is needed now: it is never queued.
    pub(crate) async fn exchange(
        &self,
        request: ApiRequest,
        budget: WakeBudget,
    ) -> Result<Response, SyncError> {
        let route = request.route();
        self.send(request, Sending::or_drop(budget))
            .await?
            .answered(route)
    }

    /// Sends what earlier wakes could not, oldest first, until the queue is empty, no relay
    /// is reachable, or the budget runs out. Expired entries are dropped first.
    ///
    /// # Errors
    ///
    /// As [`SyncEngine::send`]; [`SyncError::QueuedRequest`] for an entry that does not
    /// decode. On an error the failed request is dropped and the rest stay queued.
    pub async fn flush_queue(&self, budget: WakeBudget) -> Result<FlushReport, SyncError> {
        let now = self.now();
        let mut report = FlushReport {
            expired: self
                .blocking(move |session| Ok(session.request_queue().drop_expired(now)?))
                .await?,
            ..FlushReport::default()
        };
        loop {
            // Taken off the queue before sending: a crash mid-send loses the request rather
            // than sending it twice.
            let Some(queued) = self
                .blocking(|session| Ok(session.request_queue().take_oldest()?))
                .await?
            else {
                break;
            };
            let request = ApiRequest::from_queued(queued.group, &queued.request)?;
            match self.attempt(request, budget).await? {
                Attempt::Answered(response) => report.sent.push(response),
                Attempt::NotSent(..) => {
                    self.blocking(move |session| Ok(session.request_queue().put_back(&queued)?))
                        .await?;
                    break;
                }
            }
        }
        report.remaining = self.queued().await?;
        Ok(report)
    }

    /// Opens a live channel (`Route::Live`) through the stream route: chunked OHTTP, never
    /// direct. Not queued: the caller (pingers-meetup-session) retries.
    ///
    /// # Errors
    ///
    /// [`SyncError::NoRelay`]; [`SyncError::RelayKeyMismatch`]; [`SyncError::Transport`].
    pub async fn open_live(&self, request: ApiRequest) -> Result<Box<dyn DuplexStream>, SyncError> {
        let route = request.route();
        if route != Route::Live {
            return Err(SyncError::Transport(TransportError::ClassNotAllowed));
        }
        match self.transport.stream(request.to_binary(None)).await {
            Ok(stream) => Ok(stream),
            Err(TransportError::NoRelay) => Err(SyncError::NoRelay { route }),
            Err(TransportError::Ohttp(OhttpError::KeyMismatch)) => Err(SyncError::RelayKeyMismatch),
            Err(error) => Err(SyncError::Transport(error)),
        }
    }
}

/// The engine's futures must be `Send`: the client runs them on a multi-thread runtime.
#[cfg(test)]
#[expect(dead_code, reason = "compiled, never run: the bounds are the test")]
fn futures_are_send(engine: &SyncEngine, request: ApiRequest, sending: Sending) {
    fn send<T: Send>(_: &T) {}
    send(&engine.send(request, sending));
    send(&engine.flush_queue(sending.budget));
    send(&engine.credential_choice());
}
