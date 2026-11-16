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

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::http::StatusCode;
use pingers_ohttp::{BinaryRequest, KeyConfig};
use pingers_time::{Clock as _, Timer, UnixSeconds};

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
/// How long the map host's key config is served from memory before it is fetched again: the
/// route needs no credential, so it must not reach the map host on every request.
pub(crate) const KEY_CACHE_TTL: Duration = Duration::from_mins(5);
/// Map requests forwarded per minute, from all clients together: the route needs no credential
/// (it is anonymous and relayed), so this bounds what it can make the server and the map host
/// do. Far above what downloads need (a region is some tens of 256 KiB pieces).
pub(crate) const FORWARDS_PER_MINUTE: u32 = 1_800;
/// Map requests forwarded at once.
pub(crate) const CONCURRENT_FORWARDS: usize = 32;

/// The map host's gateway, over HTTP.
pub(crate) struct MapRelay {
    client: reqwest::Client,
    gateway: MapHostGateway,
    timer: Arc<dyn Timer>,
    timeout: Duration,
    /// The last key config fetched, and until when it is served without asking again.
    keys: Mutex<Option<(Vec<u8>, UnixSeconds)>>,
    /// The current minute's forwards.
    window: Mutex<(UnixSeconds, u32)>,
    /// Forwards in flight.
    in_flight: tokio::sync::Semaphore,
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
            keys: Mutex::new(None),
            window: Mutex::new((UnixSeconds::EPOCH, 0)),
            in_flight: tokio::sync::Semaphore::new(CONCURRENT_FORWARDS),
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
            crate::capped::body(response, limit)
                .await
                .ok_or(Unreachable)
        };
        tokio::select! {
            fetched = fetched => fetched,
            () = self.timer.sleep(self.timeout) => Err(Unreachable),
        }
    }

    /// Counts one forward against the minute's allowance; `false` when it is used up.
    fn admit(&self, now: UnixSeconds) -> bool {
        let mut window = self.window.lock().unwrap_or_else(PoisonError::into_inner);
        if now.saturating_duration_since(window.0) >= Duration::from_mins(1) {
            *window = (now, 0);
        }
        if window.1 >= FORWARDS_PER_MINUTE {
            return false;
        }
        window.1 = window.1.saturating_add(1);
        true
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
    let Ok(_permit) = relay.in_flight.try_acquire() else {
        return Reply::status(StatusCode::TOO_MANY_REQUESTS);
    };
    if !relay.admit(shared.clock.now()) {
        return Reply::status(StatusCode::TOO_MANY_REQUESTS);
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

/// `GET /v1/maps/ohttp-keys`: only a config that decodes is passed on, and it is kept for
/// [`KEY_CACHE_TTL`], so this credential-free route asks the map host at most that often.
pub(crate) async fn keys(shared: &Shared) -> Reply {
    let Some(relay) = &shared.map_relay else {
        return Reply::status(StatusCode::NOT_FOUND);
    };
    let now = shared.clock.now();
    let cached = relay
        .keys
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .filter(|(_, until)| now < *until)
        .map(|(body, _)| body.clone());
    let body = if let Some(body) = cached {
        body
    } else {
        let fetched = relay
            .exchange(relay.client.get(relay.url("/ohttp-keys")), MAX_KEY_CONFIG)
            .await;
        match fetched {
            Ok(body) if KeyConfig::decode(&body).is_ok() => {
                if let Some(until) = now.checked_add(KEY_CACHE_TTL) {
                    *relay.keys.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some((body.clone(), until));
                }
                body
            }
            Ok(_) | Err(Unreachable) => return Reply::status(StatusCode::BAD_GATEWAY),
        }
    };
    Reply {
        status: StatusCode::OK,
        content_type: Some(OHTTP_KEYS),
        headers: Vec::new(),
        body,
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    /// At most `FORWARDS_PER_MINUTE` forwards in a minute, all clients together; the next
    /// minute starts afresh.
    #[test]
    fn forwards_are_limited_per_minute() -> Result<(), Box<dyn std::error::Error>> {
        let clock = pingers_time::ManualClock::new(UnixSeconds::new(1_800_000_000));
        let relay = MapRelay::new(
            MapHostGateway::from_settings(Some("https://maps.example"), None)?
                .ok_or("no gateway")?,
            Arc::new(clock.clone()),
            Duration::from_secs(1),
            Vec::new(),
        )?;
        let now = UnixSeconds::new(1_800_000_000);
        assert!((0..FORWARDS_PER_MINUTE).all(|_| relay.admit(now)));
        assert!(!relay.admit(now));
        let later = now.checked_add(Duration::from_mins(1)).ok_or("time")?;
        assert!(relay.admit(later));
        Ok(())
    }
}
