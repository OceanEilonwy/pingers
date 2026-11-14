// SPDX-License-Identifier: AGPL-3.0-only
//! The server's background work, every task owned and cancelled on shutdown (style guide
//! §10): push jobs from mailbox and group wakes, live channel readers, and the periodic
//! sweeps (expiry, the hourly membership root, the daily issuer key rotation).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use pingers_delivery::{DeliveryError, Wake};
use pingers_mailbox::{
    ListedRelays, PUSH_CONTENT_TYPE, PUSH_PATH, PushJob, PushRelay, PushRequest, RelayAnswer,
    RelayDirectory as _, RelayTarget, RelayUnreachable, forward,
};
use pingers_proto::{
    GroupId, PushRelayConfig, RelayId, RelayRef, ScheduledPushRelayConfig, SubscriptionId,
    WireDecode as _, WireEncode as _,
};
use pingers_time::Timer;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use tokio::task::JoinSet;

use crate::groups::GroupService;
use crate::mailbox_routes::MailboxService;

/// The push relay over HTTP: `POST {url}/v1/push` with the server's bearer credential, the
/// CBOR `PushRequest` body, and a timeout on the injected timer.
pub(crate) struct HttpPushRelay {
    client: reqwest::Client,
    timer: Arc<dyn Timer>,
    timeout: Duration,
}

impl HttpPushRelay {
    /// A client that never follows redirects: a relay that redirects is misconfigured, and
    /// following would send the credential elsewhere. `roots` are trusted besides the
    /// system's (a self-hosted relay's CA, or the in-process harness's test CA); relay URLs
    /// are `https://`, or plain `http://` only to a loopback address (`PushRelayUrl`).
    pub(crate) fn new(
        timer: Arc<dyn Timer>,
        timeout: Duration,
        roots: Vec<reqwest::Certificate>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .tls_certs_merge(roots)
            .build()?;
        Ok(Self {
            client,
            timer,
            timeout,
        })
    }

    /// Posts `request` to `url` (a full push URL) with `authorization`.
    pub(crate) fn post(
        &self,
        url: &str,
        authorization: String,
        request: &PushRequest,
    ) -> impl Future<Output = Result<RelayAnswer, RelayUnreachable>> + Send + use<> {
        let sent = request.encode().ok().map(|body| {
            self.client
                .post(url)
                .header(AUTHORIZATION, authorization)
                .header(CONTENT_TYPE, PUSH_CONTENT_TYPE)
                .body(body)
                .send()
        });
        let timer = Arc::clone(&self.timer);
        let timeout = self.timeout;
        async move {
            // A request that cannot be encoded is never sent.
            let Some(sent) = sent else {
                return Err(RelayUnreachable);
            };
            tokio::select! {
                answer = sent => answer
                    .map(|response| RelayAnswer::from_status(response.status().as_u16()))
                    .map_err(|_| RelayUnreachable),
                () = timer.sleep(timeout) => Err(RelayUnreachable),
            }
        }
    }

    /// `GET {url}/v1/config` or `/v1/config/next` from a push relay: the config, at most
    /// [`MAX_RELAY_CONFIG_BYTES`], only on a 200 and only if it decodes as `kind`'s type.
    /// `Ok(None)` when the relay answers 404 for the next key: none is scheduled. No credential
    /// goes with it: the routes are public at the relay.
    pub(crate) fn fetch_config(
        &self,
        relay_url: &str,
        kind: RelayConfigKind,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, RelayUnreachable>> + Send + use<> {
        let url = format!("{}{}", relay_url.trim_end_matches('/'), kind.path());
        let sent = self.client.get(url).send();
        let timer = Arc::clone(&self.timer);
        let timeout = self.timeout;
        async move {
            let fetched = async {
                let response = sent.await.map_err(|_| RelayUnreachable)?;
                let status = response.status();
                if status == reqwest::StatusCode::NOT_FOUND && kind == RelayConfigKind::Next {
                    return Ok(None);
                }
                if status != reqwest::StatusCode::OK {
                    return Err(RelayUnreachable);
                }
                let body = response.bytes().await.map_err(|_| RelayUnreachable)?;
                if body.len() > MAX_RELAY_CONFIG_BYTES || !kind.decodes(&body) {
                    return Err(RelayUnreachable);
                }
                Ok(Some(body.to_vec()))
            };
            tokio::select! {
                fetched = fetched => fetched,
                () = timer.sleep(timeout) => Err(RelayUnreachable),
            }
        }
    }
}

/// Which of a push relay's two key configs (design D, push relay API).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayConfigKind {
    /// `GET /v1/config`: the key devices seal to now, a `PushRelayConfig`.
    Published,
    /// `GET /v1/config/next`: the key scheduled next, a `ScheduledPushRelayConfig`.
    Next,
}

impl RelayConfigKind {
    const fn path(self) -> &'static str {
        match self {
            Self::Published => "/v1/config",
            Self::Next => "/v1/config/next",
        }
    }

    fn decodes(self, body: &[u8]) -> bool {
        match self {
            Self::Published => PushRelayConfig::decode(body).is_ok(),
            Self::Next => ScheduledPushRelayConfig::decode(body).is_ok(),
        }
    }
}

