// SPDX-License-Identifier: AGPL-3.0-only
//! The server's background work, every task owned and cancelled on shutdown (style guide
//! §10): push jobs from mailbox wakes, and the expiry sweep.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pingers_mailbox::{
    PUSH_PATH, PushJob, PushRelay, PushRequest, RelayAnswer, RelayTarget, RelayUnreachable, forward,
};
use pingers_time::Timer;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use tokio::task::JoinSet;

use crate::mailbox_routes::MailboxService;

/// The push relay over HTTP: `POST {url}/v1/push` with the server's bearer credential, the
/// JSON body, and a timeout on the injected timer.
pub(crate) struct HttpPushRelay {
    client: reqwest::Client,
    timer: Arc<dyn Timer>,
    timeout: Duration,
}

impl HttpPushRelay {
    /// A client that never follows redirects: a relay that redirects is misconfigured, and
    /// following would send the credential elsewhere.
    pub(crate) fn new(timer: Arc<dyn Timer>, timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
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
        let sent = self
            .client
            .post(url)
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(request.to_json())
            .send();
        let timer = Arc::clone(&self.timer);
        let timeout = self.timeout;
        async move {
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

/// Owns the background tasks.
pub(crate) struct Tasks {
    relay: Arc<HttpPushRelay>,
    timer: Arc<dyn Timer>,
    mailboxes: Arc<Mutex<MailboxService>>,
    running: Mutex<JoinSet<()>>,
}

impl Tasks {
    pub(crate) fn new(
        relay: HttpPushRelay,
        timer: Arc<dyn Timer>,
        mailboxes: Arc<Mutex<MailboxService>>,
    ) -> Self {
        Self {
            relay: Arc::new(relay),
            timer,
            mailboxes,
            running: Mutex::new(JoinSet::new()),
        }
    }

    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
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
