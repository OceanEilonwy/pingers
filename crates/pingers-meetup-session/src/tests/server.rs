// SPDX-License-Identifier: AGPL-3.0-only
//! A fake server: group logs with one commit per epoch, and live fan-out over fake channels.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pingers_crypto::LogAuthKeyPair;
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, ContentType, CreateGroup, Delivery, Epoch, GroupId,
    GroupLogEntry, LiveFrame, SequenceNumber, SubscriptionId, WireDecode as _, WireEncode as _,
};
use pingers_transport::{DuplexStream, TransportError};
use tokio::sync::{mpsc, watch};

use crate::{GroupNetwork, NetworkError};

use super::{PATIENCE, TestError};

#[derive(Default)]
struct GroupLog {
    head: u64,
    entries: Vec<GroupLogEntry>,
}

struct Channel {
    id: u64,
    group: GroupId,
    member: &'static str,
    downlink: mpsc::UnboundedSender<Vec<u8>>,
    /// Frames put on the downlink so far.
    forwarded: usize,
}

/// Something a member sent, as the server saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Uplink {
    pub(crate) member: &'static str,
    pub(crate) delivery: Delivery,
}

#[derive(Default)]
struct State {
    groups: HashMap<GroupId, GroupLog>,
    channels: Vec<Channel>,
    next_channel: u64,
    opened: Vec<&'static str>,
    refused: Vec<&'static str>,
    refuse_streams: bool,
    hang_posts: bool,
    hung: usize,
    refuse_commits_from: Vec<&'static str>,
    refuse_posts_from: Vec<&'static str>,
    /// Frames each channel's member has taken off its downlink, by channel id.
    pulled: HashMap<u64, usize>,
    ephemeral_posts: usize,
    refused_commits: usize,
    uplinks: Vec<Uplink>,
    deleted: Vec<GroupId>,
    deleted_subscriptions: Vec<SubscriptionId>,
    /// Subscriptions accepted commits removed, in order.
    removed_by_commits: Vec<SubscriptionId>,
    /// Every byte string a device sent the server, as it arrived: requests, paths and frames.
    seen: Vec<Vec<u8>>,
}

/// The server every device talks to.
pub(crate) struct FakeServer {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

impl Default for FakeServer {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: watch::Sender::new(0),
        }
    }
}

impl FakeServer {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn touch(&self) {
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    /// Waits until `condition` holds of the server's state.
    async fn until(
        &self,
        what: &'static str,
        mut condition: impl FnMut(&State) -> bool,
    ) -> Result<(), TestError> {
        let mut changed = self.changed.subscribe();
        tokio::time::timeout(PATIENCE, async {
            loop {
                if condition(&self.lock()) {
                    return;
                }
                if changed.changed().await.is_err() {
                    return;
                }
            }
        })
        .await
        .map_err(|_| TestError::Timeout(what))
    }

    /// Frames `member` sent on its live channel with this delivery.
    pub(crate) fn uplinks(&self, member: &str, delivery: Delivery) -> usize {
        self.lock()
            .uplinks
            .iter()
            .filter(|uplink| uplink.member == member && uplink.delivery == delivery)
            .count()
    }

    pub(crate) async fn until_uplinks(
        &self,
        member: &'static str,
        delivery: Delivery,
        count: usize,
    ) -> Result<(), TestError> {
        self.until("live frames", |state| {
            state
                .uplinks
                .iter()
                .filter(|uplink| uplink.member == member && uplink.delivery == delivery)
                .count()
                >= count
        })
        .await
    }

    /// How many live channels `member` opened.
    pub(crate) fn opened(&self, member: &str) -> usize {
        self.lock()
            .opened
            .iter()
            .filter(|opener| **opener == member)
            .count()
    }

    pub(crate) async fn until_opened(
        &self,
        member: &'static str,
        count: usize,
    ) -> Result<(), TestError> {
        self.until("a live channel", |state| {
            state
                .opened
                .iter()
                .filter(|opener| **opener == member)
                .count()
                >= count
        })
        .await
    }

    /// Closes `member`'s live channels from the server side, as a dropped exchange looks.
    pub(crate) fn drop_channels(&self, member: &str) {
        self.lock()
            .channels
            .retain(|channel| channel.member != member);
        self.touch();
    }