/// The most a push relay's config may hold: 41 bytes of key, a few platforms, an operator.
pub(crate) const MAX_RELAY_CONFIG_BYTES: usize = 4096;

/// `{url}/v1/push`, without doubling a trailing slash.
pub(crate) fn push_url(target: &RelayTarget) -> String {
    format!("{}{PUSH_PATH}", target.url.as_str().trim_end_matches('/'))
}

impl PushRelay for HttpPushRelay {
    fn push(
        &self,
        target: &RelayTarget,
        request: &PushRequest,
    ) -> impl Future<Output = Result<RelayAnswer, RelayUnreachable>> + Send {
        self.post(
            &push_url(target),
            target.credential.authorization(),
            request,
        )
    }
}

/// What the background tasks work with.
pub(crate) struct TaskParts {
    pub(crate) relay: HttpPushRelay,
    pub(crate) timer: Arc<dyn Timer>,
    pub(crate) mailboxes: Arc<Mutex<MailboxService>>,
    pub(crate) groups: Arc<GroupService>,
    /// Where group wakes go (the same relays as mailbox wakes).
    pub(crate) directory: ListedRelays,
}

/// A subscription's turn, held until dropped.
type Turn = tokio::sync::OwnedMutexGuard<()>;

/// One subscription, as the key of its turn.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SubscriptionKey {
    group: GroupId,
    subscription: SubscriptionId,
}

/// A turn per subscription, taken to send a push to it or to delete it. Pushes to different
/// subscriptions run side by side; for one subscription, a delete waits for the push in
/// flight, and a push that waited sees the delete (or the `410 Gone` the push before it got).
#[derive(Default)]
struct SubscriptionTurns {
    /// Only subscriptions with a push or a delete under way are listed: the entry lives as
    /// long as someone holds or waits for the turn.
    turns: Mutex<HashMap<SubscriptionKey, Weak<tokio::sync::Mutex<()>>>>,
}

impl SubscriptionTurns {
    /// Waits for and takes the turn of `subscription` in `group`; it is given up on drop.
    async fn take(&self, group: GroupId, subscription: SubscriptionId) -> Turn {
        self.of(group, subscription).lock_owned().await
    }

    /// As [`Self::take`], blocking the thread: only for the blocking pool.
    fn take_blocking(&self, group: GroupId, subscription: SubscriptionId) -> Turn {
        self.of(group, subscription).blocking_lock_owned()
    }

    /// The turn of `subscription` in `group`, shared with whoever holds or awaits it now.
    fn of(&self, group: GroupId, subscription: SubscriptionId) -> Arc<tokio::sync::Mutex<()>> {
        let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
        turns.retain(|_, turn| turn.strong_count() > 0);
        let key = SubscriptionKey {
            group,
            subscription,
        };
        if let Some(turn) = turns.get(&key).and_then(Weak::upgrade) {
            return turn;
        }
        let turn = Arc::new(tokio::sync::Mutex::new(()));
        turns.insert(key, Arc::downgrade(&turn));
        turn
    }

    /// How many hold or await the turn of `subscription` in `group` now.
    #[cfg(test)]
    fn waiting(&self, group: GroupId, subscription: SubscriptionId) -> usize {
        self.turns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&SubscriptionKey {
                group,
                subscription,
            })
            .map_or(0, Weak::strong_count)
    }
}

/// What became of one group wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WakeOutcome {
    /// The relay was asked; this is its answer. On `Gone` the subscription has been deleted.
    Sent(Result<RelayAnswer, RelayUnreachable>),
    /// The subscription was deleted, replaced or expired, or its group deleted, after the wake
    /// was made: nothing was sent.
    NotSubscribed,
    /// Whether the subscription still stands could not be read: nothing was sent.
    Unchecked,
}

/// Sends group wakes and deletes subscriptions, one turn at a time per subscription, so that
/// a subscription is never pushed after its delete is acknowledged or after the relay
/// answered `410 Gone` for it.
#[derive(Clone)]
pub(crate) struct GroupPusher {
    relay: Arc<HttpPushRelay>,
    groups: Arc<GroupService>,
    turns: Arc<SubscriptionTurns>,
}

impl GroupPusher {
    pub(crate) fn new(relay: Arc<HttpPushRelay>, groups: Arc<GroupService>) -> Self {
        Self {
            relay,
            groups,
            turns: Arc::new(SubscriptionTurns::default()),
        }
    }

