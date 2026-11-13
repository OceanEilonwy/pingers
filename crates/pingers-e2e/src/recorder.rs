// SPDX-License-Identifier: AGPL-3.0-only
//! Every inner request the devices sent through the relay, and every answer: exactly what the
//! server decapsulated and returned.

use std::sync::{Arc, Mutex, PoisonError};

use pingers_ohttp::{BinaryRequest, BinaryResponse};
use pingers_transport::{OhttpTransport, PrivacyClass, Transport as _};

use crate::E2eError;

/// One request and its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    /// Which simulated device sent it.
    pub device: String,
    /// The inner method.
    pub method: String,
    /// The inner path, with its query.
    pub path: String,
    /// The inner headers' names and values.
    pub headers: Vec<(String, Vec<u8>)>,
    /// The inner request body, if bodies are kept.
    pub request_body: Option<Vec<u8>>,
    /// The inner request body's length.
    pub request_len: usize,
    /// The answer's status.
    pub status: u16,
    /// The answer's body, if bodies are kept.
    pub response_body: Option<Vec<u8>>,
    /// The answer body's length.
    pub response_len: usize,
}

/// A shared log of [`Exchange`]s.
#[derive(Debug, Default)]
pub struct Recorder {
    keep_bodies: bool,
    exchanges: Mutex<Vec<Exchange>>,
}

impl Recorder {
    /// A recorder that keeps request and response bodies (for the privacy scans).
    #[must_use]
    pub fn keeping_bodies() -> Arc<Self> {
        Arc::new(Self {
            keep_bodies: true,
            exchanges: Mutex::default(),
        })
    }

    /// A recorder that keeps only sizes (for load runs).
    #[must_use]
    pub fn sizes_only() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Everything recorded so far, oldest first.
    #[must_use]
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn push(&self, exchange: Exchange) {
        self.exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(exchange);
    }
}

/// A device's anonymous channel: the OHTTP transport, recording as it goes.
pub(crate) struct Net {
    pub(crate) device: String,
    pub(crate) transport: Arc<OhttpTransport>,
    pub(crate) recorder: Arc<Recorder>,
    pub(crate) credentials: Arc<crate::TokenPool>,
}

impl Net {
    /// Sends one anonymous request through the relay and records it.
    pub(crate) async fn send(
        &self,
        mut request: BinaryRequest,
    ) -> Result<BinaryResponse, E2eError> {
        self.credentials.attach(&mut request).await?;
        let keep = self.recorder.keep_bodies;
        let mut exchange = Exchange {
            device: self.device.clone(),
            method: request.method.clone(),
            path: request.path.clone(),
            headers: request
                .headers
                .iter()
                .map(|header| (header.name.clone(), header.value.clone()))
                .collect(),
            request_body: keep.then(|| request.body.clone()),
            request_len: request.body.len(),
            status: 0,
            response_body: None,
            response_len: 0,
        };
        let response = self
            .transport
            .request(PrivacyClass::Anonymous, request)
            .await?;
        exchange.status = response.status.get();
        exchange.response_len = response.body.len();
        exchange.response_body = keep.then(|| response.body.clone());
        self.recorder.push(exchange);
        Ok(response)
    }
}
