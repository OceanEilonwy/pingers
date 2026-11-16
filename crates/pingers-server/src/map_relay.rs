// SPDX-License-Identifier: AGPL-3.0-only
//! Map downloads through the privacy relay (design B.12): the server forwards map requests,
//! still sealed to the map host's own OHTTP gateway, to that gateway and nowhere else.
//!
//! - `POST /v1/maps/ohttp`: the body is a `message/ohttp-req` for the map host's key; the
//!   answer is the map host's `message/ohttp-res`, unopened.
//! - `GET /v1/maps/ohttp-keys`: the map host's key config, so the phone can seal to it
//!   without contacting the map host itself.
//!
//! Both arrive only through the relay (they are anonymous routes). The server sees that some
//! anonymous client fetched map data, and the sizes, which the phone and the map host pad to
//! size classes; never which file. Without a map host there is nothing to forward to: 404.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use pingers_ohttp::{BinaryRequest, KeyConfig};
use pingers_time::Timer;

use crate::Shared;
use crate::config::MapHostGateway;
use crate::dispatch::{OHTTP_KEYS, Reply};

/// The map host gateway's answer media type.
const OHTTP_RESPONSE: &str = "message/ohttp-res";
/// The largest answer forwarded: the map host pads its answers to at most 1 MiB, plus the
/// encapsulation.
pub(crate) const MAX_ANSWER: usize = 1024 * 1024 + 4 * 1024;
/// The most a key config may hold.
const MAX_KEY_CONFIG: usize = 1024;

/// The map host's gateway, over HTTP.
pub(crate) struct MapRelay {
    client: reqwest::Client,
    gateway: MapHostGateway,
    timer: Arc<dyn Timer>,
    timeout: Duration,
}

/// The map host's gateway did not answer, or not with what was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unreachable;

impl MapRelay {
    /// A client for `gateway` that never follows redirects (a redirect would send the request
    /// somewhere the operator did not choose). `roots` are trusted besides the system's.
    pub(crate) fn new(
        gateway: MapHostGateway,
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
            gateway,
            timer,
            timeout,
        })
    }

    /// Sends `request` to `{gateway}{path}` and returns a 200 answer's body of at most `limit`
    /// bytes, within the timeout.
    async fn exchange(
        &self,
        request: reqwest::RequestBuilder,
        limit: usize,
    ) -> Result<Vec<u8>, Unreachable> {
        let fetched = async {
            let response = request.send().await.map_err(|_| Unreachable)?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(Unreachable);
            }
            let body = response.bytes().await.map_err(|_| Unreachable)?;
            if body.len() > limit {
                return Err(Unreachable);
            }
            Ok(body.to_vec())
        };
        tokio::select! {
            fetched = fetched => fetched,
            () = self.timer.sleep(self.timeout) => Err(Unreachable),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.gateway.origin())
    }
}

/// `POST /v1/maps/ohttp`.
pub(crate) async fn request(shared: &Shared, request: &BinaryRequest) -> Reply {
    let Some(relay) = &shared.map_relay else {
        return Reply::status(StatusCode::NOT_FOUND);
    };
    if request.body.is_empty() {
        return Reply::status(StatusCode::BAD_REQUEST);
    }
    let forwarded = relay
        .client
        .post(relay.url("/ohttp"))
        .header(reqwest::header::CONTENT_TYPE, "message/ohttp-req")
        .body(request.body.clone());
    match relay.exchange(forwarded, MAX_ANSWER).await {
        Ok(body) => Reply {
            status: StatusCode::OK,
            content_type: Some(OHTTP_RESPONSE),
            headers: Vec::new(),
            body,
        },
        Err(Unreachable) => Reply::status(StatusCode::BAD_GATEWAY),
    }
}

/// `GET /v1/maps/ohttp-keys`: only a config that decodes is passed on.
pub(crate) async fn keys(shared: &Shared) -> Reply {
    let Some(relay) = &shared.map_relay else {
        return Reply::status(StatusCode::NOT_FOUND);
    };
    let fetched = relay
        .exchange(relay.client.get(relay.url("/ohttp-keys")), MAX_KEY_CONFIG)
        .await;
    match fetched {
        Ok(body) if KeyConfig::decode(&body).is_ok() => Reply {
            status: StatusCode::OK,
            content_type: Some(OHTTP_KEYS),
            headers: Vec::new(),
            body,
        },
        Ok(_) | Err(Unreachable) => Reply::status(StatusCode::BAD_GATEWAY),
    }
}
