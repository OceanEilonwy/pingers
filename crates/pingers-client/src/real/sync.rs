// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-sync: the client's [`SyncPort`], pingers-wake's `ServerPort`,
//! pingers-meetup-session's `GroupNetwork`, and the outbox that sends what pingers-parties
//! returns.
//!
//! One [`Net`] per process holds the server registry ([`Servers`]: one engine per server, see
//! `servers.rs`) and the outbox. Every request goes to the server of the Party or group it is
//! for. A server this device does not know, or a client without one, answers
//! [`Unavailable::Sync`] (or `PortError::Unavailable` to a wake, which then ends that server's
//! round cleanly).
//!
//! The outbox is in memory: a request that could not be sent stays for the next flush (each
//! party command and each wake), and is lost at exit. pingers-sync queues what it can itself.
//! Each server's requests are flushed on their own: one server that cannot be reached keeps
//! only its own requests waiting.

use core::time::Duration;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use pingers_crypto::LogAuthKeyPair;
use pingers_meetup_session::{GroupNetwork, NetworkError};
use pingers_ohttp::BinaryRequest;
use pingers_parties::{LeaveRequest, Outbound, OutgoingCommit, PartiesError, PartyLifecycle};
use pingers_presence::{MailboxPurpose, OutgoingKnock};
use pingers_proto::{
    AppMessage, BoardCursor, BoardPage, CommitRequest, CommitResponse, CreateGroup, Epoch, GroupId,
    GroupLogEntry, MailboxId, MapHostUrl, Padded, PartyId, SecretText, SequenceNumber, ServerId,
    SizeClass, SubscriptionId, TimeToLive, WelcomeBytes,
};
use pingers_store::{MailboxPurpose as StoredPurpose, WakePushes as StoredPushes};
use pingers_sync::{
    ApiRequest, Committed, Delivered, GroupCall, GroupError, GroupLifetime, GroupSigner,
    GroupStart, NewLease, PoolTarget, Sending, SyncError, WakeBudget,
};
use pingers_time::{Bucket, UnixSeconds};
use pingers_wake::PortError;
use pingers_wake::ports::{LeaseUpload, ServerPort};

use super::Backend;
use super::party_log::{self, PartyLog};
use super::records::{self, Key, corrupt};
use super::servers::{Link, Servers};
use crate::ports::{ServerProbe, ServerSection, SyncPort};
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

/// An invite mailbox outlives the invite (24 hours, pingers-parties' `INVITE_LIFETIME`).
const INVITE_MAILBOX_LIFETIME: Duration = Duration::from_hours(48);

/// A request waiting for its server.
struct Waiting {
    server: ServerId,
    outbound: Outbound,
}

/// The servers and the outbox.
pub(crate) struct Net {
    pub(super) backend: Arc<Backend>,
    pub(crate) servers: Servers,
    pub(super) probes: super::probe::Probes,
    outbox: Mutex<Vec<Waiting>>,
}

impl Net {
    pub(crate) fn new(backend: Arc<Backend>, servers: Servers) -> Self {
        Self {
            backend,
            servers,
            probes: super::probe::Probes::default(),
            outbox: Mutex::new(Vec::new()),
        }
    }

    fn outbox(&self) -> std::sync::MutexGuard<'_, Vec<Waiting>> {
        self.outbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `outbound` for `server`'s next flush.
    pub(crate) fn push(&self, server: &ServerId, outbound: impl IntoIterator<Item = Outbound>) {
        self.outbox()
            .extend(outbound.into_iter().map(|outbound| Waiting {
                server: server.clone(),
                outbound,
            }));
    }

    /// Notices raised since the last call (`Notice::ClockSkew`, `Notice::RelayKeyMismatch`).
    pub(crate) fn take_notices(&self) -> Vec<Notice> {
        self.servers.take_notices()
    }