    /// Waits until `member` has no live channel open, as when its Meetup task has stopped.
    pub(crate) async fn until_no_channel(&self, member: &'static str) -> Result<(), TestError> {
        self.until("the channel closed", |state| {
            !state
                .channels
                .iter()
                .any(|channel| channel.member == member)
        })
        .await
    }

    /// Waits until `member` has made `count` refused attempts to open a channel.
    pub(crate) async fn until_refused(
        &self,
        member: &'static str,
        count: usize,
    ) -> Result<(), TestError> {
        self.until("a refused attempt", |state| {
            state
                .refused
                .iter()
                .filter(|opener| **opener == member)
                .count()
                >= count
        })
        .await
    }

    /// Answers every commit `member` posts with `NetworkError::Unavailable`.
    pub(crate) fn refuse_commits_from(&self, member: &'static str) {
        self.lock().refuse_commits_from.push(member);
    }

    pub(crate) async fn until_refused_commits(&self, count: usize) -> Result<(), TestError> {
        self.until("a refused commit", |state| state.refused_commits >= count)
            .await
    }

    /// Lets `member`'s commits through again.
    pub(crate) fn allow_commits_from(&self, member: &str) {
        self.lock()
            .refuse_commits_from
            .retain(|refused| *refused != member);
    }

    /// Answers every message `member` posts to a group log with `NetworkError::Unavailable`.
    pub(crate) fn refuse_posts_from(&self, member: &'static str) {
        self.lock().refuse_posts_from.push(member);
    }

    /// Lets `member`'s posts through again.
    pub(crate) fn allow_posts_from(&self, member: &str) {
        self.lock()
            .refuse_posts_from
            .retain(|refused| *refused != member);
    }

    /// Waits until `member` has taken every frame forwarded to it off its open channels.
    ///
    /// A member reads the clock for a frame before it next yields, and the tests run on one
    /// thread, so once this returns no frame already sent can be timed after a later
    /// `advance`.
    pub(crate) async fn until_drained(&self, member: &'static str) -> Result<(), TestError> {
        self.until("a drained channel", |state| {
            state
                .channels
                .iter()
                .filter(|channel| channel.member == member)
                .all(|channel| {
                    state.pulled.get(&channel.id).copied().unwrap_or(0) >= channel.forwarded
                })
        })
        .await
    }

    /// Ephemeral frames posted to the group log (they never should be).
    pub(crate) fn ephemeral_posts(&self) -> usize {
        self.lock().ephemeral_posts
    }

    /// Attempts by `member` to open a channel that were refused.
    pub(crate) fn refused(&self, member: &str) -> usize {
        self.lock()
            .refused
            .iter()
            .filter(|opener| **opener == member)
            .count()
    }

    /// Makes every message post hang until it is let go.
    pub(crate) fn hang_posts(&self, hang: bool) {
        self.lock().hang_posts = hang;
    }

    pub(crate) async fn until_hung(&self, count: usize) -> Result<(), TestError> {
        self.until("a hanging post", |state| state.hung >= count)
            .await
    }

    /// Makes every attempt to open a live channel fail, or succeed again.
    pub(crate) fn refuse_streams(&self, refuse: bool) {
        self.lock().refuse_streams = refuse;
    }

    /// The commits stored in a group's log.
    pub(crate) fn commits(&self, group: &GroupId) -> usize {
        self.lock().groups.get(group).map_or(0, |log| {
            log.entries
                .iter()
                .filter(|entry| entry.content_type == ContentType::Commit)
                .count()
        })
    }

    pub(crate) fn removed_by_commits(&self) -> Vec<SubscriptionId> {
        self.lock().removed_by_commits.clone()
    }

    pub(crate) fn deleted_subscriptions(&self) -> Vec<SubscriptionId> {
        self.lock().deleted_subscriptions.clone()
    }

    pub(crate) fn deleted(&self) -> Vec<GroupId> {
        self.lock().deleted.clone()
    }

    /// Everything devices sent the server, so a test can check what it never learns.
    pub(crate) fn seen(&self) -> Vec<Vec<u8>> {
        self.lock().seen.clone()
    }

