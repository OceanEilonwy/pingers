// SPDX-License-Identifier: AGPL-3.0-only
//! The async half: creating or joining, the live channel, the timers, and the task that owns
//! them while the Meetup lasts.

use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use pingers_crypto::{GrantKeyPair, GrantSecretKey, LogAuthKeyPair, MeetupPsk};
use pingers_geo::Fix;
use pingers_meetup::{CadenceMode, EndReason, MeetupAction, MeetupEvent, MeetupInput};
use pingers_mls::{
    CipherSuitePolicy, KeyPackagePurpose, MeetupCreation, MeetupPskBinding, MeetupPskId,
};
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, CreateGroup, GroupId, KeyPackageBytes, LiveFrame,
    PartyId, SeenSegment, SequenceNumber, SpotId, Subscription, SubscriptionId, Urgency,
    WelcomeBytes, WireDecode as _,
};
use pingers_store::{Database, Meetup};
use pingers_time::{Clock, Timer, UnixSeconds};
use pingers_transport::{DuplexStream, Transport};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::group::{self, Local, Outgoing};
use crate::state::{AddMember, Core, CoreSetup, Effects, PendingCommit, Proposals};
use crate::view::{ChannelState, MeetupView};
use crate::{GroupNetwork, SessionError};

/// The randomness a Meetup draws keys, nonces, dummy frames and padding from. Production
/// passes an OS-seeded CSPRNG; tests a seeded one (style guide §6).
pub type SessionRng = Box<dyn rand_core::CryptoRng + Send>;

/// The longest wait between attempts to re-open the live channel.
pub const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);

/// Commands queued for the task before senders wait.
const COMMAND_BUFFER: usize = 16;

/// What every live Meetup on this device shares.
#[derive(Clone)]
pub struct Runtime {
    /// The encrypted store (Meetup rows, chat, MLS state, allow-list).
    pub database: Arc<Database>,
    /// The router; the live channel uses its `Stream` class.
    pub transport: Arc<dyn Transport>,
    /// The group log, through pingers-sync.
    pub network: Arc<dyn GroupNetwork>,
    /// Wall time, for the state machine's timers.
    pub clock: Arc<dyn Clock>,
    /// Monotonic sleeps until those timers fall due.
    pub timer: Arc<dyn Timer>,
}

/// Which screen state sets this device's cadence. Each member's own app sets its own; nothing
/// asks other members to go live (decided 2026-11-13).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeetupScreen {
    /// The Meetup screen or the compass is open: frames every 2 s.
    Open,
    /// It closed, or the app went to the background: frames every 60 s.
    Closed,
}

/// Why a live Meetup finished on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Ended {
    /// The user left (or the exit was confirmed); the others carry on.
    Left,
    /// This device was alone for 30 minutes; the group was deleted on the server too.
    AloneTooLong,
    /// Another member removed this device.
    Removed,
}

/// Creating a Meetup after a knock passed this device's liveness check (design B.5, G.4).
pub struct CreateMeetup {
    /// The Party it forms in.
    pub party: PartyId,
    /// The Spot it forms at.
    pub spot: SpotId,
    /// A random 32-byte group id.
    pub group: GroupId,
    /// The external PSK id binding it to the Party and Spot.
    pub psk_id: MeetupPskId,
    /// `PSK_meet(b)`.
    pub psk: MeetupPsk,
    /// The knocker's KeyPackage.
    pub knocker: KeyPackageBytes,
    /// The creator's and the knocker's sealed-push subscriptions.
    pub subscriptions: Vec<Subscription>,
    /// This device's own subscription, named on the live channel.
    pub own_subscription: Option<SubscriptionId>,
    /// The Party's ciphersuite.
    pub suite: CipherSuitePolicy,
}

/// Joining a Meetup from the Welcome its creator or a member sent (design B.5, G.4, G.5).
pub struct JoinMeetup {
    /// The Party.
    pub party: PartyId,
    /// The Spot.
    pub spot: SpotId,
    /// The Welcome from the reply mailbox.
    pub welcome: WelcomeBytes,
    /// The PSK id the Meetup must be bound to.
    pub psk_id: MeetupPskId,
    /// `PSK_meet(b)`.
    pub psk: MeetupPsk,
    /// The grant secret whose public half is in the KeyPackage this device knocked with
    /// ([`KnockKeyPackage::grant_key`]).
    pub grant_key: GrantSecretKey,
    /// This device's own subscription, named on the live channel.
    pub own_subscription: Option<SubscriptionId>,
}

