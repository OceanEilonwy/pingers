// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-sync: the client's [`SyncPort`], pingers-wake's `ServerPort`,
//! pingers-meetup-session's `GroupNetwork`, and the outbox that sends what pingers-parties
//! returns.
//!
//! One [`Net`] per process holds the engine (built when the client is given an instance) and
//! the outbox. Without an instance every network call answers [`Unavailable::Sync`] (or
//! `PortError::Unavailable` to a wake, which then ends its network part cleanly).
//!
//! The outbox is in memory: a request that could not be sent stays for the next flush (each
//! party command and each wake), and is lost at exit. pingers-sync queues what it can itself.

use core::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use pingers_crypto::LogAuthKeyPair;
use pingers_meetup_session::{GroupNetwork, NetworkError};
use pingers_ohttp::BinaryRequest;
use pingers_parties::{LeaveRequest, Outbound, OutgoingCommit, PartiesError, PartyLifecycle};
use pingers_presence::{MailboxPurpose, OutgoingKnock};
use pingers_proto::{
    AppMessage, BoardCursor, BoardPage, CommitRequest, CommitResponse, CreateGroup, Epoch, GroupId,
    GroupLogEntry, MailboxId, Padded, SecretText, SequenceNumber, ServerId, SizeClass,
    SubscriptionId, TimeToLive, WelcomeBytes,
};
use pingers_store::{MailboxPurpose as StoredPurpose, WakePushes as StoredPushes};
use pingers_sync::{
    ApiRequest, CodeSource, Committed, Delivered, GroupCall, GroupError, GroupLifetime,
    GroupSigner, GroupStart, NewLease, PoolTarget, Sending, SyncEngine, SyncError, WakeBudget,
};
use pingers_time::{Bucket, UnixSeconds};
use pingers_wake::PortError;
use pingers_wake::ports::{LeaseUpload, ServerPort};

use super::Backend;
use super::party_log::{self, PartyLog};
use crate::ports::{ServerSection, SyncPort};
use crate::snapshot::{RelayStatus, WakePushes};
use crate::{ClientError, Notice, PushTokens, Unavailable};

/// A Party group: kept for months, uploaded at epoch 0 (pingers-parties' new group).
const PARTY_START: GroupStart = GroupStart {
    lifetime: GroupLifetime::Party,
    epoch: Epoch::new(0),
};

/// How long a write that could not be sent may wait in pingers-sync's queue.
const KEEP_FOR: Duration = Duration::from_hours(1);

/// How many knock mailboxes the pool keeps, and for how long each is registered: a lease is
/// an hour, so a day covers every lease started while it is pooled.
const KNOCK_POOL: u32 = 4;
const MAILBOX_LIFETIME: Duration = Duration::from_hours(24);

/// What a client with an instance talks through: the engine, and the build's registration
/// code for the first registration.
pub(crate) struct Online {
    pub(crate) engine: Arc<SyncEngine>,
    pub(crate) registration_code: Option<SecretText>,
}

/// The engine and the outbox.
pub(crate) struct Net {
    pub(super) backend: Arc<Backend>,
    pub(super) engine: Option<Arc<SyncEngine>>,
    outbox: Mutex<Vec<Outbound>>,
    relay: Mutex<RelayStatus>,
    clock_skew: AtomicBool,
    pub(super) key_mismatch: AtomicBool,
    /// The server's parameters and the push relay's key, once fetched (`push.rs`).
    pub(super) learned: super::push::Learned,
    /// The build's registration code for this device's first registration, if any.
    registration_code: Option<SecretText>,
}

impl Net {
    pub(crate) fn new(
        backend: Arc<Backend>,
        online: Option<Online>,
        learned: super::push::Learned,
    ) -> Self {
        let (engine, registration_code) = match online {
            Some(online) => (Some(online.engine), online.registration_code),
            None => (None, None),
        };
        Self {
            backend,
            engine,
            outbox: Mutex::new(Vec::new()),
            relay: Mutex::new(RelayStatus::Unreachable),
            clock_skew: AtomicBool::new(false),
            key_mismatch: AtomicBool::new(false),
            learned,
            registration_code,
        }
    }

    pub(super) fn engine(&self) -> Result<&SyncEngine, ClientError> {
        self.engine.as_deref().ok_or(Unavailable::Sync.into())
    }

