// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-sync's places, until it merges.
//!
//! WAITING(pingers-sync): every type here is an explicit stub. [`Sync`] answers the client's
//! [`SyncPort`] with [`Unavailable::Sync`]; [`Server`] answers pingers-wake's `ServerPort` with
//! `PortError::Unavailable`, so a wake ends its network part cleanly
//! (`WakeEnding::PortFailed`) and still schedules the next one; [`GroupLogs`] answers
//! pingers-meetup-session's `GroupNetwork` with `NetworkError::Unavailable`. The
//! [`Outbox`] keeps the `Outbound` values pingers-parties returns, in memory only, so they can
//! be handed over when pingers-sync lands; today they are dropped at exit.

use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use pingers_crypto::LogAuthKeyPair;
use pingers_meetup_session::{GroupNetwork, NetworkError};
use pingers_ohttp::BinaryRequest;
use pingers_parties::Outbound;
use pingers_presence::{MailboxPurpose, OutgoingKnock};
use pingers_proto::{
    AppMessage, BoardCursor, BoardPage, CommitRequest, CommitResponse, CreateGroup, GroupId,
    GroupLogEntry, MailboxId, SequenceNumber, SubscriptionId,
};
use pingers_time::Bucket;
use pingers_wake::PortError;
use pingers_wake::ports::{LeaseUpload, ServerPort};

use crate::ports::{ServerSection, SyncPort};
use crate::snapshot::{RelayStatus, WakePushes};
use crate::{ClientError, PushTokens, Unavailable};

/// The requests pingers-parties made for pingers-sync to send.
#[derive(Default)]
pub(crate) struct Outbox(Mutex<Vec<Outbound>>);

impl Outbox {
    /// Keeps `outbound` for pingers-sync.
    pub(crate) fn push(&self, outbound: impl IntoIterator<Item = Outbound>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(outbound);
    }
}

/// The client's [`SyncPort`].
pub(crate) struct Sync;

#[async_trait]
impl SyncPort for Sync {
    async fn push_tokens_changed(&self, _tokens: Option<PushTokens>) -> Result<(), ClientError> {
        Err(Unavailable::Sync.into())
    }

    async fn set_wake_pushes(
        &self,
        _setting: WakePushes,
        _tokens: Option<PushTokens>,
    ) -> Result<(), ClientError> {
        Err(Unavailable::Sync.into())
    }

    /// No fallbacks are known and no relay has been reached: nothing talks to the server yet.
    async fn section(&self) -> Result<ServerSection, ClientError> {
        Ok(ServerSection {
            fallbacks: Vec::new(),
            relay: RelayStatus::Unreachable,
        })
    }
}

/// pingers-wake's `ServerPort`.
pub(crate) struct Server;

impl ServerPort for Server {
    async fn pull_group_logs(&self) -> Result<(), PortError> {
        Err(PortError::Unavailable)
    }

    async fn upload_lease(&self, _upload: LeaseUpload) -> Result<Option<BoardPage>, PortError> {
        Err(PortError::Unavailable)
    }

    async fn board_page(&self, _cursor: BoardCursor) -> Result<BoardPage, PortError> {
        Err(PortError::Unavailable)
    }

    async fn post_knock(&self, _knock: OutgoingKnock) -> Result<(), PortError> {
        Err(PortError::Unavailable)
    }

    async fn read_mailbox(&self, _mailbox: MailboxId) -> Result<Vec<Vec<u8>>, PortError> {
        Err(PortError::Unavailable)
    }

    fn take_mailbox(&self, _purpose: MailboxPurpose, _lease_start: Bucket) -> Option<MailboxId> {
        None
    }

    async fn refill_mailbox_pool(&self) -> Result<(), PortError> {
        Err(PortError::Unavailable)
    }
}

/// pingers-meetup-session's `GroupNetwork`.
#[expect(
    dead_code,
    reason = "goes into the meetup Runtime with pingers-sync's transport; nothing builds one yet"
)]
pub(crate) struct GroupLogs;

#[async_trait]
impl GroupNetwork for GroupLogs {
    async fn create_group(&self, _request: CreateGroup) -> Result<(), NetworkError> {
        Err(NetworkError::Unavailable)
    }

    async fn post_commit(
        &self,
        _group: &GroupId,
        _auth: &LogAuthKeyPair,
        _request: CommitRequest,
    ) -> Result<CommitResponse, NetworkError> {
        Err(NetworkError::Unavailable)
    }

    async fn post_message(
        &self,
        _group: &GroupId,
        _auth: &LogAuthKeyPair,
        _message: AppMessage,
    ) -> Result<(), NetworkError> {
        Err(NetworkError::Unavailable)
    }

    async fn fetch_log(
        &self,
        _group: &GroupId,
        _auth: &LogAuthKeyPair,
        _after: Option<SequenceNumber>,
    ) -> Result<Vec<GroupLogEntry>, NetworkError> {
        Err(NetworkError::Unavailable)
    }

    async fn delete_group(
        &self,
        _group: &GroupId,
        _auth: &LogAuthKeyPair,
    ) -> Result<(), NetworkError> {
        Err(NetworkError::Unavailable)
    }

    fn live_head(
        &self,
        _group: &GroupId,
        _auth: &LogAuthKeyPair,
        _subscription: Option<SubscriptionId>,
    ) -> Result<BinaryRequest, NetworkError> {
        Err(NetworkError::Unavailable)
    }
}