/// A Meetup KeyPackage for a knock, with the grant secret to keep for [`JoinMeetup`].
pub struct KnockKeyPackage {
    /// The KeyPackage, with the `pingers_grant_key` leaf extension.
    pub key_package: KeyPackageBytes,
    /// The grant secret; pass it to [`join`] when the Welcome comes.
    pub grant_key: GrantSecretKey,
}

/// A Meetup this device created: the running task, and the Welcome for the knocker's reply
/// mailbox.
pub struct Created {
    /// The running Meetup.
    pub handle: MeetupHandle,
    /// Its events.
    pub events: mpsc::UnboundedReceiver<MeetupEvent>,
    /// Post this to the knocker's reply mailbox.
    pub welcome: WelcomeBytes,
}

/// A Meetup this device joined.
pub struct Joined {
    /// The running Meetup.
    pub handle: MeetupHandle,
    /// Its events.
    pub events: mpsc::UnboundedReceiver<MeetupEvent>,
}

/// A fresh Meetup KeyPackage and grant key pair for a knock (design B.6.1).
///
/// # Errors
///
/// [`SessionError::NotFound`] without an identity; [`SessionError::Crypto`],
/// [`SessionError::Mls`], [`SessionError::Store`].
pub async fn knock_key_package(
    database: &Arc<Database>,
    suite: CipherSuitePolicy,
    mut rng: SessionRng,
) -> Result<KnockKeyPackage, SessionError> {
    run_blocking(database, move |local| {
        let grant = GrantKeyPair::generate(&mut rng)?;
        let key_package = local.mls().create_key_package(
            KeyPackagePurpose::Meetup {
                grant_key: &grant.public,
            },
            suite,
            &mut rng,
        )?;
        Ok(KnockKeyPackage {
            key_package,
            grant_key: grant.secret,
        })
    })
    .await
}

/// Creates a Meetup with the knocker: the group with the external PSK, the group on the
/// server, and the first commit; then starts it. Nothing is left behind if a step fails.
///
/// # Errors
///
/// [`SessionError::Mls`] (`NotInPartyRoster` for a knocker outside the Party),
/// [`SessionError::Network`], [`SessionError::CommitConflict`], [`SessionError::NotFound`],
/// [`SessionError::Store`].
pub async fn create(
    runtime: &Runtime,
    request: CreateMeetup,
    mut rng: SessionRng,
) -> Result<Created, SessionError> {
    let CreateMeetup {
        party,
        spot,
        group,
        psk_id,
        psk,
        knocker,
        subscriptions,
        own_subscription,
        suite,
    } = request;
    let prepared = run_blocking(&runtime.database, move |local| {
        let grant = GrantKeyPair::generate(&mut rng)?;
        let roster = group::party_context(local, &party)?.roster;
        let created = local.mls().create_meetup_group(
            &MeetupCreation {
                group_id: group,
                psk: MeetupPskBinding {
                    id: psk_id,
                    value: &psk,
                },
                party_roster: &roster,
                first_member: &knocker,
                own_grant_key: &grant.public,
                suite,
            },
            &mut rng,
        )?;
        Ok(Prepared {
            auth: created.group.log_auth_key_pair()?,
            next_auth: created.group.pending_log_auth_key_pair()?,
            epoch: created.group.epoch(),
            commit: created.commit,
            grant_key: grant.secret,
            rng,
        })
    })
    .await?;
    let welcome = match register(&*runtime.network, group, &prepared, subscriptions).await {
        Ok(welcome) => welcome,
        Err(error) => {
            let _cleanup = run_blocking(&runtime.database, move |local| {
                Ok(local.mls().delete_group(&group)?)
            })
            .await;
            return Err(error);
        }
    };
    let Prepared { grant_key, rng, .. } = prepared;
    let now = runtime.clock.now();
    let core = run_blocking(&runtime.database, move |local| {
        local.mls().meetup_group(&group)?.merge_pending_commit()?;
        start_core(
            local,
            Started {
                party,
                spot,
                group,
                grant_key,
                rng,
            },
            now,
        )
    })
    .await?;
    let Joined { handle, events } = spawn(runtime, core, own_subscription);
    Ok(Created {
        handle,
        events,
        welcome,
    })
}

