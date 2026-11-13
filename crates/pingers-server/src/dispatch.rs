// SPDX-License-Identifier: AGPL-3.0-only
//! From a decoded request to the crate that answers it. No logic of its own (architecture §3):
//! the channel check of design B.12, then one call per route.

use axum::http::StatusCode;
use pingers_ohttp::{BinaryRequest, BinaryResponse, Header, OhttpError};

use crate::Shared;
use crate::route::{Channel, Resolved, Route, resolve};
use crate::wire::either;
use crate::{board_routes, mailbox_routes};

/// RFC 9458 key config media type.
pub(crate) const OHTTP_KEYS: &str = "application/ohttp-keys";

/// An answer, before it is sent directly or sealed for the relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    pub(crate) status: StatusCode,
    pub(crate) content_type: Option<&'static str>,
    pub(crate) body: Vec<u8>,
}

impl Reply {
    /// A status with no body.
    pub(crate) const fn status(status: StatusCode) -> Self {
        Self {
            status,
            content_type: None,
            body: Vec::new(),
        }
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
                .collect(),
            body: self.body,
        })
    }
}

/// Answers a request that arrived on `channel` (direct or one-shot OHTTP; the live channel
/// has its own path through the gateway).
pub(crate) async fn answer(shared: &Shared, channel: Channel, request: &BinaryRequest) -> Reply {
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

/// One call per route, to the crate that owns it.
///
/// Anonymous routes are served without checking the RLN proof or Privacy Pass token yet:
/// that check is pingers-quota's, which has no API on main.
async fn dispatch(shared: &Shared, route: Route, request: &BinaryRequest) -> Reply {
    match route {
        Route::OhttpConfig => Reply {
            status: StatusCode::OK,
            content_type: Some(OHTTP_KEYS),
            body: shared.served_config.encoded().to_vec(),
        },
        Route::BoardSync => board_routes::sync(shared, request),
        Route::BoardPage => board_routes::page(shared, request),
        Route::BoardRevokeLease => board_routes::revoke(shared, request),
        Route::CreateMailbox => either(mailbox_routes::register(shared, request).await),
        Route::PostToMailbox => either(mailbox_routes::post(shared, request).await),
        Route::ReadMailbox => either(mailbox_routes::read(shared, request).await),
        Route::DeleteMailbox => either(mailbox_routes::delete(shared, request).await),
        // Waiting on pingers-delivery (group logs, subscriptions; the live channel is
        // `LiveChannel`).
        Route::CreateGroup
        | Route::Commit
        | Route::PutSubscription
        | Route::DeleteSubscription
        | Route::PostGroupMessage
        | Route::ReadGroupMessages
        | Route::GroupState
        | Route::DeleteGroup
        | Route::Live
        // Waiting on pingers-quota (accounts gate leaf registration and token issuance; the
        // membership tree; Privacy Pass).
        | Route::CreateAccount
        | Route::Auth
        | Route::DeleteAccount
        | Route::RlnLeaf
        | Route::RlnLeaves
        | Route::PrivacyPassIssue
        | Route::PrivacyPassKeys
        // Waiting on pingers-quota, whose credential policy it publishes.
        | Route::Params => Reply::status(StatusCode::NOT_IMPLEMENTED),
    }
}