    /// Sends `wake` to `target` now, if its subscription still stands when its turn comes;
    /// on `410 Gone` deletes the subscription before giving up the turn.
    pub(crate) async fn send(
        &self,
        group: GroupId,
        wake: Wake,
        target: &RelayTarget,
    ) -> WakeOutcome {
        let subscription = wake.subscription;
        let request = PushRequest {
            key_id: wake.push.key_id,
            blob: wake.push.blob,
            bind: wake.bind,
            urgency: wake.urgency,
            handle: wake.push.handle,
        };
        let _turn = self.turns.take(group, subscription).await;
        let groups = Arc::clone(&self.groups);
        match tokio::task::spawn_blocking(move || groups.still_subscribed(&group, &wake)).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return WakeOutcome::NotSubscribed,
            Ok(Err(_)) | Err(_) => return WakeOutcome::Unchecked,
        }
        let answer = self.relay.push(target, &request).await;
        if let Ok(RelayAnswer::Gone) = answer {
            // A failure here leaves the subscription for the next 410 or its expiry.
            let groups = Arc::clone(&self.groups);
            let _ = tokio::task::spawn_blocking(move || {
                groups.remove_subscription(&group, &subscription)
            })
            .await;
        }
        WakeOutcome::Sent(answer)
    }

    /// `DELETE /v1/groups/{id}/subs/{sub_id}`: deletes the subscription once no push to it is
    /// in flight, so none reaches the relay after this returns. Blocks (on that push and on
    /// SQLite): call it from the blocking pool, never from async code.
    ///
    /// # Errors
    ///
    /// As [`pingers_delivery::Delivery::remove_subscription`].
    pub(crate) fn remove_subscription(
        &self,
        group: &GroupId,
        subscription: &SubscriptionId,
    ) -> Result<(), DeliveryError> {
        let _turn = self.turns.take_blocking(*group, *subscription);
        self.groups.remove_subscription(group, subscription)
    }

    /// How many sends and deletes hold or await the turn of `subscription` in `group`.
    #[cfg(test)]
    pub(crate) fn waiting(&self, group: GroupId, subscription: SubscriptionId) -> usize {
        self.turns.waiting(group, subscription)
    }
}

/// Owns the background tasks.
pub(crate) struct Tasks {
    relay: Arc<HttpPushRelay>,
    timer: Arc<dyn Timer>,
    mailboxes: Arc<Mutex<MailboxService>>,
    pusher: GroupPusher,
    directory: ListedRelays,
    running: Mutex<JoinSet<()>>,
}

impl Tasks {
    pub(crate) fn new(parts: TaskParts) -> Self {
        let TaskParts {
            relay,
            timer,
            mailboxes,
            groups,
            directory,
        } = parts;
        let relay = Arc::new(relay);
        Self {
            pusher: GroupPusher::new(Arc::clone(&relay), groups),
            relay,
            timer,
            mailboxes,
            directory,
            running: Mutex::new(JoinSet::new()),
        }
    }

    /// Group wakes and subscription deletes, in turn per subscription.
    pub(crate) const fn pusher(&self) -> &GroupPusher {
        &self.pusher
    }

    /// Runs `task` until it ends or the server shuts down.
    pub(crate) fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        // Reap finished tasks so the set holds only live ones.
        while running.try_join_next().is_some() {}
        running.spawn(task);
    }

    /// Sends a mailbox wake after its delay; a 410 deletes the blob (pingers-mailbox's
    /// `forward`). The outcome is not logged.
    pub(crate) fn forward(&self, job: PushJob) {
        let relay = Arc::clone(&self.relay);
        let timer = Arc::clone(&self.timer);
        let mailboxes = Arc::clone(&self.mailboxes);
        self.spawn(async move {
            let _ = forward(&job, &*relay, &timer, &*mailboxes).await;
        });
    }

    /// Sends a group wake to its relay after its delay (pingers-delivery's wake rule), unless
    /// its subscription has gone in the meantime; a 410 deletes the subscription. A relay this
    /// server has no entry for is skipped, and nothing is retried (design B.4.9).
    pub(crate) fn wake(&self, group: GroupId, wake: &Wake) {
        let Some(target) = self.directory.resolve(&wake.push.relay) else {
            return;
        };
        let pusher = self.pusher.clone();
        let timer = Arc::clone(&self.timer);
        let wake = wake.clone();
        self.spawn(async move {
            if !wake.delay.is_zero() {
                timer.sleep(wake.delay).await;
            }
            let _ = pusher.send(group, wake, &target).await;
        });
    }

    /// The `kind` key config of the listed push relay `relay`, fetched from it now: `None` if
    /// this server lists no such relay, `Some(Err)` if it did not answer with a config,
    /// `Some(Ok(None))` if it has no next key scheduled.
    pub(crate) fn push_relay_config(
        &self,
        relay: RelayId,
        kind: RelayConfigKind,
    ) -> Option<impl Future<Output = Result<Option<Vec<u8>>, RelayUnreachable>> + Send + use<>>
    {
        let target = self.directory.resolve(&RelayRef::Listed(relay))?;
        Some(self.relay.fetch_config(target.url.as_str(), kind))
    }

    /// Runs `sweep` every `interval`.
    pub(crate) fn every<F, S>(&self, interval: Duration, mut sweep: S)
    where
        S: FnMut() -> F + Send + 'static,
        F: Future<Output = ()> + Send,
    {
        let timer = Arc::clone(&self.timer);
        self.spawn(async move {
            loop {
                timer.sleep(interval).await;
                sweep().await;
            }
        });
    }

    /// Cancels every task.
    pub(crate) fn abort_all(&self) {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .abort_all();
    }
}