    fn saw(&self, bytes: Vec<u8>) {
        self.lock().seen.push(bytes);
    }

    /// Appends to the group's log.
    fn append(state: &mut State, group: &GroupId, frame: &LiveFrame, content_type: ContentType) {
        if let Some(log) = state.groups.get_mut(group) {
            let sequence = SequenceNumber::new(u64::try_from(log.entries.len()).unwrap_or(0));
            log.entries.push(GroupLogEntry {
                sequence,
                epoch: frame.epoch,
                content_type,
                body: frame.body.clone(),
            });
        }
    }

    fn forward(state: &mut State, group: &GroupId, frame: &LiveFrame, except: Option<u64>) {
        let Ok(bytes) = frame.encode() else {
            return;
        };
        for channel in &mut state.channels {
            if channel.group == *group
                && Some(channel.id) != except
                && channel.downlink.send(bytes.clone()).is_ok()
            {
                channel.forwarded = channel.forwarded.saturating_add(1);
            }
        }
    }

    /// Counts a frame `channel`'s member took off its downlink.
    fn pulled(&self, channel: u64) {
        let mut state = self.lock();
        let pulled = state.pulled.entry(channel).or_insert(0);
        *pulled = pulled.saturating_add(1);
        drop(state);
        self.touch();
    }

    fn uplink(&self, id: u64, bytes: &[u8]) -> Result<(), TransportError> {
        let mut state = self.lock();
        state.seen.push(bytes.to_vec());
        let Some(channel) = state.channels.iter().find(|channel| channel.id == id) else {
            return Err(TransportError::StreamClosed);
        };
        let (group, member) = (channel.group, channel.member);
        let frame = LiveFrame::decode(bytes).map_err(|_| TransportError::InvalidRequest)?;
        state.uplinks.push(Uplink {
            member,
            delivery: frame.delivery,
        });
        if frame.delivery == Delivery::Stored {
            Self::append(&mut state, &group, &frame, ContentType::Application);
        }
        Self::forward(&mut state, &group, &frame, Some(id));
        drop(state);
        self.touch();
        Ok(())
    }

    fn detach(&self, id: u64) {
        self.lock().channels.retain(|channel| channel.id != id);
        self.touch();
    }
}

/// One device's connection to the fake server: its group log client and its transport.
pub(crate) struct FakeNetwork {
    server: Arc<FakeServer>,
    member: &'static str,
}

impl FakeNetwork {
    pub(crate) const fn new(server: Arc<FakeServer>, member: &'static str) -> Self {
        Self { server, member }
    }
}

#[async_trait::async_trait]
impl GroupNetwork for FakeNetwork {
    async fn create_group(&self, request: CreateGroup) -> Result<(), NetworkError> {
        self.server
            .saw(request.encode().map_err(|_| NetworkError::Rejected)?);
        self.server
            .lock()
            .groups
            .insert(request.group_id, GroupLog::default());
        Ok(())
    }

    async fn post_commit(
        &self,
        group: &GroupId,
        _auth: &LogAuthKeyPair,
        request: CommitRequest,
    ) -> Result<CommitResponse, NetworkError> {
        self.server
            .saw(request.encode().map_err(|_| NetworkError::Rejected)?);
        let mut state = self.server.lock();
        if state.refuse_commits_from.contains(&self.member) {
            state.refused_commits = state.refused_commits.saturating_add(1);
            drop(state);
            self.server.touch();
            return Err(NetworkError::Unavailable);
        }
        let log = state.groups.get_mut(group).ok_or(NetworkError::Rejected)?;
        if request.epoch.get() != log.head {
            return Ok(CommitResponse::Conflict {
                head_epoch: Epoch::new(log.head),
            });
        }
        log.head = log.head.saturating_add(1);
        let sequence = SequenceNumber::new(u64::try_from(log.entries.len()).unwrap_or(0));
        state
            .removed_by_commits
            .extend(request.subscriptions_removed.iter().copied());
        let frame = LiveFrame {
            epoch: request.epoch,
            body: request.commit,
            delivery: Delivery::Stored,
        };
        FakeServer::append(&mut state, group, &frame, ContentType::Commit);
        FakeServer::forward(&mut state, group, &frame, None);
        drop(state);
        self.server.touch();
        Ok(CommitResponse::Accepted { sequence })
    }