/// A Meetup group created locally, its first commit still pending.
struct Prepared {
    auth: LogAuthKeyPair,
    next_auth: LogAuthKeyPair,
    epoch: pingers_proto::Epoch,
    commit: pingers_mls::Commit,
    grant_key: GrantSecretKey,
    rng: SessionRng,
}

/// Registers a new group on the server and posts its first commit; returns the Welcome.
async fn register(
    network: &dyn GroupNetwork,
    group: GroupId,
    prepared: &Prepared,
    subscriptions: Vec<Subscription>,
) -> Result<WelcomeBytes, SessionError> {
    network
        .create_group(CreateGroup {
            group_id: group,
            auth_key: prepared.auth.public_key(),
            subscriptions,
            state: None,
        })
        .await?;
    let response = network
        .post_commit(
            &group,
            &prepared.auth,
            CommitRequest {
                epoch: prepared.epoch,
                commit: prepared.commit.commit.clone(),
                next_auth_key: prepared.next_auth.public_key(),
                state: None,
                subscriptions_added: Vec::new(),
                subscriptions_removed: Vec::new(),
                wake: Urgency::Alert,
            },
        )
        .await?;
    match response {
        CommitResponse::Accepted { .. } => prepared
            .commit
            .welcome
            .clone()
            .ok_or(SessionError::Internal),
        CommitResponse::Conflict { .. } => Err(SessionError::CommitConflict),
    }
}

/// Joins a Meetup from a Welcome, checking the PSK and the Party roster, and starts it. The
/// first catch-up of the group log brings the meet point snapshot, never earlier chat.
///
/// # Errors
///
/// [`SessionError::Mls`] (`PskMismatch`, `NotInPartyRoster`, `InvalidMessage`),
/// [`SessionError::NotFound`], [`SessionError::Store`].
pub async fn join(
    runtime: &Runtime,
    request: JoinMeetup,
    rng: SessionRng,
) -> Result<Joined, SessionError> {
    let JoinMeetup {
        party,
        spot,
        welcome,
        psk_id,
        psk,
        grant_key,
        own_subscription,
    } = request;
    let now = runtime.clock.now();
    let core = run_blocking(&runtime.database, move |local| {
        let roster = group::party_context(local, &party)?.roster;
        let group = local
            .mls()
            .join_meetup_group(
                &welcome,
                &MeetupPskBinding {
                    id: psk_id,
                    value: &psk,
                },
                &roster,
            )?
            .group_id();
        start_core(
            local,
            Started {
                party,
                spot,
                group,
                grant_key,
                rng,
            },
            now,
        )
    })
    .await?;
    Ok(spawn(runtime, core, own_subscription))
}

/// Deletes every Meetup left in the store, with its MLS state: call once at launch. A
/// Meetup's state machine lives only in its task, so one that outlived its task (the process
/// was killed) cannot be resumed, and its keys must not linger (design B.5).
///
/// # Errors
///
/// [`SessionError::Store`].
pub async fn discard_stale(database: &Arc<Database>) -> Result<usize, SessionError> {
    let database = Arc::clone(database);
    tokio::task::spawn_blocking(move || {
        let session = database.session()?;
        let meetups = session.meetups().list()?;
        for meetup in &meetups {
            session.meetups().end(&meetup.group)?;
        }
        Ok(meetups.len())
    })
    .await
    .map_err(|_| SessionError::Internal)?
}

/// A running Meetup. Dropping it stops the task at once without leaving the group; the
/// state is then discarded by [`discard_stale`] at the next launch. Use [`MeetupHandle::leave`]
/// to leave properly.
pub struct MeetupHandle {
    group: GroupId,
    commands: mpsc::Sender<Command>,
    view: watch::Receiver<MeetupView>,
    task: Option<JoinHandle<Result<Ended, SessionError>>>,
}

impl MeetupHandle {
    /// The Meetup group.
    #[must_use]
    pub const fn group(&self) -> GroupId {
        self.group
    }

