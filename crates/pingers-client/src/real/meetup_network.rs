// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-meetup-session's [`GroupNetwork`] over pingers-sync's engine: the Meetup group's
//! log, and its live channel (chunked OHTTP through the relay, opened by pingers-sync's
//! `open_live` on the Stream route, never direct).

use std::sync::Arc;

use async_trait::async_trait;
use pingers_crypto::LogAuthKeyPair;
use pingers_meetup_session::{GroupNetwork, NetworkError};
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, CreateGroup, Epoch, GroupId, GroupLogEntry,
    SequenceNumber, SubscriptionId,
};
use pingers_sync::{
    ApiRequest, GroupCall, GroupLifetime, GroupSigner, GroupStart, Sending, SyncError, WakeBudget,
};
use pingers_transport::DuplexStream;

use super::sync::{Net, delivered};

/// pingers-meetup-session's `GroupNetwork`.
pub(crate) struct GroupLogs(pub(crate) Arc<Net>);

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
    /// A Grp signer with `auth`, at the server-corrected time.
    fn signer<'key>(&self, auth: &'key LogAuthKeyPair) -> Result<GroupSigner<'key>, NetworkError> {
        let engine = self.0.engine().map_err(|_none| NetworkError::Unavailable)?;
        Ok(GroupSigner {
            key: auth,
            now: engine.corrected_now(),
        })
    }
}

#[async_trait]
impl GroupNetwork for GroupLogs {
    async fn create_group(&self, request: CreateGroup) -> Result<(), NetworkError> {
        let engine = self.0.engine().map_err(|_none| NetworkError::Unavailable)?;
        let start = GroupStart {
            lifetime: GroupLifetime::Meetup,
            epoch: Epoch::new(1),
        };
        let result = engine
            .create_group(&request, start, WakeBudget::UNLIMITED)
            .await;
        self.0.observe(&result);
        result.map_err(|error| network_error(&error))
    }

    async fn post_commit(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        request: CommitRequest,
    ) -> Result<CommitResponse, NetworkError> {
        let engine = self.0.engine().map_err(|_none| NetworkError::Unavailable)?;
        let request = ApiRequest::commit(group, &request, &self.signer(auth)?)
            .map_err(|error| network_error(&error))?;
        let result = async {
            let route = request.route();
            engine
                .send(request, Sending::or_drop(WakeBudget::UNLIMITED))
                .await?
                .answered(route)?
                .decode::<CommitResponse>()
        }
        .await;
        self.0.observe(&result);
        result.map_err(|error| network_error(&error))
    }

    async fn post_message(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        message: AppMessage,
    ) -> Result<(), NetworkError> {
        let engine = self.0.engine().map_err(|_none| NetworkError::Unavailable)?;
        let call = GroupCall { group, key: auth };
        let result = delivered(
            engine
                .post_group_message(call, &message, self.0.keep_until())
                .await,
        );
        self.0.observe(&result);
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
        let engine = self.0.engine().map_err(|_none| NetworkError::Unavailable)?;
        let mut entries: Vec<GroupLogEntry> = Vec::new();
        let mut cursor = after;
        loop {
            let call = GroupCall { group, key: auth };
            let result = engine
                .read_group_messages(call, cursor, WakeBudget::UNLIMITED)
                .await;
            self.0.observe(&result);
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
        let engine = self.0.engine().map_err(|_none| NetworkError::Unavailable)?;
        let result = engine
            .delete_group(GroupCall { group, key: auth }, WakeBudget::UNLIMITED)
            .await;
        self.0.observe(&result);
        result.map_err(|error| network_error(&error))
    }

    async fn delete_subscription(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        subscription: SubscriptionId,
    ) -> Result<(), NetworkError> {
        let engine = self.0.engine().map_err(|_none| NetworkError::Unavailable)?;
        let result = engine
            .delete_subscription(
                GroupCall { group, key: auth },
                &subscription,
                WakeBudget::UNLIMITED,
            )
            .await;
        self.0.observe(&result);
        result.map_err(|error| network_error(&error))
    }

    async fn open_live(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        subscription: Option<SubscriptionId>,
    ) -> Result<Box<dyn DuplexStream>, NetworkError> {
        let engine = self.0.engine().map_err(|_none| NetworkError::Unavailable)?;
        let request = ApiRequest::live(group, subscription.as_ref(), &self.signer(auth)?)
            .map_err(|error| network_error(&error))?;
        let result = engine.open_live(request).await;
        self.0.observe(&result);
        result.map_err(|error| network_error(&error))
    }
}
