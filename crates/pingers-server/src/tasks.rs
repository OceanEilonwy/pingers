// SPDX-License-Identifier: AGPL-3.0-only
//! The server's background work, every task owned and cancelled on shutdown (style guide
//! §10): push jobs from mailbox and group wakes, live channel readers, and the periodic
//! sweeps (expiry, the hourly membership root, the daily issuer key rotation).

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pingers_delivery::Wake;
use pingers_mailbox::{
    ListedRelays, PUSH_CONTENT_TYPE, PUSH_PATH, PushJob, PushRelay, PushRequest, RelayAnswer,
    RelayDirectory as _, RelayTarget, RelayUnreachable, forward,
};
use pingers_proto::{GroupId, WireEncode as _};
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
}

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

/// Owns the background tasks.
pub(crate) struct Tasks {
    relay: Arc<HttpPushRelay>,
    timer: Arc<dyn Timer>,
    mailboxes: Arc<Mutex<MailboxService>>,
    groups: Arc<GroupService>,
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
        Self {
            relay: Arc::new(relay),
            timer,
            mailboxes,
            groups,
            directory,
            running: Mutex::new(JoinSet::new()),
        }
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

    /// Sends a group wake to its relay after its delay (pingers-delivery's wake rule); a 410
    /// deletes the subscription. A relay this server has no entry for is skipped, and nothing
    /// is retried (design B.4.9).
    pub(crate) fn wake(&self, group: GroupId, wake: &Wake) {
        let Some(target) = self.directory.resolve(&wake.push.relay) else {
            return;
        };
        let request = PushRequest {
            key_id: wake.push.key_id,
            blob: wake.push.blob,
            bind: wake.bind,
            urgency: wake.urgency,
            handle: wake.push.handle,
        };
        let relay = Arc::clone(&self.relay);
        let timer = Arc::clone(&self.timer);
        let groups = Arc::clone(&self.groups);
        let subscription = wake.subscription;
        let delay = wake.delay;
        self.spawn(async move {
            if !delay.is_zero() {
                timer.sleep(delay).await;
            }
            if let Ok(RelayAnswer::Gone) = relay.push(&target, &request).await {
                // A failure here leaves the blob for the next 410 or its expiry.
                let _ = tokio::task::spawn_blocking(move || {
                    groups.remove_subscription(&group, &subscription)
                })
                .await;
            }
        });
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