    fn outbox(&self) -> std::sync::MutexGuard<'_, Vec<Outbound>> {
        self.outbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `outbound` for the next flush.
    pub(crate) fn push(&self, outbound: impl IntoIterator<Item = Outbound>) {
        self.outbox().extend(outbound);
    }

    /// What a network result says about the relay and the clock.
    pub(super) fn observe<T>(&self, result: &Result<T, SyncError>) {
        let status = match result {
            Ok(_) => RelayStatus::Reachable,
            Err(SyncError::NoRelay { .. }) => RelayStatus::Unreachable,
            Err(SyncError::RelayKeyMismatch) => {
                self.key_mismatch.store(true, Ordering::Relaxed);
                RelayStatus::KeyMismatch
            }
            // The OHTTP relay answered; only the push relay's key is wrong. No push token is
            // sealed to it until the user resolves the notice.
            Err(SyncError::PushRelayKeyMismatch) => {
                self.key_mismatch.store(true, Ordering::Relaxed);
                RelayStatus::Reachable
            }
            Err(SyncError::ClockSkew { .. }) => {
                self.clock_skew.store(true, Ordering::Relaxed);
                return;
            }
            Err(_) => return,
        };
        *self.relay.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }

    /// Notices raised since the last call (`Notice::ClockSkew`, `Notice::RelayKeyMismatch`).
    pub(crate) fn take_notices(&self) -> Vec<Notice> {
        let mut notices = Vec::new();
        if self.clock_skew.swap(false, Ordering::Relaxed) {
            notices.push(Notice::ClockSkew);
        }
        if self.key_mismatch.swap(false, Ordering::Relaxed) {
            notices.push(Notice::RelayKeyMismatch);
        }
        notices
    }

    fn keep_until(&self) -> Sending {
        let now = party_log::now(&self.backend.outside);
        let until = UnixSeconds::new(now.get().saturating_add(KEEP_FOR.as_secs()));
        Sending::or_keep_until(until, WakeBudget::UNLIMITED)
    }

    /// pingers-sync's `prepare_credentials`, with the build's registration code on offer: the
    /// account registered, its leaf, and the token stock. Once done it costs a few store reads.
    /// Only a store failure is an error; a network failure or a refusal leaves credentialed
    /// requests waiting for a later call (each command's flush and each wake).
    ///
    /// # Errors
    ///
    /// [`ClientError::Sync`] with a store failure; [`Unavailable::Sync`] without an instance.
    pub(crate) async fn prepare_credentials(&self) -> Result<(), ClientError> {
        let engine = self.engine()?;
        if let Some(code) = &self.registration_code {
            engine
                .offer_registration_code(code.clone(), CodeSource::Build)
                .await?;
        }
        let result = engine.prepare_credentials(WakeBudget::UNLIMITED).await;
        self.observe(&result);
        match result {
            Err(SyncError::Store(error)) => Err(SyncError::Store(error).into()),
            Ok(_) | Err(_) => Ok(()),
        }
    }

    /// [`Net::prepare_credentials`] when there is an instance; offline, nothing to do. Commands
    /// that need the account (a new Party, a join) call it first: the first one makes it.
    ///
    /// # Errors
    ///
    /// As [`Net::prepare_credentials`].
    pub(crate) async fn ready(&self) -> Result<(), ClientError> {
        if self.engine.is_none() {
            return Ok(());
        }
        self.prepare_credentials().await
    }

    /// Offers the registration code an invite link carries for the first registration, and
    /// prepares the credentials with it.
    ///
    /// # Errors
    ///
    /// As [`Net::prepare_credentials`].
    pub(crate) async fn offer_invite_code(&self, code: SecretText) -> Result<(), ClientError> {
        self.engine()?
            .offer_registration_code(code, CodeSource::Invite)
            .await?;
        self.prepare_credentials().await
    }

    /// The registration code this device registered with, for its invite links.
    ///
    /// # Errors
    ///
    /// [`ClientError::Sync`] with a store failure. Without an instance there is none.
    pub(crate) async fn shared_registration_code(&self) -> Result<Option<SecretText>, ClientError> {
        match &self.engine {
            Some(engine) => Ok(engine.shared_registration_code().await?),
            None => Ok(None),
        }
    }

