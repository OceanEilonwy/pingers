// SPDX-License-Identifier: AGPL-3.0-only
//! From a decoded request to the crate that answers it. No logic of its own (architecture §3):
//! the channel check of design B.12, then one call per route.

use std::sync::Arc;

use axum::http::StatusCode;
use pingers_ohttp::{BinaryRequest, BinaryResponse, Header, OhttpError};

use crate::route::{Channel, Resolved, Route, resolve};
use crate::tasks::RelayConfigKind;
use crate::wire::either;
use crate::{LiveRoute, Shared, board_routes, groups, mailbox_routes, quota_routes};

/// RFC 9458 key config media type.
pub(crate) const OHTTP_KEYS: &str = "application/ohttp-keys";

/// An answer, before it is sent directly or sealed for the relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    pub(crate) status: StatusCode,
    pub(crate) content_type: Option<&'static str>,
    /// Further response headers (a new group session).
    pub(crate) headers: Vec<(&'static str, String)>,
    pub(crate) body: Vec<u8>,
}

impl Reply {
    /// A status with no body.
    pub(crate) const fn status(status: StatusCode) -> Self {
        Self {
            status,
            content_type: None,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// The reply with one more header.
    pub(crate) fn with_header(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }

    /// The reply as an inner Binary HTTP response.
    pub(crate) fn into_binary(self) -> Result<BinaryResponse, OhttpError> {
        Ok(BinaryResponse {
            status: pingers_ohttp::StatusCode::new(self.status.as_u16())?,
            headers: self
                .content_type
                .map(|content_type| Header {
                    name: "content-type".to_owned(),
                    value: content_type.as_bytes().to_vec(),
                })
                .into_iter()
                .chain(self.headers.into_iter().map(|(name, value)| Header {
                    name: name.to_owned(),
                    value: value.into_bytes(),
                }))
                .collect(),
            body: self.body,
        })
    }
}

/// Answers a request that arrived on `channel` (direct or one-shot OHTTP; the live channel
/// has its own path through the gateway).
pub(crate) async fn answer(
    shared: &Arc<Shared>,
    channel: Channel,
    request: &BinaryRequest,
) -> Reply {
    match resolve(&request.method, &request.path) {
        Resolved::NotFound => Reply::status(StatusCode::NOT_FOUND),
        Resolved::MethodNotAllowed => Reply::status(StatusCode::METHOD_NOT_ALLOWED),
        // The route exists, but not on this channel: an anonymous route sent direct would have
        // revealed the caller's address, so it is refused rather than served.
        Resolved::Found(route) if !route.class().allowed_on(channel) => {
            Reply::status(StatusCode::MISDIRECTED_REQUEST)
        }
        Resolved::Found(route) => dispatch(shared, route, request).await,
    }
}

/// A route handler that may block (SQLite, proof verification).
type Blocking = fn(&Shared, &BinaryRequest) -> Result<Reply, Reply>;

/// Runs `handler` on the blocking pool.
async fn blocking(shared: &Arc<Shared>, request: &BinaryRequest, handler: Blocking) -> Reply {
    let shared = Arc::clone(shared);
    let request = request.clone();
    tokio::task::spawn_blocking(move || either(handler(&shared, &request)))
        .await
        .unwrap_or_else(|_| Reply::status(StatusCode::INTERNAL_SERVER_ERROR))
}

/// `GET /v1/ohttp/config`: the served key config (`application/ohttp-keys`, one config).
pub(crate) fn key_config_reply(shared: &Shared) -> Reply {
    Reply {
        status: StatusCode::OK,
        content_type: Some(OHTTP_KEYS),
        headers: Vec::new(),
        body: shared.served_config.encoded().to_vec(),
    }
}

/// One call per route, to the crate that owns it. Anonymous routes that take an Anon
/// credential check it first (`credential.rs`); Cap routes (board pages and revokes, mailbox
/// reads and deletes) carry their capability instead.
async fn dispatch(shared: &Arc<Shared>, route: Route, request: &BinaryRequest) -> Reply {
    let handler: Blocking = match route {
        Route::OhttpConfig => return key_config_reply(shared),
        Route::MapRequest => return crate::map_relay::request(shared, request).await,
        Route::MapKeys => return crate::map_relay::keys(shared).await,
        Route::PushRelayConfig => {
            return push_relay_config(shared, request, RelayConfigKind::Published).await;
        }
        Route::PushRelayNextConfig => {
            return push_relay_config(shared, request, RelayConfigKind::Next).await;
        }
        Route::BoardPage => return board_routes::page(shared, request),
        Route::BoardRevokeLease => return board_routes::revoke(shared, request),
        Route::CreateMailbox => return either(mailbox_routes::register(shared, request).await),
        Route::PostToMailbox => return either(mailbox_routes::post(shared, request).await),
        Route::ReadMailbox => return either(mailbox_routes::read(shared, request).await),
        Route::DeleteMailbox => return either(mailbox_routes::delete(shared, request).await),
        Route::DeleteMailboxMessages => {
            return either(mailbox_routes::delete_read(shared, request).await);
        }
        Route::MailboxPush => return either(mailbox_routes::replace_push(shared, request).await),
        Route::Live => match &shared.live {
            LiveRoute::Groups => groups::uplink,
            // A split channel's uplink frame; the chunked exchange goes through the gateway.
            LiveRoute::Custom(channel) => return live_uplink(channel.as_ref(), request),
        },
        Route::BoardSync => board_routes::sync,
        Route::CreateGroup => groups::create,
        Route::Commit => groups::commit,
        Route::PutSubscription => groups::put_subscription,
        Route::DeleteSubscription => groups::delete_subscription,
        Route::PostGroupMessage => groups::post,
        Route::ReadGroupMessages => groups::read,
        Route::GroupState => groups::state,
        Route::DeleteGroup => groups::delete,
        Route::Params => quota_routes::params,
        Route::CreateAccount => quota_routes::create_account,
        Route::Auth => quota_routes::auth,
        Route::DeleteAccount => quota_routes::delete_account,
        Route::RlnLeaf => quota_routes::rln_leaf,
        Route::RlnLeaves => quota_routes::rln_leaves,
        Route::PrivacyPassIssue => quota_routes::issue,
        Route::PrivacyPassKeys => quota_routes::keys,
    };
    blocking(shared, request, handler).await
}

/// `GET /v1/push-relays/{relay id}/config` and `.../config/next`: the listed relay's key
/// config, or the one it publishes next with its activation time, as the relay serves it. 404
/// for a relay this server does not list or, for the next key, when none is scheduled; 502 if
/// it did not answer with a config. Devices check its hash against their pins: the server
/// cannot substitute a key.
async fn push_relay_config(
    shared: &Arc<Shared>,
    request: &BinaryRequest,
    kind: RelayConfigKind,
) -> Reply {
    let path = request
        .path
        .split_once('?')
        .map_or(request.path.as_str(), |(path, _query)| path);
    let relay = path
        .strip_prefix("/v1/push-relays/")
        .and_then(|rest| match kind {
            RelayConfigKind::Published => rest.strip_suffix("/config"),
            RelayConfigKind::Next => rest.strip_suffix("/config/next"),
        })
        .and_then(|id| id.parse::<u16>().ok())
        .map(pingers_proto::RelayId::new);
    let Some(fetch) = relay.and_then(|relay| shared.tasks.push_relay_config(relay, kind)) else {
        return Reply::status(StatusCode::NOT_FOUND);
    };
    match fetch.await {
        Ok(None) => Reply::status(StatusCode::NOT_FOUND),
        Ok(Some(body)) => Reply {
            status: StatusCode::OK,
            content_type: Some(CBOR),
            headers: Vec::new(),
            body,
        },
        Err(_unreachable) => Reply::status(StatusCode::BAD_GATEWAY),
    }
}

/// The media type of a push relay's config, as the relay serves it.
const CBOR: &str = "application/cbor";

/// `POST /v1/groups/{id}/live` as a one-shot request: one uplink frame of a split channel.
fn live_uplink(channel: &dyn crate::LiveChannel, request: &BinaryRequest) -> Reply {
    if request.body.is_empty() {
        return Reply::status(StatusCode::BAD_REQUEST);
    }
    let frame = crate::LiveFrame::new(request.body.clone());
    match channel.send(request, frame) {
        Ok(()) => Reply::status(StatusCode::ACCEPTED),
        Err(crate::LiveRefused::NoChannel) => Reply::status(StatusCode::NOT_FOUND),
        Err(crate::LiveRefused::NotWired) => Reply::status(StatusCode::NOT_IMPLEMENTED),
    }
}