    /// The latest snapshot for the screens; it changes after every step.
    #[must_use]
    pub fn view(&self) -> watch::Receiver<MeetupView> {
        self.view.clone()
    }

    /// The Meetup screen or compass opened or closed on this device: switches this device's
    /// own frame cadence.
    ///
    /// # Errors
    ///
    /// [`SessionError::Ended`] once the Meetup is over.
    pub async fn screen(&self, screen: MeetupScreen) -> Result<(), SessionError> {
        let mode = match screen {
            MeetupScreen::Open => CadenceMode::Live,
            MeetupScreen::Closed => CadenceMode::Background,
        };
        self.input(MeetupInput::Cadence(mode)).await
    }

    /// A new fix of this device's position; the next location frame carries it.
    ///
    /// # Errors
    ///
    /// [`SessionError::Ended`]; [`SessionError::Meetup`] for a fix outside the wire range.
    pub async fn own_fix(&self, fix: Fix) -> Result<(), SessionError> {
        self.input(MeetupInput::OwnFix(fix)).await
    }

    /// The board segment hashes this device saw, sent as its first message (design B.4.3).
    ///
    /// # Errors
    ///
    /// [`SessionError::Ended`].
    pub async fn board_seen(&self, segments: Vec<SeenSegment>) -> Result<(), SessionError> {
        self.input(MeetupInput::OwnBoardSeen(segments)).await
    }

    /// Something the user did: ask, answer, revoke, chat, meet point, visibility.
    ///
    /// # Errors
    ///
    /// [`SessionError::Meetup`] for an action the state machine refuses;
    /// [`SessionError::Network`] if a stored frame (chat, meet point) could not be posted;
    /// [`SessionError::Ended`].
    pub async fn act(&self, action: MeetupAction) -> Result<(), SessionError> {
        self.input(MeetupInput::Action(action)).await
    }

    /// Adds a newcomer whose knock passed this device's liveness check, and returns the
    /// Welcome for its reply mailbox.
    ///
    /// # Errors
    ///
    /// [`SessionError::CommitConflict`] if another member's commit won (the newcomer is
    /// probably in already: stop); [`SessionError::Mls`] (`NotInPartyRoster`);
    /// [`SessionError::Network`]; [`SessionError::Ended`].
    pub async fn add_member(&self, newcomer: AddMember) -> Result<WelcomeBytes, SessionError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Add {
            newcomer: Box::new(newcomer),
            reply,
        })
        .await?;
        answer.await.map_err(|_| SessionError::Ended)?
    }

    /// Leaves: sends this device's self-remove proposal (or, when alone, deletes the group
    /// on the server), closes the channel and deletes the Meetup's keys and content.
    ///
    /// # Errors
    ///
    /// The error that stopped the task, if it had already failed; [`SessionError::Store`] if
    /// the state could not be deleted.
    pub async fn leave(self) -> Result<Ended, SessionError> {
        let (reply, answer) = oneshot::channel();
        if self.send(Command::Leave { reply }).await.is_ok() {
            let _left = answer.await;
        }
        self.finished().await
    }

    /// Waits until the Meetup ends on its own (alone too long, or removed).
    ///
    /// # Errors
    ///
    /// The error that stopped the task.
    pub async fn finished(mut self) -> Result<Ended, SessionError> {
        let task = self.task.take().ok_or(SessionError::Ended)?;
        task.await.map_err(|_| SessionError::Internal)?
    }

    async fn input(&self, input: MeetupInput) -> Result<(), SessionError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Input {
            input: Box::new(input),
            reply,
        })
        .await?;
        answer.await.map_err(|_| SessionError::Ended)?
    }

    async fn send(&self, command: Command) -> Result<(), SessionError> {
        self.commands
            .send(command)
            .await
            .map_err(|_| SessionError::Ended)
    }
}

impl Drop for MeetupHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

enum Command {
    Input {
        input: Box<MeetupInput>,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    Add {
        newcomer: Box<AddMember>,
        reply: oneshot::Sender<Result<WelcomeBytes, SessionError>>,
    },
    Leave {
        reply: oneshot::Sender<()>,
    },
}

/// What woke the task.
enum Wake {
    Command(Option<Command>),
    Received(Result<Option<Vec<u8>>, pingers_transport::TransportError>),
    Tick,
    Reconnect,
}

/// What a commit was for.
enum CommitJob {
    Remove(pingers_proto::LeafIndex),
    Proposals,
    Add(Box<AddMember>),
}

/// When the live channel is re-opened after it dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reconnect {
    /// When the next attempt is due; `None` while connected.
    at: Option<UnixSeconds>,
    /// Attempts since a frame last arrived.
    failures: u32,
}