    /// Sends every waiting request. One that could not be sent now stays for the next flush;
    /// one the server refused is dropped. Returns whether a Party changed (a merged commit).
    ///
    /// # Errors
    ///
    /// A storage failure; network failures only keep the request.
    pub(crate) async fn flush(&self) -> Result<bool, ClientError> {
        if self.engine.is_none() {
            return Ok(false);
        }
        self.ready().await?;
        let waiting = std::mem::take(&mut *self.outbox());
        let mut changed = false;
        let mut kept = Vec::new();
        for outbound in waiting {
            match self.send(outbound).await {
                Ok(Sent::Done { changed: party }) => changed |= party,
                Ok(Sent::Later(outbound)) => kept.push(*outbound),
                Err(error @ (ClientError::Store(_) | ClientError::Records(_))) => {
                    self.push(kept);
                    return Err(error);
                }
                // Refused, or not ours to retry (a commit pingers-parties dropped).
                Err(_) => {}
            }
        }
        self.push(kept);
        Ok(changed)
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Outbound is non-exhaustive; a new kind is passed over until it is mapped"
    )]
    async fn send(&self, outbound: Outbound) -> Result<Sent, ClientError> {
        let engine = self.engine()?;
        let outbound = match outbound {
            Outbound::Commit(commit) => return self.commit(commit).await,
            other => other,
        };
        let result = match &outbound {
            Outbound::CreateGroup(request) => engine
                .create_group(request, PARTY_START, WakeBudget::UNLIMITED)
                .await
                .map(|()| true),
            Outbound::GroupMessage { group, message } => {
                let key = self.group_key(*group).await?;
                let call = GroupCall { group, key: &key };
                delivered(
                    engine
                        .post_group_message(call, message, self.keep_until())
                        .await,
                )
            }
            Outbound::MailboxPost { mailbox, body } => delivered(
                engine
                    .post_to_mailbox(mailbox, body, self.keep_until())
                    .await,
            ),
            Outbound::DeleteMailbox { mailbox } => engine
                .delete_mailbox(mailbox, WakeBudget::UNLIMITED)
                .await
                .map(|()| true),
            Outbound::Leave(leave) => self.leave(engine, leave).await,
            _ => return Ok(Sent::Done { changed: false }),
        };
        self.observe(&result);
        match result {
            Ok(true) => Ok(Sent::Done { changed: false }),
            Ok(false)
            | Err(
                SyncError::NoRelay { .. }
                | SyncError::BudgetExhausted { .. }
                | SyncError::ClockSkew { .. }
                | SyncError::NoCredential
                | SyncError::Transport(_),
            ) => Ok(Sent::Later(Box::new(outbound))),
            Err(error) => Err(error.into()),
        }
    }

    async fn leave(&self, engine: &SyncEngine, leave: &LeaveRequest) -> Result<bool, SyncError> {
        let call = GroupCall {
            group: &leave.group,
            key: &leave.auth,
        };
        if !delivered(
            engine
                .post_group_message(call, &leave.proposal, self.keep_until())
                .await,
        )? {
            return Ok(false);
        }
        if let Some(subscription) = &leave.subscription {
            engine
                .delete_subscription(call, subscription, WakeBudget::UNLIMITED)
                .await?;
        }
        Ok(true)
    }

    /// Commits this device's commit, then posts its Welcome. A commit that could not be sent
    /// stays pending in pingers-parties and in the outbox.
    async fn commit(&self, commit: OutgoingCommit) -> Result<Sent, ClientError> {
        let engine = self.engine()?;
        let party = commit.party;
        let mut log = PartyLog::load(
            &self.backend.database,
            &self.backend.outside,
            party,
            Some(commit),
        )?;
        let result = engine
            .commit(&log.group(), &mut log, WakeBudget::UNLIMITED)
            .await;
        self.push(std::mem::take(&mut log.outbound));
        match result {
            Ok(Committed::Accepted(_)) => {
                if let Some(welcome) = log.accepted.take().and_then(|commit| commit.welcome) {
                    let posted = engine
                        .post_to_mailbox(&welcome.mailbox, &welcome.body, self.keep_until())
                        .await;
                    self.observe(&posted);
                    let _: Delivered<SequenceNumber> = posted?;
                }
                Ok(Sent::Done { changed: true })
            }
            Err(GroupError::Owner(error)) => Err(error),
            Err(GroupError::Sync(error)) => {
                let transient = matches!(
                    error,
                    SyncError::NoRelay { .. }
                        | SyncError::BudgetExhausted { .. }
                        | SyncError::ClockSkew { .. }
                        | SyncError::NoCredential
                        | SyncError::Transport(_)
                );
                self.observe::<()>(&Err(error));
                match log.pending.take() {
                    Some(commit) if transient => {
                        Ok(Sent::Later(Box::new(Outbound::Commit(commit))))
                    }
                    _ => Ok(Sent::Done { changed: false }),
                }
            }
            // Nothing left to commit, or the conflicts ran out: pingers-parties dropped it.
            Ok(Committed::NothingToCommit) | Err(_) => Ok(Sent::Done { changed: true }),
        }
    }

    /// The current log-auth key of the Party whose group is `group`.
    async fn group_key(&self, group: GroupId) -> Result<LogAuthKeyPair, ClientError> {
        self.backend
            .store(move |session, clock, _rng| {
                let party = session
                    .parties()
                    .by_group(&group)?
                    .ok_or(PartiesError::UnknownParty)?;
                Ok(PartyLifecycle::new(session, clock).log_auth_key(&party.id)?)
            })
            .await
    }

    /// Pulls every Party's log (design B.4.7: before every board sync).
    async fn pull_all(&self) -> Result<bool, ClientError> {
        let engine = self.engine()?;
        let parties = self
            .backend
            .store(|session, _clock, _rng| party_log::parties(session))
            .await?;
        let mut changed = false;
        for party in parties {
            let mut log =
                PartyLog::load(&self.backend.database, &self.backend.outside, party, None)?;
            let result = engine
                .pull_group(&log.group(), &mut log, WakeBudget::UNLIMITED)
                .await;
            self.push(std::mem::take(&mut log.outbound));
            changed |= log.changed;
            match result {
                Ok(_) => {}
                Err(GroupError::Sync(error)) => {
                    let result = Err(error);
                    self.observe::<()>(&result);
                    result?;
                }
                Err(GroupError::Owner(error)) => return Err(error),
                // GroupError is non-exhaustive; nothing else is a pull's to report.
                Err(_conflicts) => {}
            }
        }
        Ok(changed)
    }
}

