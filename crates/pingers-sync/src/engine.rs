// SPDX-License-Identifier: AGPL-3.0-only
//! The engine: sends one request with the right privacy class and credential, inside the wake
//! budget, and queues or drops what could not be sent.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pingers_ohttp::{BinaryResponse, OhttpError};
use pingers_proto::{ServerId, WireDecode};
use pingers_store::Session;
use pingers_time::{Clock, Timer, UnixSeconds};
use pingers_transport::{DuplexStream, PrivacyClass, Transport, TransportError};

use crate::credential::{
    Attached, CredentialChoice, CredentialPolicy, ProcessRole, ProofInput, choose_credential,
    proof_header, prove_request, readiness, token_header,
};
use crate::queue::{Pushed, QueueCapacity, Queued, RequestQueue};
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
    /// The store session this wake (or app run) holds.
    pub session: Arc<Session>,
    /// Wall time, for buckets, expiries and group request signatures.
    pub clock: Arc<dyn Clock>,
    /// Monotonic time, for the wake budget.
    pub timer: Arc<dyn Timer>,
    /// Randomness.
    pub rng: SyncRng,
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
    fn new(route: Route, response: BinaryResponse) -> Self {
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
    pub(crate) session: Arc<Session>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) timer: Arc<dyn Timer>,
    pub(crate) rng: Arc<Mutex<SyncRng>>,
    pub(crate) settings: Mutex<SyncSettings>,
    queue: Mutex<RequestQueue>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `work` against the store on the blocking pool (SQLCipher blocks).
pub(crate) async fn blocking<T: Send + 'static>(
    session: &Arc<Session>,
    work: impl FnOnce(&Session) -> Result<T, SyncError> + Send + 'static,
) -> Result<T, SyncError> {
    let session = Arc::clone(session);
    tokio::task::spawn_blocking(move || work(&session))
        .await
        .map_err(|_| SyncError::Internal)?
}

impl SyncEngine {
    /// An engine over `parts`.
    #[must_use]
    pub fn new(parts: SyncParts, settings: SyncSettings) -> Self {
        Self {
            transport: parts.transport,
            session: parts.session,
            clock: parts.clock,
            timer: parts.timer,
            rng: Arc::new(Mutex::new(parts.rng)),
            queue: Mutex::new(RequestQueue::new(settings.queue_capacity)),
            settings: Mutex::new(settings),
        }
    }

    /// Replaces the credential policy, after fetching `Params` again or a prover self-test.
    pub fn set_credential_policy(&self, policy: CredentialPolicy) {
        lock(&self.settings).credentials = policy;
    }

    pub(crate) fn settings(&self) -> SyncSettings {
        lock(&self.settings).clone()
    }

    pub(crate) fn now(&self) -> UnixSeconds {
        self.clock.now()
    }

    /// Draws from the engine's randomness.
    pub(crate) fn with_rng<T>(&self, use_rng: impl FnOnce(&mut SyncRng) -> T) -> T {
        use_rng(&mut lock(&self.rng))
    }

    /// How many requests wait for a later wake.
    #[must_use]
    pub fn queued(&self) -> usize {
        lock(&self.queue).len()
    }

    /// The credential the next anonymous request would carry, for "What your server sees"
    /// (a Privacy Pass choice in the main app is a fallback, design B.4.8).
    ///
    /// # Errors
    ///
    /// [`SyncError::NoCredential`] if none is possible; [`SyncError::Store`].
    pub async fn credential_choice(&self) -> Result<CredentialChoice, SyncError> {
        let settings = self.settings();
        let now = self.now();
        let readiness = blocking(&self.session, move |session| {
            Ok(readiness(session.rln_identity().load()?.as_ref(), now))
        })
        .await?;
        choose_credential(settings.role, &settings.credentials, readiness)
    }

    /// The credential for `request`, made or taken now.
    async fn attach(&self, request: &ApiRequest) -> Result<Attached, SyncError> {
        let Some(scope) = request.route().credential() else {
            return Ok(Attached::Nothing);
        };
        let settings = self.settings();
        if self.credential_choice().await? == CredentialChoice::MembershipProof {
            let rng = Arc::clone(&self.rng);
            let request = request.clone();
            let bucket = self.now().bucket();
            let proof = blocking(&self.session, move |session| {
                prove_request(
                    session,
                    &mut lock(&rng),
                    &ProofInput {
                        server: &settings.server,
                        scope,
                        bucket,
                        method: request.method(),
                        path: request.path(),
                        body: request.body(),
                    },
                )
            })
            .await?;
            if let Some(auth) = proof {
                return Ok(Attached::Proof {
                    header: proof_header(&auth)?,
                });
            }
            // The bucket's message ids are spent: a token may still be accepted.
            let accepts_tokens = self
                .settings()
                .credentials
                .accepted
                .contains(&pingers_proto::AnonymousCredential::PrivacyPass);
            if !accepts_tokens {
                return Err(SyncError::NoCredential);
            }
        }
        let token = blocking(&self.session, |session| {
            Ok(session.privacy_pass_tokens().take()?)
        })
        .await?
        .ok_or(SyncError::NoCredential)?;
        Ok(Attached::Token {
            header: token_header(&token),
            token,
        })
    }

    /// Returns an unspent token to the store: the request carrying it never left the device.
    async fn restore(&self, attached: Attached) -> Result<(), SyncError> {
        if let Attached::Token { token, .. } = attached {
            blocking(&self.session, move |session| {
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
    fn hold(
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
                // A full queue drops its oldest entry: newer requests are more likely to
                // still matter.
                let _: Pushed = lock(&self.queue).push(request, until);
                Ok(Outcome::Queued(reason))
            }
            QueuePolicy::KeepUntil(_) | QueuePolicy::Drop => Ok(Outcome::Dropped(reason)),
        }
    }

    /// Sends `request` once, or reports why it was not sent.
    async fn attempt(&self, request: ApiRequest, budget: WakeBudget) -> Result<Attempt, SyncError> {
        let route = request.route();
        if !budget.allows_request(&*self.timer) {
            return Ok(Attempt::NotSent(request, NotSentReason::BudgetExhausted));
        }
        let attached = self.attach(&request).await?;
        let binary = request.to_binary(attached.header());
        let sent = self
            .within(budget, self.transport.request(route.class(), binary))
            .await;
        match sent {
            None => Err(SyncError::BudgetExhausted { route }),
            Some(Ok(response)) => Ok(Attempt::Answered(Response::new(route, response))),
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
        match self.attempt(request, sending.budget).await? {
            Attempt::Answered(response) => Ok(Outcome::Answered(response)),
            Attempt::NotSent(request, reason) => self.hold(request, sending.policy, reason),
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
    /// As [`SyncEngine::send`]; on an error the failed request is dropped and the rest stay
    /// queued.
    pub async fn flush_queue(&self, budget: WakeBudget) -> Result<FlushReport, SyncError> {
        let mut report = FlushReport {
            expired: lock(&self.queue).drop_expired(self.now()),
            ..FlushReport::default()
        };
        loop {
            let Some(queued) = lock(&self.queue).pop() else {
                break;
            };
            let keep_until = queued.keep_until;
            match self.attempt(queued.request.clone(), budget).await {
                Ok(Attempt::Answered(response)) => report.sent.push(response),
                Ok(Attempt::NotSent(request, _)) => {
                    lock(&self.queue).push_front(Queued {
                        request,
                        keep_until,
                    });
                    break;
                }
                Err(error) => {
                    report.remaining = self.queued();
                    return Err(error);
                }
            }
        }
        report.remaining = self.queued();
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