    /// The server a Party lives on.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`] (`UnknownParty`); a store failure.
    pub(crate) async fn party_server(&self, party: PartyId) -> Result<ServerId, ClientError> {
        self.backend
            .store(move |session, _clock, _rng| {
                Ok(session
                    .parties()
                    .get(&party)?
                    .ok_or(PartiesError::UnknownParty)?
                    .server)
            })
            .await
    }

    /// The link to `server`, or [`Unavailable::Sync`].
    pub(crate) async fn link(&self, server: &ServerId) -> Result<Arc<Link>, ClientError> {
        self.servers.link(server).await.map_err(|error| {
            if matches!(error, ClientError::Unavailable(Unavailable::UnknownServer)) {
                Unavailable::Sync.into()
            } else {
                error
            }
        })
    }

    /// The account on `server`, made now if it is not yet (a new Party or a join).
    ///
    /// # Errors
    ///
    /// [`ClientError::RegistrationCodeRequired`]; [`ClientError::RegistrationCodeRefused`];
    /// [`Unavailable::Sync`] for an unknown server; a store failure.
    pub(crate) async fn account(&self, server: &ServerId) -> Result<(), ClientError> {
        self.link(server).await?.account().await
    }

    /// The registration code this device registered with on `server`, for its invite links.
    ///
    /// # Errors
    ///
    /// [`ClientError::Sync`] with a store failure. An unknown server has none.
    pub(crate) async fn shared_registration_code(
        &self,
        server: &ServerId,
    ) -> Result<Option<SecretText>, ClientError> {
        match self.servers.link(server).await {
            Ok(link) => Ok(link.engine.shared_registration_code().await?),
            Err(ClientError::Unavailable(Unavailable::UnknownServer)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Sends every waiting request, server by server. One that could not be sent now stays for
    /// the next flush; one the server refused is dropped.
    ///
    /// # Errors
    ///
    /// A storage failure; network failures only keep the request.
    pub(crate) async fn flush(&self) -> Result<(), ClientError> {
        for link in self.servers.all().await? {
            self.flush_server(&link).await?;
        }
        Ok(())
    }

    /// [`Net::flush`] for one server.
    async fn flush_server(&self, link: &Link) -> Result<(), ClientError> {
        let waiting: Vec<Outbound> = {
            let mut outbox = self.outbox();
            let (mine, others): (Vec<Waiting>, Vec<Waiting>) = core::mem::take(&mut *outbox)
                .into_iter()
                .partition(|waiting| waiting.server == link.server);
            *outbox = others;
            mine.into_iter().map(|waiting| waiting.outbound).collect()
        };
        if waiting.is_empty() {
            return Ok(());
        }
        link.prepare_credentials().await?;
        let mut kept = Vec::new();
        for outbound in waiting {
            match self.send(link, outbound).await {
                Ok(Sent::Later(outbound)) => kept.push(*outbound),
                Err(error @ (ClientError::Store(_) | ClientError::Records(_))) => {
                    self.push(&link.server, kept);
                    return Err(error);
                }
                // Sent; or refused, or not ours to retry (a commit pingers-parties dropped).
                Ok(Sent::Done) | Err(_) => {}
            }
        }
        self.push(&link.server, kept);
        Ok(())
    }

    fn keep_until(&self) -> Sending {
        let now = party_log::now(&self.backend.outside);
        let until = UnixSeconds::new(now.get().saturating_add(KEEP_FOR.as_secs()));
        Sending::or_keep_until(until, WakeBudget::UNLIMITED)
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "Outbound is non-exhaustive; a new kind is passed over until it is mapped"
    )]
    async fn send(&self, link: &Link, outbound: Outbound) -> Result<Sent, ClientError> {
        let engine = &link.engine;
        let outbound = match outbound {
            Outbound::Commit(commit) => return self.commit(link, commit).await,
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
            Outbound::Leave(leave) => self.leave(link, leave).await,
            _ => return Ok(Sent::Done),
        };
        link.observe(&result);
        match result {
            Ok(true) => Ok(Sent::Done),
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

    async fn leave(&self, link: &Link, leave: &LeaveRequest) -> Result<bool, SyncError> {
        let call = GroupCall {
            group: &leave.group,
            key: &leave.auth,
        };
        if !delivered(
            link.engine
                .post_group_message(call, &leave.proposal, self.keep_until())
                .await,
        )? {
            return Ok(false);
        }
        if let Some(subscription) = &leave.subscription {
            link.engine
                .delete_subscription(call, subscription, WakeBudget::UNLIMITED)
                .await?;
        }
        Ok(true)
    }

    /// Commits this device's commit, then posts its Welcome. A commit that could not be sent
    /// stays pending in pingers-parties and in the outbox.
    async fn commit(&self, link: &Link, commit: OutgoingCommit) -> Result<Sent, ClientError> {
        let engine = &link.engine;
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
        self.push(&link.server, core::mem::take(&mut log.outbound));
        match result {
            Ok(Committed::Accepted(_)) => {
                if let Some(welcome) = log.accepted.take().and_then(|commit| commit.welcome) {
                    let posted = engine
                        .post_to_mailbox(&welcome.mailbox, &welcome.body, self.keep_until())
                        .await;
                    link.observe(&posted);
                    let _: Delivered<SequenceNumber> = posted?;
                }
                Ok(Sent::Done)
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
                link.observe::<()>(&Err(error));
                match log.pending.take() {
                    Some(commit) if transient => {
                        Ok(Sent::Later(Box::new(Outbound::Commit(commit))))
                    }
                    _ => Ok(Sent::Done),
                }
            }
            // Nothing left to commit, or the conflicts ran out: pingers-parties dropped it.
            Ok(Committed::NothingToCommit) | Err(_) => Ok(Sent::Done),
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

    /// Pulls the log of every Party on `link`'s server (design B.4.7: before every board sync).
    async fn pull_server(&self, link: &Link) -> Result<bool, ClientError> {
        let server = link.server.clone();
        let parties = self
            .backend
            .store(move |session, _clock, _rng| party_log::parties_on(session, &server))
            .await?;
        let mut changed = false;
        for party in parties {
            let mut log =
                PartyLog::load(&self.backend.database, &self.backend.outside, party, None)?;
            let result = link
                .engine
                .pull_group(&log.group(), &mut log, WakeBudget::UNLIMITED)
                .await;
            self.push(&link.server, core::mem::take(&mut log.outbound));
            changed |= log.changed;
            match result {
                Ok(_) => {}
                Err(GroupError::Sync(error)) => {
                    let result = Err(error);
                    link.observe::<()>(&result);
                    result?;
                }
                Err(GroupError::Owner(error)) => return Err(error),
                // GroupError is non-exhaustive; nothing else is a pull's to report.
                Err(_conflicts) => {}
            }
        }
        Ok(changed)
    }

    /// Registers one invite mailbox on `server` for the pool, with the alert seal when wake-up
    /// pushes are sealed: an invite reads join requests from it until the invite expires.
    ///
    /// # Errors
    ///
    /// [`Unavailable::Sync`] for an unknown server; [`ClientError::Sync`].
    pub(crate) async fn refill_invite_pool(&self, server: &ServerId) -> Result<(), ClientError> {
        let link = self.link(server).await?;
        link.prepare_credentials().await?;
        let target = PoolTarget {
            purpose: StoredPurpose::Invite,
            size: 1,
            lifetime: TimeToLive::new(INVITE_MAILBOX_LIFETIME).map_err(|_never| corrupt())?,
        };
        let refilled = link
            .engine
            .refill_mailbox_pool(&target, WakeBudget::UNLIMITED)
            .await;
        link.observe(&refilled);
        let _registered: pingers_sync::PoolRefill = refilled?;
        Ok(())
    }

    /// The built-in server's parameters, once per process, with its map host kept in a record
    /// for the next start (the app's map host chain reads it before the first wake).
    ///
    /// # Errors
    ///
    /// [`Unavailable::Sync`] without a built-in server; [`ClientError::Sync`]; a store failure.
    pub(crate) async fn built_in_params(&self) -> Result<(), ClientError> {
        let link = self.servers.built_in_link().await?;
        let Some(fetched) = link.fetch_params_once().await? else {
            return Ok(());
        };
        let map_host = fetched.map_host;
        self.backend
            .store(move |session, _clock, _rng| match &map_host {
                Some(url) => records::write(session, &Key::ServerMapHost, |writer| {
                    writer.text(url.as_str());
                }),
                None => records::delete(session, &Key::ServerMapHost),
            })
            .await
    }

    /// The parameters of `link`'s server, once per process (for the built-in server with its
    /// map host recorded). Only a store failure is an error: anything else is the next call's
    /// to retry.
    async fn params_of(&self, link: &Link) -> Result<(), ClientError> {
        let result = if self.servers.built_in() == Some(&link.server) {
            self.built_in_params().await
        } else {
            link.server_params().await.map(drop)
        };
        match result {
            Err(error @ (ClientError::Store(_) | ClientError::Records(_))) => Err(error),
            Ok(()) | Err(_) => Ok(()),
        }
    }

    /// The map host the built-in server advertises, as last recorded (each process records it
    /// when it first reads the parameters).
    ///
    /// # Errors
    ///
    /// A storage failure, or a corrupt record.
    pub(crate) async fn map_host(&self) -> Result<Option<MapHostUrl>, ClientError> {
        self.backend
            .store(|session, _clock, _rng| {
                records::read(session, &Key::ServerMapHost, |reader| {
                    MapHostUrl::new(reader.text()?).map_err(|_invalid| corrupt())
                })
            })
            .await
    }

    /// Whether "No wake-up pushes" is set.
    async fn wake_pushes(&self) -> Result<StoredPushes, ClientError> {
        self.backend
            .store(|session, _clock, _rng| Ok(session.settings().wake_pushes()?))
            .await
    }

    /// Makes sure `server`'s engine holds seals (a join request carries a sealed
    /// subscription when wake-up pushes are on).
    ///
    /// # Errors
    ///
    /// [`Unavailable::WakePushesOff`] with "No wake-up pushes" set;
    /// [`Unavailable::NoPushToken`]; as `Link::seal_wake_push`.
    pub(crate) async fn ensure_wake_push(&self, server: &ServerId) -> Result<(), ClientError> {
        if self.wake_pushes().await? == StoredPushes::Disabled {
            return Err(Unavailable::WakePushesOff.into());
        }
        self.link(server).await?.ensure_wake_push().await
    }

    /// Seals the platform's tokens for every server. A server whose push relay cannot be
    /// reached or has no pins is passed over (its mailboxes wake nobody until a later seal),
    /// unless no server could be sealed for at all.
    ///
    /// # Errors
    ///
    /// A store failure; [`Unavailable::Sync`] without any server; the first server's error
    /// when no server was sealed for.
    async fn seal_everywhere(&self, tokens: &PushTokens) -> Result<(), ClientError> {
        let links = self.servers.all().await?;
        let mut first_error = None;
        let mut sealed = false;
        for link in &links {
            match link.seal_wake_push(tokens).await {
                Ok(()) => sealed = true,
                Err(error @ (ClientError::Store(_) | ClientError::Records(_))) => {
                    return Err(error);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) if !sealed => Err(error),
            None if links.is_empty() => Err(Unavailable::Sync.into()),
            _ => Ok(()),
        }
    }

    async fn clear_everywhere(&self) -> Result<(), ClientError> {
        for link in self.servers.all().await? {
            link.clear_wake_push();
        }
        Ok(())
    }
}

/// What sending one request came to.
enum Sent {
    Done,
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

/// A server call for a wake: its result observed on the link, mapped to a port error.
async fn on<T>(
    link: &Link,
    call: impl core::future::Future<Output = Result<T, SyncError>>,
) -> Result<T, PortError> {
    let result = call.await;
    link.observe(&result);
    result.map_err(|error| port_error(&error.into()))
}

impl Net {
    async fn port_link(&self, server: &ServerId) -> Result<Arc<Link>, PortError> {
        self.link(server)
            .await
            .map_err(|_unknown| PortError::Unavailable)
    }

    /// Posts a Meetup Welcome to the knocker's reply mailbox on the Party's server, padded to a
    /// mailbox class. If it never arrives, a Meetup this device just created ends by itself
    /// after 30 minutes alone (design B.5).
    pub(crate) async fn post_welcome(
        &self,
        party: PartyId,
        mailbox: MailboxId,
        welcome: WelcomeBytes,
    ) -> Result<(), PortError> {
        let server = self
            .party_server(party)
            .await
            .map_err(|_unknown| PortError::Unavailable)?;
        let link = self.port_link(&server).await?;
        let body = Padded::pad_smallest(welcome.as_bytes(), &SizeClass::MAILBOX_POST)
            .map_err(|_too_big| PortError::Unavailable)?;
        match on(
            &link,
            link.engine
                .post_to_mailbox(&mailbox, &body, self.keep_until()),
        )
        .await?
        {
            Delivered::Done(_) | Delivered::Queued(_) => Ok(()),
            Delivered::Dropped(_) => Err(PortError::Unreachable),
        }
    }
}

impl ServerPort for Net {
    async fn pull_group_logs(&self, server: &ServerId) -> Result<(), PortError> {
        let link = self.port_link(server).await?;
        // The account, its leaf and the token stock first: each step after this one spends a
        // credential. Once in place this costs a few store reads.
        link.prepare_credentials()
            .await
            .map_err(|error| port_error(&error))?;
        // A Party may have changed: the wake's next Party view reads it again.
        self.flush_server(&link)
            .await
            .map_err(|error| port_error(&error))?;
        // The server's parameters once per process (the map host, the push relays); a failure
        // here is the next wake's to retry and does not stop the pull.
        self.params_of(&link)
            .await
            .map_err(|error| port_error(&error))?;
        self.pull_server(&link)
            .await
            .map(drop)
            .map_err(|error| port_error(&error))
    }

    async fn upload_lease(
        &self,
        server: &ServerId,
        upload: LeaseUpload,
    ) -> Result<Option<BoardPage>, PortError> {
        let link = self.port_link(server).await?;
        let lease = NewLease {
            lease: upload.lease,
            revoke_key: upload.revoke_key,
            read: upload.read,
        };
        match on(
            &link,
            link.engine.upload_lease(lease, WakeBudget::UNLIMITED),
        )
        .await?
        {
            Delivered::Done(response) => Ok(response.page),
            Delivered::Queued(_) | Delivered::Dropped(_) => Err(PortError::Unreachable),
        }
    }

    async fn board_page(
        &self,
        server: &ServerId,
        cursor: BoardCursor,
    ) -> Result<BoardPage, PortError> {
        let link = self.port_link(server).await?;
        on(
            &link,
            link.engine.board_page(&cursor, WakeBudget::UNLIMITED),
        )
        .await
    }

    async fn post_knock(&self, server: &ServerId, knock: OutgoingKnock) -> Result<(), PortError> {
        let link = self.port_link(server).await?;
        let body = Padded::from_wire(knock.post).map_err(|_bad| PortError::Unavailable)?;
        let sending = Sending::or_drop(WakeBudget::UNLIMITED);
        match on(
            &link,
            link.engine.post_to_mailbox(&knock.mailbox, &body, sending),
        )
        .await?
        {
            Delivered::Done(_) | Delivered::Queued(_) => Ok(()),
            Delivered::Dropped(_) => Err(PortError::Unreachable),
        }
    }

    async fn read_mailbox(
        &self,
        server: &ServerId,
        mailbox: MailboxId,
    ) -> Result<Vec<Vec<u8>>, PortError> {
        let link = self.port_link(server).await?;
        let messages = on(
            &link,
            link.engine
                .read_mailbox(&mailbox, None, WakeBudget::UNLIMITED),
        )
        .await?;
        Ok(messages
            .messages
            .into_iter()
            .map(|message| message.body.into_bytes())
            .collect())
    }

    fn take_mailbox(
        &self,
        server: &ServerId,
        purpose: MailboxPurpose,
        lease_start: Bucket,
    ) -> Option<MailboxId> {
        self.servers
            .loaded(server)?
            .engine
            .take_mailbox_for_lease(stored_purpose(purpose), lease_start)
            .ok()
            .flatten()
    }

    async fn refill_mailbox_pool(&self, server: &ServerId) -> Result<(), PortError> {
        let link = self.port_link(server).await?;
        let target = PoolTarget {
            purpose: StoredPurpose::Knock,
            size: KNOCK_POOL,
            lifetime: TimeToLive::new(MAILBOX_LIFETIME).map_err(|_never| PortError::Unavailable)?,
        };
        on(
            &link,
            link.engine
                .refill_mailbox_pool(&target, WakeBudget::UNLIMITED),
        )
        .await
        .map(drop)
    }
}

/// The client's [`SyncPort`].
pub(crate) struct Sync(pub(crate) Arc<Net>);

#[async_trait]
impl SyncPort for Sync {
    /// New mailboxes and subscriptions on every server are sealed with the new tokens from now
    /// on; with pushes off nothing is sealed. Mailboxes and subscriptions already registered
    /// keep their old seal until they expire or are renewed.
    async fn push_tokens_changed(&self, tokens: Option<PushTokens>) -> Result<(), ClientError> {
        match (self.0.wake_pushes().await?, tokens) {
            (StoredPushes::Disabled, _) => Ok(()),
            (StoredPushes::Enabled, None) => self.0.clear_everywhere().await,
            (StoredPushes::Enabled, Some(tokens)) => self.0.seal_everywhere(&tokens).await,
        }
    }

    /// "No wake-up pushes" seals no token anywhere; turning them on fetches each server's push
    /// relay key (checked against its pins) and seals the platform's token to it.
    async fn set_wake_pushes(
        &self,
        setting: WakePushes,
        tokens: Option<PushTokens>,
    ) -> Result<(), ClientError> {
        match setting {
            WakePushes::On => {
                let tokens = tokens.ok_or(Unavailable::NoPushToken)?;
                self.0.seal_everywhere(&tokens).await?;
                self.0
                    .backend
                    .store(|session, _clock, _rng| {
                        Ok(session.settings().set_wake_pushes(StoredPushes::Enabled)?)
                    })
                    .await
            }
            WakePushes::NoWakePushes => {
                self.0.clear_everywhere().await?;
                self.0
                    .backend
                    .store(|session, _clock, _rng| {
                        Ok(session.settings().set_wake_pushes(StoredPushes::Disabled)?)
                    })
                    .await
            }
        }
    }

    /// Each server's gateway key config fetched afresh through its relay and checked against
    /// its pins (pingers-sync's `check_relay`): nothing reaches a gateway, nothing about the
    /// device goes with it. Each server's status shows in its Parties' settings; the answer is
    /// the built-in server's (or, without one, the first server's). Without any server nothing
    /// is sent and the answer is `Unreachable`.
    async fn check_reachability(&self) -> Result<RelayStatus, ClientError> {
        let links = self.0.servers.all().await?;
        let mut statuses = Vec::new();
        for link in &links {
            let result = link.engine.check_relay(WakeBudget::UNLIMITED).await;
            link.observe(&result);
            let status = match result {
                Ok(()) => {
                    // Who runs the server, for New party and the Party settings: the
                    // parameters, read once per process.
                    self.0.params_of(link).await?;
                    RelayStatus::Reachable
                }
                Err(SyncError::NoRelay { .. } | SyncError::Transport(_)) => {
                    link.set_relay_status(RelayStatus::Unreachable);
                    RelayStatus::Unreachable
                }
                Err(SyncError::RelayKeyMismatch) => RelayStatus::KeyMismatch,
                Err(error) => return Err(error.into()),
            };
            statuses.push((link.server.clone(), status));
        }
        let built_in = self.0.servers.built_in();
        Ok(statuses
            .iter()
            .find(|(server, _status)| Some(server) == built_in)
            .or_else(|| statuses.first())
            .map_or(RelayStatus::Unreachable, |(_server, status)| *status))
    }

    async fn probe_server(&self, address: String) -> Result<ServerProbe, ClientError> {
        self.0.probe_server(&address).await
    }

    async fn trust_server(&self, server: ServerId) -> Result<(), ClientError> {
        self.0.trust_server(&server).await
    }

    /// The code goes to the server's engine as the user's; a refusal is
    /// [`ClientError::RegistrationCodeRefused`], distinct from a server that is not reached.
    async fn submit_registration_code(
        &self,
        server: Option<ServerId>,
        code: SecretText,
    ) -> Result<(), ClientError> {
        let server = match server {
            Some(server) => server,
            None => self
                .0
                .servers
                .built_in()
                .cloned()
                .ok_or(Unavailable::Sync)?,
        };
        self.0
            .link(&server)
            .await?
            .offer_code(code, pingers_sync::CodeSource::User)
            .await
    }

    async fn section(&self) -> Result<ServerSection, ClientError> {
        let map_host = self.0.map_host().await?;
        let built_in = self.0.servers.built_in_link().await.ok();
        Ok(ServerSection {
            fallbacks: Vec::new(),
            relay: built_in
                .as_ref()
                .map_or(RelayStatus::Unreachable, |link| link.relay_status()),
            map_host,
            operators: built_in.and_then(|link| link.operators()),
        })
    }
}

/// pingers-meetup-session's `GroupNetwork`: every call goes to the server of the Meetup's
/// Party.
///
/// WAITING(pingers-meetup-session): `live_head` hands back a raw `BinaryRequest` for its own
/// transport, while pingers-sync builds and sends the live request itself (`open_live`); until
/// the two meet, the live channel is not opened from here.
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
        SyncError::Status { .. } | SyncError::BadResponse { .. } => NetworkError::Rejected,
        _ => NetworkError::Unavailable,
    }
}

impl GroupLogs {
    async fn link(&self, _group: &GroupId) -> Result<Arc<Link>, NetworkError> {
        self.net
            .link(&self.server)
            .await
            .map_err(|_unknown| NetworkError::Unavailable)
    }
}

#[async_trait]
impl GroupNetwork for GroupLogs {
    async fn create_group(&self, request: CreateGroup) -> Result<(), NetworkError> {
        let link = self.link(&request.group_id).await?;
        let start = GroupStart {
            lifetime: GroupLifetime::Meetup,
            epoch: Epoch::new(1),
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
        let link = self.link(group).await?;
        let engine = &link.engine;
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
        link.observe(&result);
        result.map_err(|error| network_error(&error))
    }

    async fn post_message(
        &self,
        group: &GroupId,
        auth: &LogAuthKeyPair,
        message: AppMessage,
    ) -> Result<(), NetworkError> {
        let link = self.link(group).await?;
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
        let link = self.link(group).await?;
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
        let link = self.link(group).await?;
        let result = link
            .engine
            .delete_group(GroupCall { group, key: auth }, WakeBudget::UNLIMITED)
            .await;
        link.observe(&result);
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