/// What sending one request came to.
enum Sent {
    Done { changed: bool },
    Later(Box<Outbound>),
}

/// Whether a write went out (`true`), or waits in pingers-sync's queue or was dropped there.
fn delivered<T>(result: Result<Delivered<T>, SyncError>) -> Result<bool, SyncError> {
    Ok(match result? {
        Delivered::Done(_) | Delivered::Queued(_) => true,
        Delivered::Dropped(_) => false,
    })
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other failure is local: the wake ends its network part"
)]
fn port_error(error: &ClientError) -> PortError {
    match error {
        ClientError::Sync(
            SyncError::NoRelay { .. } | SyncError::BudgetExhausted { .. } | SyncError::Transport(_),
        ) => PortError::Unreachable,
        ClientError::Sync(SyncError::Status { .. } | SyncError::ClockSkew { .. }) => {
            PortError::Refused
        }
        _ => PortError::Unavailable,
    }
}

const fn stored_purpose(purpose: MailboxPurpose) -> StoredPurpose {
    match purpose {
        MailboxPurpose::Knock => StoredPurpose::Knock,
        MailboxPurpose::Reply => StoredPurpose::Reply,
        MailboxPurpose::Invite => StoredPurpose::Invite,
    }
}

impl Net {
    async fn server<T>(
        &self,
        call: impl core::future::Future<Output = Result<T, SyncError>>,
    ) -> Result<T, PortError> {
        let result = call.await;
        self.observe(&result);
        result.map_err(|error| port_error(&error.into()))
    }
}

impl Net {
    /// Posts a Meetup Welcome to the knocker's reply mailbox, padded to a mailbox class. If it
    /// never arrives, a Meetup this device just created ends by itself after 30 minutes alone
    /// (design B.5).
    pub(crate) async fn post_welcome(
        &self,
        mailbox: MailboxId,
        welcome: WelcomeBytes,
    ) -> Result<(), PortError> {
        let engine = self.engine().map_err(|_none| PortError::Unavailable)?;
        let body = Padded::pad_smallest(welcome.as_bytes(), &SizeClass::MAILBOX_POST)
            .map_err(|_too_big| PortError::Unavailable)?;
        match self
            .server(engine.post_to_mailbox(&mailbox, &body, self.keep_until()))
            .await?
        {
            Delivered::Done(_) | Delivered::Queued(_) => Ok(()),
            Delivered::Dropped(_) => Err(PortError::Unreachable),
        }
    }
}

