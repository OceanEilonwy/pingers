// SPDX-License-Identifier: AGPL-3.0-only
//! The group log requests a live Meetup needs, behind a small trait.
//!
//! pingers-sync owns talking to the server (Grp request signing, Privacy Pass, retries,
//! paging); this crate only says what it needs. pingers-client implements [`GroupNetwork`]
//! over pingers-sync, which keeps the two L3 crates independent of each other.

use pingers_crypto::LogAuthKeyPair;
use pingers_ohttp::BinaryRequest;
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, CreateGroup, GroupId, GroupLogEntry, SequenceNumber,
    SubscriptionId,
};

/// Why a group log request failed. Kinds only: the implementation keeps the details, so no
/// server message or request content reaches the UI through this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NetworkError {
    /// No relay or server is reachable now; try again later.
    #[error("server unreachable")]
    Unavailable,
    /// The server refused the request (authorisation, unknown group, malformed request).
    #[error("server refused the request")]
    Rejected,
}

/// The server's group endpoints (design D), as a live Meetup uses them. Every call that takes
/// `auth` is a Grp request signed with that epoch's log-auth key.
#[async_trait::async_trait]
pub trait GroupNetwork: Send + Sync {
    /// `POST /v1/groups`: registers a new Meetup group (Anonymous, Privacy Pass).
    ///
    /// # Errors
    ///
    /// Any [`NetworkError`].
    async fn create_group(&self, request: CreateGroup) -> Result<(), NetworkError>;

    /// `POST /v1/groups/{id}/commit`: the epoch's one commit, or a conflict.
    ///
    /// # Errors
    ///
    /// Any [`NetworkError`]; a lost race is `Ok(CommitResponse::Conflict { .. })`.
    async fn post_commit(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        request: CommitRequest,
    ) -> Result<CommitResponse, NetworkError>;

    /// `POST /v1/groups/{id}/msgs`: a stored message that should wake absent members.
    ///
    /// # Errors
    ///
    /// Any [`NetworkError`].
    async fn post_message(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        message: AppMessage,
    ) -> Result<(), NetworkError>;

    /// `GET /v1/groups/{id}/msgs?after=`: every stored entry after `after` (all pages), in log
    /// order.
    ///
    /// # Errors
    ///
    /// Any [`NetworkError`].
    async fn fetch_log(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        after: Option<SequenceNumber>,
    ) -> Result<Vec<GroupLogEntry>, NetworkError>;

    /// `DELETE /v1/groups/{id}`: the last member ends the Meetup on the server.
    ///
    /// # Errors
    ///
    /// Any [`NetworkError`].
    async fn delete_group(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
    ) -> Result<(), NetworkError>;

    /// The signed inner request head of `POST /v1/groups/{id}/live?sub=` for
    /// [`pingers_transport::Transport::stream`]. `subscription` is this device's own, so the
    /// server does not push to it while the channel is open.
    ///
    /// # Errors
    ///
    /// [`NetworkError::Rejected`] if the head cannot be built.
    fn live_head(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        subscription: Option<SubscriptionId>,
    ) -> Result<BinaryRequest, NetworkError>;
}