impl Reconnect {
    /// The wait before attempt `failures`: none for the first, then 1 s doubling to
    /// [`MAX_RECONNECT_DELAY`].
    fn delay(failures: u32) -> Duration {
        let Some(exponent) = failures.checked_sub(2) else {
            return Duration::ZERO;
        };
        2_u64
            .checked_pow(exponent)
            .map(Duration::from_secs)
            .map_or(MAX_RECONNECT_DELAY, |delay| delay.min(MAX_RECONNECT_DELAY))
    }
}

/// What [`start_core`] needs.
struct Started {
    party: PartyId,
    spot: SpotId,
    group: GroupId,
    grant_key: GrantSecretKey,
    rng: SessionRng,
}

/// Records the Meetup in the store and starts its state machine.
fn start_core(local: Local<'_>, started: Started, now: UnixSeconds) -> Result<Core, SessionError> {
    let Started {
        party,
        spot,
        group,
        grant_key,
        rng,
    } = started;
    local.session.meetups().start(&Meetup {
        group,
        party,
        spot,
        started_at: now,
        grant_key: GrantSecretKey::from_bytes(Zeroizing::new(*grant_key.expose_bytes())),
    })?;
    Core::start(
        local,
        CoreSetup {
            group,
            party,
            grant_key,
            rng,
        },
        now,
    )
}

fn spawn(runtime: &Runtime, core: Core, subscription: Option<SubscriptionId>) -> Joined {
    let group = core.group();
    let (commands, receiver) = mpsc::channel(COMMAND_BUFFER);
    let (events, events_receiver) = mpsc::unbounded_channel();
    let (view, view_receiver) = watch::channel(core.view(ChannelState::Reconnecting));
    let driver = Driver {
        runtime: runtime.clone(),
        core: Arc::new(Mutex::new(core)),
        group,
        subscription,
        commands: receiver,
        events,
        view,
        channel: None,
        reconnect: Reconnect::default(),
        last_sequence: None,
        next_deadline: None,
        catch_up: CatchUp::Done,
    };
    let task = tokio::spawn(driver.run());
    Joined {
        handle: MeetupHandle {
            group,
            commands,
            view: view_receiver,
            task: Some(task),
        },
        events: events_receiver,
    }
}

/// Opens a store session and runs `work` in one transaction on the blocking pool.
async fn run_blocking<T: Send + 'static>(
    database: &Arc<Database>,
    work: impl FnOnce(Local<'_>) -> Result<T, SessionError> + Send + 'static,
) -> Result<T, SessionError> {
    let database = Arc::clone(database);
    tokio::task::spawn_blocking(move || {
        let session = database.session()?;
        let identity = session.identity().load()?.ok_or(SessionError::NotFound)?;
        session.transaction(|session| {
            work(Local {
                session,
                signer: &identity.signing_key,
            })
        })
    })
    .await
    .map_err(|_| SessionError::Internal)?
}

/// Whether the group log must be read before the next wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CatchUp {
    /// Read it: the channel (re)opened, or a commit lost a race.
    Due,
    /// Nothing missed.
    Done,
}

/// The task that owns one live Meetup.
struct Driver {
    runtime: Runtime,
    core: Arc<Mutex<Core>>,
    group: GroupId,
    subscription: Option<SubscriptionId>,
    commands: mpsc::Receiver<Command>,
    events: mpsc::UnboundedSender<MeetupEvent>,
    view: watch::Sender<MeetupView>,
    channel: Option<Box<dyn DuplexStream>>,
    reconnect: Reconnect,
    last_sequence: Option<SequenceNumber>,
    next_deadline: Option<UnixSeconds>,
    catch_up: CatchUp,
}

impl Driver {
    async fn run(mut self) -> Result<Ended, SessionError> {
        let result = self.serve().await;
        // Dropping the stream aborts it; a clean close was already sent where it matters.
        self.channel = None;
        result
    }

