// SPDX-License-Identifier: AGPL-3.0-only
//! A loopback HTTP server for tests that records requests and answers with a scripted status:
//! stands in for FCM and for UnifiedPush distributors.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;

/// One request the mock received.
#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub(crate) path_and_query: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

#[derive(Debug)]
struct Script {
    status: StatusCode,
    body: String,
    recorded: Vec<Recorded>,
}

/// The mock server; aborted when dropped.
pub(crate) struct MockHttp {
    address: SocketAddr,
    script: Arc<Mutex<Script>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockHttp {
    /// Starts a server answering `status` with `body` to every request. With `hang`, it never
    /// answers.
    pub(crate) async fn start(status: StatusCode, body: &str) -> std::io::Result<Self> {
        Self::start_with(status, body, Behaviour::Answer).await
    }

    /// Starts a server that accepts requests but never answers.
    pub(crate) async fn start_hanging() -> std::io::Result<Self> {
        Self::start_with(StatusCode::OK, "", Behaviour::Hang).await
    }

    async fn start_with(
        status: StatusCode,
        body: &str,
        behaviour: Behaviour,
    ) -> std::io::Result<Self> {
        let script = Arc::new(Mutex::new(Script {
            status,
            body: body.to_owned(),
            recorded: Vec::new(),
        }));
        let handler_script = Arc::clone(&script);
        let router = Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: Bytes| {
            let script = Arc::clone(&handler_script);
            async move {
                if behaviour == Behaviour::Hang {
                    std::future::pending::<()>().await;
                }
                let mut script = script.lock().unwrap_or_else(PoisonError::into_inner);
                script.recorded.push(Recorded {
                    path_and_query: uri
                        .path_and_query()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    headers,
                    body,
                });
                (script.status, script.body.clone()).into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            // The task is aborted when the mock is dropped; a serve error just ends it.
            drop(axum::serve(listener, router).await);
        });
        Ok(Self {
            address,
            script,
            task,
        })
    }

    /// `http://127.0.0.1:<port>` followed by `path`.
    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    /// Requests received so far.
    pub(crate) fn recorded(&self) -> Vec<Recorded> {
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recorded
            .clone()
    }
}

impl Drop for MockHttp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behaviour {
    Answer,
    Hang,
}