    async fn post_message(
        &self,
        group: &GroupId,
        _auth: &LogAuthKeyPair,
        message: AppMessage,
    ) -> Result<(), NetworkError> {
        if self.server.lock().refuse_posts_from.contains(&self.member) {
            return Err(NetworkError::Unavailable);
        }
        self.server
            .saw(message.encode().map_err(|_| NetworkError::Rejected)?);
        let hang = {
            let mut state = self.server.lock();
            if state.hang_posts {
                state.hung = state.hung.saturating_add(1);
            }
            state.hang_posts
        };
        if hang {
            self.server.touch();
            return core::future::pending().await;
        }
        let mut state = self.server.lock();
        let frame = LiveFrame {
            epoch: message.epoch,
            body: message.body,
            delivery: message.delivery,
        };
        if frame.delivery == Delivery::Ephemeral {
            state.ephemeral_posts = state.ephemeral_posts.saturating_add(1);
        }
        FakeServer::append(&mut state, group, &frame, ContentType::Application);
        FakeServer::forward(&mut state, group, &frame, None);
        drop(state);
        self.server.touch();
        Ok(())
    }

    async fn fetch_log(
        &self,
        group: &GroupId,
        _auth: &LogAuthKeyPair,
        after: Option<SequenceNumber>,
    ) -> Result<Vec<GroupLogEntry>, NetworkError> {
        let state = self.server.lock();
        let log = state.groups.get(group).ok_or(NetworkError::Rejected)?;
        Ok(log
            .entries
            .iter()
            .filter(|entry| after.is_none_or(|after| entry.sequence > after))
            .cloned()
            .collect())
    }

    async fn delete_subscription(
        &self,
        _group: &GroupId,
        _auth: &LogAuthKeyPair,
        subscription: SubscriptionId,
    ) -> Result<(), NetworkError> {
        self.server.lock().deleted_subscriptions.push(subscription);
        Ok(())
    }

    async fn delete_group(
        &self,
        group: &GroupId,
        _auth: &LogAuthKeyPair,
    ) -> Result<(), NetworkError> {
        let mut state = self.server.lock();
        state.groups.remove(group);
        state.channels.retain(|channel| channel.group != *group);
        state.deleted.push(*group);
        drop(state);
        self.server.touch();
        Ok(())
    }

    async fn open_live(
        &self,
        group: &GroupId,
        _auth: &LogAuthKeyPair,
        _subscription: Option<SubscriptionId>,
    ) -> Result<Box<dyn DuplexStream>, NetworkError> {
        self.server.saw(hex_id(group).into_bytes());
        let mut state = self.server.lock();
        if state.refuse_streams {
            state.refused.push(self.member);
            drop(state);
            self.server.touch();
            return Err(NetworkError::Unavailable);
        }
        if !state.groups.contains_key(group) {
            return Err(NetworkError::Rejected);
        }
        let id = state.next_channel;
        state.next_channel = id.saturating_add(1);
        let (downlink, receiver) = mpsc::unbounded_channel();
        state.channels.push(Channel {
            id,
            group: *group,
            member: self.member,
            downlink,
            forwarded: 0,
        });
        state.opened.push(self.member);
        drop(state);
        self.server.touch();
        Ok(Box::new(FakeStream {
            server: Arc::clone(&self.server),
            id,
            receiver,
        }))
    }
}

fn hex_id(group: &GroupId) -> String {
    hex::encode(group.as_bytes())
}

/// One live channel.
struct FakeStream {
    server: Arc<FakeServer>,
    id: u64,
    receiver: mpsc::UnboundedReceiver<Vec<u8>>,
}

#[async_trait::async_trait]
impl DuplexStream for FakeStream {
    async fn send(&mut self, frame: Vec<u8>) -> Result<(), TransportError> {
        self.server.uplink(self.id, &frame)
    }

    async fn receive(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        let frame = self.receiver.recv().await;
        if frame.is_some() {
            self.server.pulled(self.id);
        }
        Ok(frame)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.server.detach(self.id);
        Ok(())
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.server.detach(self.id);
    }
}