// One engine serves every server for now: the server each call names is not used yet.
impl ServerPort for Net {
    async fn pull_group_logs(&self, _server: &ServerId) -> Result<(), PortError> {
        // A Party may have changed: the wake's next Party view reads it again.
        self.flush().await.map_err(|error| port_error(&error))?;
        // The server's parameters once per process (the map host, the push relays); a failure
        // here is the next wake's to retry and does not stop the pull.
        if let Err(error @ (ClientError::Store(_) | ClientError::Records(_))) =
            self.server_params().await
        {
            return Err(port_error(&error));
        }
        self.pull_all()
            .await
            .map(drop)
            .map_err(|error| port_error(&error))
    }

    async fn upload_lease(
        &self,
        _server: &ServerId,
        upload: LeaseUpload,
    ) -> Result<Option<BoardPage>, PortError> {
        let engine = self.engine().map_err(|_none| PortError::Unavailable)?;
        let lease = NewLease {
            lease: upload.lease,
            revoke_key: upload.revoke_key,
            read: upload.read,
        };
        match self
            .server(engine.upload_lease(lease, WakeBudget::UNLIMITED))
            .await?
        {
            Delivered::Done(response) => Ok(response.page),
            Delivered::Queued(_) | Delivered::Dropped(_) => Err(PortError::Unreachable),
        }
    }

    async fn board_page(
        &self,
        _server: &ServerId,
        cursor: BoardCursor,
    ) -> Result<BoardPage, PortError> {
        let engine = self.engine().map_err(|_none| PortError::Unavailable)?;
        self.server(engine.board_page(&cursor, WakeBudget::UNLIMITED))
            .await
    }

    async fn post_knock(&self, _server: &ServerId, knock: OutgoingKnock) -> Result<(), PortError> {
        let engine = self.engine().map_err(|_none| PortError::Unavailable)?;
        let body = Padded::from_wire(knock.post).map_err(|_bad| PortError::Unavailable)?;
        let sending = Sending::or_drop(WakeBudget::UNLIMITED);
        match self
            .server(engine.post_to_mailbox(&knock.mailbox, &body, sending))
            .await?
        {
            Delivered::Done(_) | Delivered::Queued(_) => Ok(()),
            Delivered::Dropped(_) => Err(PortError::Unreachable),
        }
    }

    async fn read_mailbox(
        &self,
        _server: &ServerId,
        mailbox: MailboxId,
    ) -> Result<Vec<Vec<u8>>, PortError> {
        let engine = self.engine().map_err(|_none| PortError::Unavailable)?;
        let messages = self
            .server(engine.read_mailbox(&mailbox, None, WakeBudget::UNLIMITED))
            .await?;
        Ok(messages
            .messages
            .into_iter()
            .map(|message| message.body.into_bytes())
            .collect())
    }

    fn take_mailbox(
        &self,
        _server: &ServerId,
        purpose: MailboxPurpose,
        lease_start: Bucket,
    ) -> Option<MailboxId> {
        self.engine
            .as_deref()?
            .take_mailbox_for_lease(stored_purpose(purpose), lease_start)
            .ok()
            .flatten()
    }

    async fn refill_mailbox_pool(&self, _server: &ServerId) -> Result<(), PortError> {
        let engine = self.engine().map_err(|_none| PortError::Unavailable)?;
        let target = PoolTarget {
            purpose: StoredPurpose::Knock,
            size: KNOCK_POOL,
            lifetime: TimeToLive::new(MAILBOX_LIFETIME).map_err(|_never| PortError::Unavailable)?,
        };
        self.server(engine.refill_mailbox_pool(&target, WakeBudget::UNLIMITED))
            .await
            .map(drop)
    }
}

/// The client's [`SyncPort`].
pub(crate) struct Sync(pub(crate) Arc<Net>);