    async fn serve(&mut self) -> Result<Ended, SessionError> {
        self.connect().await?;
        // The first input drains what `start` prepared (grants) with the first frame.
        if let Some(ended) = self.tick().await? {
            return Ok(ended);
        }
        loop {
            if self.channel.is_none()
                && self
                    .reconnect
                    .at
                    .is_some_and(|at| at <= self.runtime.clock.now())
            {
                self.connect().await?;
            }
            if self.catch_up == CatchUp::Due {
                self.catch_up = CatchUp::Done;
                if let Some(ended) = self.read_log().await? {
                    return Ok(ended);
                }
            }
            let clock = &*self.runtime.clock;
            let timer = &*self.runtime.timer;
            let wake = tokio::select! {
                biased;
                command = self.commands.recv() => Wake::Command(command),
                received = receive(&mut self.channel) => Wake::Received(received),
                () = sleep_until(clock, timer, self.next_deadline) => Wake::Tick,
                () = sleep_until(clock, timer, self.reconnect.at) => Wake::Reconnect,
            };
            let ended = match wake {
                // Every handle is gone: the owner stopped caring; stop without leaving.
                Wake::Command(None) => return Err(SessionError::Ended),
                Wake::Command(Some(command)) => self.command(command).await?,
                Wake::Received(Ok(Some(bytes))) => {
                    self.reconnect.failures = 0;
                    self.received(&bytes).await?
                }
                Wake::Received(Ok(None) | Err(_)) => {
                    self.dropped();
                    None
                }
                Wake::Tick => self.tick().await?,
                // The loop's head re-opens the channel.
                Wake::Reconnect => None,
            };
            if let Some(ended) = ended {
                return Ok(ended);
            }
        }
    }

    async fn command(&mut self, command: Command) -> Result<Option<Ended>, SessionError> {
        match command {
            Command::Input { input, reply } => {
                let now = self.runtime.clock.now();
                let step = self
                    .step(move |core, local| core.input(local, *input, now))
                    .await;
                let (answer, ended) = match step {
                    Ok(effects) => self.carry_out(effects).await?,
                    Err(error) if error.is_fatal() => return Err(error),
                    Err(error) => (Err(error), None),
                };
                let _answered = reply.send(answer);
                Ok(ended)
            }
            Command::Add { newcomer, reply } => {
                let (answer, ended) = match self.commit(CommitJob::Add(newcomer)).await {
                    Ok((effects, welcome)) => {
                        let (_delivered, ended) = self.carry_out(effects).await?;
                        (welcome.ok_or(SessionError::Internal), ended)
                    }
                    Err(error) if error.is_fatal() => return Err(error),
                    Err(error) => (Err(error), None),
                };
                let _answered = reply.send(answer);
                Ok(ended)
            }
            Command::Leave { reply } => {
                let ended = self.leave().await;
                let _answered = reply.send(());
                ended.map(Some)
            }
        }
    }

    async fn tick(&mut self) -> Result<Option<Ended>, SessionError> {
        let now = self.runtime.clock.now();
        let effects = self
            .step(move |core, local| core.input(local, MeetupInput::Tick, now))
            .await?;
        Ok(self.carry_out(effects).await?.1)
    }

    async fn received(&mut self, bytes: &[u8]) -> Result<Option<Ended>, SessionError> {
        let Ok(frame) = LiveFrame::decode(bytes) else {
            return Ok(None);
        };
        self.process(frame.body).await
    }

    /// Processes one MLS message; one that does not open is dropped.
    async fn process(
        &mut self,
        body: pingers_proto::MlsMessageBytes,
    ) -> Result<Option<Ended>, SessionError> {
        let now = self.runtime.clock.now();
        match self
            .step(move |core, local| core.receive(local, &body, now))
            .await
        {
            Ok(effects) => Ok(self.carry_out(effects).await?.1),
            Err(error) if error.is_fatal() => Err(error),
            Err(_dropped) => Ok(None),
        }
    }

