// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-meetup-session's [`GroupNetwork`] over pingers-sync's engine of the server the
//! Meetup's Party lives on: the Meetup group's log, and its live channel (chunked OHTTP through
//! that server's relay, opened by pingers-sync's `open_live` on the Stream route, never direct).

use std::sync::Arc;

use async_trait::async_trait;
use pingers_crypto::LogAuthKeyPair;
use pingers_meetup_session::{GroupNetwork, NetworkError};
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, CreateGroup, Epoch, GroupId, GroupLogEntry,
    SequenceNumber, ServerId, SubscriptionId,
};
use pingers_sync::{
    ApiRequest, GroupCall, GroupLifetime, GroupSigner, GroupStart, Sending, SyncError, WakeBudget,
};
use pingers_transport::DuplexStream;

use super::servers::Link;
use super::sync::{Net, delivered};

/// pingers-meetup-session's `GroupNetwork` for one Meetup: every call goes to the server of the
/// Meetup's Party.
pub(crate) struct GroupLogs {
    pub(crate) net: Arc<Net>,
    /// The server of the Meetup's Party.
    pub(crate) server: ServerId,
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other failure is the network's or local, to try again later"
)]
fn network_error(error: &SyncError) -> NetworkError {
    match error {
        SyncError::Status { .. } | SyncError::BadResponse { .. } | SyncError::RelayKeyMismatch => {
            NetworkError::Rejected
        }
        _ => NetworkError::Unavailable,
    }
}

impl GroupLogs {
    /// The link to the Meetup's server; a server this device cannot reach now is unavailable.
    async fn link(&self) -> Result<Arc<Link>, NetworkError> {
        self.net
            .link(&self.server)
            .await
            .map_err(|_unknown| NetworkError::Unavailable)
    }

    /// A Grp signer with `auth`, at `link`'s server-corrected time.
    fn signer<'key>(link: &Link, auth: &'key LogAuthKeyPair) -> GroupSigner<'key> {
        GroupSigner {
            key: auth,
            now: link.engine.corrected_now(),
        }
    }
}

#[async_trait]
impl GroupNetwork for GroupLogs {
    async fn create_group(&self, request: CreateGroup) -> Result<(), NetworkError> {
        let link = self.link().await?;
        // The creator's group is at epoch 0 until its first commit (adding the knocker) lands.
        let start = GroupStart {
            lifetime: GroupLifetime::Meetup,
            epoch: Epoch::new(0),
        };
        let result = link
            .engine
            .create_group(&request, start, WakeBudget::UNLIMITED)
            .await;
        link.observe(&result);
        result.map_err(|error| network_error(&error))
    }

    async fn post_commit(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        request: CommitRequest,
    ) -> Result<CommitResponse, NetworkError> {
        let link = self.link().await?;
        let request = ApiRequest::commit(group, &request, &Self::signer(&link, auth))
            .map_err(|error| network_error(&error))?;
        let result = async {
            let route = request.route();
            link.engine
                .send(request, Sending::or_drop(WakeBudget::UNLIMITED))
                .await?
                .answered(route)?
                .decode::<CommitResponse>()
        }
        .await;
        link.observe(&result);
        result.map_err(|error| network_error(&error))
    }

    async fn post_message(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        message: AppMessage,
    ) -> Result<(), NetworkError> {
        let link = self.link().await?;
        let call = GroupCall { group, key: auth };
        let result = delivered(
            link.engine
                .post_group_message(call, &message, self.net.keep_until())
                .await,
        );
        link.observe(&result);
        match result {
            Ok(true) => Ok(()),
            Ok(false) => Err(NetworkError::Unavailable),
            Err(error) => Err(network_error(&error)),
        }
    }

    async fn fetch_log(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        after: Option<SequenceNumber>,
    ) -> Result<Vec<GroupLogEntry>, NetworkError> {
        let link = self.link().await?;
        let mut entries: Vec<GroupLogEntry> = Vec::new();
        let mut cursor = after;
        loop {
            let call = GroupCall { group, key: auth };
            let result = link
                .engine
                .read_group_messages(call, cursor, WakeBudget::UNLIMITED)
                .await;
            link.observe(&result);
            let page = result.map_err(|error| network_error(&error))?;
            // Pages follow each other until one comes back empty.
            let Some(last) = page.messages.last().map(|entry| entry.sequence) else {
                return Ok(entries);
            };
            entries.extend(page.messages);
            cursor = Some(last);
        }
    }

    async fn delete_group(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
    ) -> Result<(), NetworkError> {
        let link = self.link().await?;
        let result = link
            .engine
            .delete_group(GroupCall { group, key: auth }, WakeBudget::UNLIMITED)
            .await;
        link.observe(&result);
        result.map_err(|error| network_error(&error))
    }

    async fn open_live(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        subscription: Option<SubscriptionId>,
    ) -> Result<Box<dyn DuplexStream>, NetworkError> {
        let link = self.link().await?;
        let request = ApiRequest::live(group, subscription.as_ref(), &Self::signer(&link, auth))
            .map_err(|error| network_error(&error))?;
        let result = link.engine.open_live(request).await;
        link.observe(&result);
        result.map_err(|error| network_error(&error))
    }
}
