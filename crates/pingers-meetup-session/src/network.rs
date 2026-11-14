// SPDX-License-Identifier: AGPL-3.0-only
//! The group requests a live Meetup needs, behind a small trait: the group log, and the live
//! channel.
//!
//! pingers-sync owns talking to the server (Grp request signing, Privacy Pass, retries,
//! paging, and opening the live channel on the router's Stream route: chunked OHTTP through
//! the relay); this crate only says what it needs. pingers-client implements
//! [`GroupNetwork`] over pingers-sync, which keeps the two L3 crates independent of each
//! other.

use pingers_crypto::LogAuthKeyPair;
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, CreateGroup, GroupId, GroupLogEntry, SequenceNumber,
    SubscriptionId,
};
use pingers_transport::DuplexStream;

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

    /// `DELETE /v1/groups/{id}/subs/{sub_id}`: a leaver takes its own subscription off the
    /// group before it goes, so the server stops waking it for a Meetup it left (design G.6).
    ///
    /// # Errors
    ///
    /// Any [`NetworkError`].
    async fn delete_subscription(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        subscription: SubscriptionId,
    ) -> Result<(), NetworkError>;

    /// `POST /v1/groups/{id}/live?sub=`: opens the Meetup's live channel, a chunked OHTTP
    /// exchange through the relay (privacy class Stream, never direct), its head signed with
    /// `auth`. `subscription` is this device's own, so the server does not push to it while
    /// the channel is open.
    ///
    /// # Errors
    ///
    /// [`NetworkError::Unavailable`] if no relay answered or the exchange failed;
    /// [`NetworkError::Rejected`] if the head cannot be built or the relay's key does not
    /// match its pin.
    async fn open_live(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        subscription: Option<SubscriptionId>,
    ) -> Result<Box<dyn DuplexStream>, NetworkError>;
}