    /// Carries out effects and the commits they lead to. Returns the first delivery failure
    /// (for the caller of an action) and whether the Meetup ended.
    async fn carry_out(
        &mut self,
        effects: Effects,
    ) -> Result<(Result<(), SessionError>, Option<Ended>), SessionError> {
        let mut delivered = Ok(());
        let mut queue = VecDeque::from([effects]);
        while let Some(effects) = queue.pop_front() {
            for event in effects.events {
                // Positions are in the view; everything else goes to the shell.
                if !matches!(event, MeetupEvent::PeerLocation(_)) {
                    let _sent = self.events.send(event);
                }
            }
            for outgoing in effects.outgoing {
                if let Err(error) = self.deliver(outgoing).await
                    && delivered.is_ok()
                {
                    delivered = Err(error);
                }
            }
            if let Some(reason) = effects.end {
                return Ok((delivered, Some(self.finish(reason).await?)));
            }
            let mut jobs: Vec<CommitJob> = Vec::new();
            if effects.commit_proposals == Proposals::Waiting {
                jobs.push(CommitJob::Proposals);
            }
            jobs.extend(effects.removals.into_iter().map(CommitJob::Remove));
            for job in jobs {
                match self.commit(job).await {
                    Ok((more, _welcome)) => queue.push_back(more),
                    Err(error) if error.is_fatal() => return Err(error),
                    // Lost the race or the leaf is gone: the winner's commit does the job.
                    Err(_skipped) => {}
                }
            }
        }
        self.refresh();
        Ok((delivered, None))
    }

    /// Sends one frame: on the channel, or through the group log when it must wake absent
    /// members or the channel is down. An ephemeral frame with no channel is dropped.
    async fn deliver(&mut self, outgoing: Outgoing) -> Result<(), SessionError> {
        if outgoing.wake.is_none()
            && let Some(channel) = self.channel.as_mut()
        {
            if channel.send(outgoing.live_bytes()?).await.is_ok() {
                return Ok(());
            }
            self.dropped();
        }
        if !outgoing.is_stored() {
            return Ok(());
        }
        let auth = self.auth()?;
        self.runtime
            .network
            .post_message(
                &self.group,
                &auth,
                AppMessage {
                    epoch: outgoing.frame.epoch,
                    body: outgoing.frame.body,
                    delivery: outgoing.frame.delivery,
                    wake: outgoing.wake,
                },
            )
            .await?;
        Ok(())
    }

    /// Makes, posts and merges (or discards) one commit.
    async fn commit(
        &mut self,
        job: CommitJob,
    ) -> Result<(Effects, Option<WelcomeBytes>), SessionError> {
        let PendingCommit { request, welcome } = self
            .step(move |core, local| match &job {
                CommitJob::Remove(leaf) => core.remove_member(local, *leaf),
                CommitJob::Proposals => core.commit_proposals(local),
                CommitJob::Add(newcomer) => core.add_member(local, newcomer),
            })
            .await?;
        let auth = self.auth()?;
        let response = self
            .runtime
            .network
            .post_commit(&self.group, &auth, request)
            .await;
        match response {
            Ok(CommitResponse::Accepted { .. }) => {
                let now = self.runtime.clock.now();
                let effects = self
                    .step(move |core, local| core.commit_accepted(local, now))
                    .await?;
                Ok((effects, welcome))
            }
            Ok(CommitResponse::Conflict { .. }) => {
                self.step(|core, local| core.commit_rejected(local)).await?;
                self.catch_up = CatchUp::Due;
                Err(SessionError::CommitConflict)
            }
            Err(error) => {
                self.step(|core, local| core.commit_rejected(local)).await?;
                Err(error.into())
            }
        }
    }

    /// Reads the group log since the last read: what was missed while the channel was down,
    /// and the meet point snapshot for a newcomer. Entries of earlier epochs do not open.
    async fn read_log(&mut self) -> Result<Option<Ended>, SessionError> {
        let auth = self.auth()?;
        let Ok(entries) = self
            .runtime
            .network
            .fetch_log(&self.group, &auth, self.last_sequence)
            .await
        else {
            // Tried again when the channel next opens.
            return Ok(None);
        };
        for entry in entries {
            self.last_sequence = Some(entry.sequence);
            if let Some(ended) = self.process(entry.body).await? {
                return Ok(Some(ended));
            }
        }
        Ok(None)
    }

