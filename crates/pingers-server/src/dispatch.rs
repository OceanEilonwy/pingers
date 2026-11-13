// SPDX-License-Identifier: AGPL-3.0-only
//! From a decoded request to the crate that answers it. No logic of its own (architecture §3):
//! the channel check of design B.12, then one call per route.

use axum::http::StatusCode;
use pingers_ohttp::{BinaryRequest, BinaryResponse, Header, OhttpError};

use crate::Shared;
use crate::route::{Channel, Resolved, Route, resolve};

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
pub(crate) fn answer(shared: &Shared, channel: Channel, request: &BinaryRequest) -> Reply {
    match resolve(&request.method, &request.path) {
        Resolved::NotFound => Reply::status(StatusCode::NOT_FOUND),
        Resolved::MethodNotAllowed => Reply::status(StatusCode::METHOD_NOT_ALLOWED),
        // The route exists, but not on this channel: an anonymous route sent direct would have
        // revealed the caller's address, so it is refused rather than served.
        Resolved::Found(route) if !route.class().allowed_on(channel) => {
            Reply::status(StatusCode::MISDIRECTED_REQUEST)
        }
        Resolved::Found(route) => dispatch(shared, route, request),
    }
}

/// One call per route, to the crate that owns it.
fn dispatch(shared: &Shared, route: Route, _request: &BinaryRequest) -> Reply {
    match route {
        Route::OhttpConfig => Reply {
            status: StatusCode::OK,
            content_type: Some(OHTTP_KEYS),
            body: shared.served_config.encoded().to_vec(),
        },
        // Waiting on pingers-board-store.
        Route::BoardSync
        | Route::BoardPage
        | Route::BoardRevokeLease
        // Waiting on pingers-mailbox (mailboxes and the wake rule).
        | Route::CreateMailbox
        | Route::PostToMailbox
        | Route::ReadMailbox
        | Route::DeleteMailbox
        // Waiting on pingers-delivery (group logs, subscriptions; the live channel is
        // `LiveChannel`).
        | Route::CreateGroup
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
        // Waiting on pingers-quota and pingers-board-store, whose policies it publishes.
        | Route::Params => Reply::status(StatusCode::NOT_IMPLEMENTED),
    }
}