#[async_trait]
impl SyncPort for Sync {
    /// New mailboxes and subscriptions are sealed with the new tokens from now on; with pushes
    /// off nothing is sealed. Mailboxes and subscriptions already registered keep their old
    /// seal until they expire or are renewed.
    async fn push_tokens_changed(&self, tokens: Option<PushTokens>) -> Result<(), ClientError> {
        let setting = self
            .0
            .backend
            .store(|session, _clock, _rng| Ok(session.settings().wake_pushes()?))
            .await?;
        match (setting, tokens) {
            (StoredPushes::Disabled, _) => Ok(()),
            (StoredPushes::Enabled, None) => {
                self.0.engine()?;
                self.0.clear_wake_push();
                Ok(())
            }
            (StoredPushes::Enabled, Some(tokens)) => self.0.seal_wake_push(&tokens).await,
        }
    }

    /// "No wake-up pushes" seals no token anywhere; turning them on fetches the push relay's
    /// key (checked against its pins) and seals the platform's token to it.
    async fn set_wake_pushes(
        &self,
        setting: WakePushes,
        tokens: Option<PushTokens>,
    ) -> Result<(), ClientError> {
        match setting {
            WakePushes::On => {
                let tokens = tokens.ok_or(Unavailable::NoPushToken)?;
                self.0.seal_wake_push(&tokens).await?;
                self.0
                    .backend
                    .store(|session, _clock, _rng| {
                        Ok(session.settings().set_wake_pushes(StoredPushes::Enabled)?)
                    })
                    .await
            }
            WakePushes::NoWakePushes => {
                self.0.clear_wake_push();
                self.0
                    .backend
                    .store(|session, _clock, _rng| {
                        Ok(session.settings().set_wake_pushes(StoredPushes::Disabled)?)
                    })
                    .await
            }
        }
    }

    /// The gateway's key config fetched afresh through the relay and checked against its pin
    /// (pingers-sync's `check_relay`): nothing reaches the gateway, nothing about the device
    /// goes with it. Without an instance nothing is sent and the answer is `Unreachable`.
    async fn check_reachability(&self) -> Result<RelayStatus, ClientError> {
        let Some(engine) = &self.0.engine else {
            *self.0.relay.lock().unwrap_or_else(PoisonError::into_inner) = RelayStatus::Unreachable;
            return Ok(RelayStatus::Unreachable);
        };
        let result = engine.check_relay(WakeBudget::UNLIMITED).await;
        self.0.observe(&result);
        match result {
            Ok(()) => {
                // Who runs the server, for New party: the parameters, read once per process. A
                // failure here leaves the operators unknown; only a store failure is an error.
                if let Err(error @ (ClientError::Store(_) | ClientError::Records(_))) =
                    self.0.server_params().await
                {
                    return Err(error);
                }
                Ok(RelayStatus::Reachable)
            }
            Err(SyncError::NoRelay { .. }) => Ok(RelayStatus::Unreachable),
            Err(SyncError::RelayKeyMismatch) => Ok(RelayStatus::KeyMismatch),
            Err(error) => Err(error.into()),
        }
    }

    async fn section(&self) -> Result<ServerSection, ClientError> {
        let map_host = self.0.map_host().await?;
        Ok(ServerSection {
            fallbacks: Vec::new(),
            relay: *self.0.relay.lock().unwrap_or_else(PoisonError::into_inner),
            map_host,
            operators: self.0.operators(),
        })
    }
}

/// pingers-meetup-session's `GroupNetwork`.
///
/// WAITING(pingers-meetup-session): `live_head` hands back a raw `BinaryRequest` for its own
/// transport, while pingers-sync builds and sends the live request itself (`open_live`); until
/// the two meet, the live channel is not opened from here.
pub(crate) struct GroupLogs(pub(crate) Arc<Net>);

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other failure is the network's or local, to try again later"
)]
fn network_error(error: &SyncError) -> NetworkError {
    match error {
        SyncError::Status { .. } | SyncError::BadResponse { .. } => NetworkError::Rejected,
        _ => NetworkError::Unavailable,
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
        let signer = GroupSigner {
            key: auth,
            now: engine.corrected_now(),
        };
        let request =
            ApiRequest::commit(group, &request, &signer).map_err(|error| network_error(&error))?;
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

    fn live_head(
        &self,
        _group: &GroupId,
        _auth: &LogAuthKeyPair,
        _subscription: Option<SubscriptionId>,
    ) -> Result<BinaryRequest, NetworkError> {
        Err(NetworkError::Unavailable)
    }
}