    async fn connect(&mut self) -> Result<(), SessionError> {
        let auth = self.auth()?;
        let opened = match self
            .runtime
            .network
            .live_head(&self.group, &auth, self.subscription)
        {
            Ok(head) => self.runtime.transport.stream(head).await.ok(),
            Err(_) => None,
        };
        match opened {
            Some(channel) => {
                self.channel = Some(channel);
                self.reconnect.at = None;
                self.catch_up = CatchUp::Due;
            }
            None => self.schedule_reconnect(),
        }
        self.refresh();
        Ok(())
    }

    /// The channel ended or failed: re-open it, at once the first time.
    fn dropped(&mut self) {
        self.channel = None;
        self.schedule_reconnect();
        self.refresh();
    }

    fn schedule_reconnect(&mut self) {
        self.reconnect.failures = self.reconnect.failures.saturating_add(1);
        let delay = Reconnect::delay(self.reconnect.failures);
        self.reconnect.at = self.runtime.clock.now().checked_add(delay);
    }

    async fn leave(&mut self) -> Result<Ended, SessionError> {
        if self.lock()?.is_alone() {
            self.delete_on_server().await;
        } else {
            let proposal = self.step(Core::propose_leave).await;
            if let Ok(proposal) = proposal {
                // Best effort: if it is lost, the others remove this device as silent.
                let _sent = self.deliver(proposal).await;
            }
        }
        self.delete_locally().await?;
        Ok(Ended::Left)
    }

    async fn finish(&mut self, reason: EndReason) -> Result<Ended, SessionError> {
        let ended = match reason {
            EndReason::AloneTooLong => {
                self.delete_on_server().await;
                Ended::AloneTooLong
            }
            EndReason::Removed => Ended::Removed,
        };
        self.delete_locally().await?;
        Ok(ended)
    }

    async fn delete_on_server(&mut self) {
        if let Ok(auth) = self.auth() {
            // Best effort: the server expires idle groups after two hours anyway.
            let _deleted = self.runtime.network.delete_group(&self.group, &auth).await;
        }
    }

    /// Closes the channel and deletes the Meetup's rows, chat, keys and MLS state.
    async fn delete_locally(&mut self) -> Result<(), SessionError> {
        if let Some(mut channel) = self.channel.take() {
            let _closed = channel.close().await;
        }
        let database = Arc::clone(&self.runtime.database);
        let group = self.group;
        tokio::task::spawn_blocking(move || {
            // Outside a transaction, so `end` also truncates the write-ahead log.
            database.session()?.meetups().end(&group)?;
            Ok::<(), SessionError>(())
        })
        .await
        .map_err(|_| SessionError::Internal)?
    }

    /// Runs `work` on the core in one store transaction on the blocking pool.
    async fn step<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut Core, Local<'_>) -> Result<T, SessionError> + Send + 'static,
    ) -> Result<T, SessionError> {
        let core = Arc::clone(&self.core);
        run_blocking(&self.runtime.database, move |local| {
            let mut core = core.lock().map_err(|_| SessionError::Internal)?;
            work(&mut core, local)
        })
        .await
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Core>, SessionError> {
        self.core.lock().map_err(|_| SessionError::Internal)
    }

    fn auth(&self) -> Result<pingers_crypto::LogAuthKeyPair, SessionError> {
        Ok(self.lock()?.auth())
    }

    /// Publishes the view and the next deadline after a step.
    fn refresh(&mut self) {
        let channel = if self.channel.is_some() {
            ChannelState::Connected
        } else {
            ChannelState::Reconnecting
        };
        if let Ok(core) = self.core.lock() {
            self.next_deadline = core.next_deadline();
            self.view.send_replace(core.view(channel));
        }
    }
}

/// The next frame from the channel; never completes while there is none.
async fn receive(
    channel: &mut Option<Box<dyn DuplexStream>>,
) -> Result<Option<Vec<u8>>, pingers_transport::TransportError> {
    match channel {
        Some(channel) => channel.receive().await,
        None => core::future::pending().await,
    }
}

/// Sleeps until wall time `at`; never completes for `None`.
async fn sleep_until(clock: &dyn Clock, timer: &dyn Timer, at: Option<UnixSeconds>) {
    match at {
        Some(at) => timer.sleep(at.saturating_duration_since(clock.now())).await,
        None => core::future::pending().await,
    }
}
