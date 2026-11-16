// SPDX-License-Identifier: AGPL-3.0-only
//! The servers this device has Parties on: one [`Link`] per server, in a registry keyed by
//! [`ServerId`] ([`Servers`]).
//!
//! A device can be in Parties on different servers at once (decided 2026-11-14). Each server
//! has its own pingers-sync engine over its own router (its OHTTP relays, its gateway pins, its
//! direct route), and the engines share the one store: every engine reads and writes only its
//! server's rows (pingers-store's `ServerSession`). Device settings stay device-wide.
//!
//! Servers come from four places, and none is pinned on first use:
//!
//! - the app's built-in server ([`Instance`]), whose pins come from the build and are written
//!   to its row at every start;
//! - an invite link, which carries the pins of the inviter, who already trusted the server;
//! - the user, who looked at a new server's keys and chose to trust them (`probe.rs`);
//! - a first-phone setup link (`setup.rs`), which carries the appliance's pins as an invite does.
//!
//! A server whose row has no gateway pins (a build without `PINGERS_GATEWAY_KEY_PINS`) gets no
//! link: the client stays offline for it rather than trusting the first key it sees. Such a
//! build's server gets its pins from a setup link or an invite, and keeps them across starts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pingers_ohttp::KeyPins;
use pingers_proto::{SecretText, ServerId};
use pingers_store::{ServerRecord, ServerTrust};
use pingers_sync::{CodeSource, SyncEngine, SyncError, WakeBudget};
use pingers_transport::{KeyPin, Transport};
use tokio::sync::RwLock;

use super::network::{self, Instance, Trust};
use super::push::{Learned, PushRelaySetup};
use super::{Backend, Outside};
use crate::snapshot::{Host, KeyTrust, Operator, PushRelayKind, RelayStatus};
use crate::{ClientError, Notice, Platform, Unavailable};

/// One server: its engine, what it said about itself, and how reaching it went.
pub(crate) struct Link {
    pub(crate) server: ServerId,
    pub(crate) engine: Arc<SyncEngine>,
    /// The router to the server, for the live channels of its Meetups.
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) record: ServerRecord,
    relay: Mutex<RelayStatus>,
    clock_skew: AtomicBool,
    key_mismatch: AtomicBool,
    /// The server's parameters and the push relay's key, once fetched (`push.rs`).
    pub(crate) learned: Learned,
    /// The build's registration code, for the built-in server's first registration.
    registration_code: Option<SecretText>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Link {
    /// What a network result says about this server's relay and the clock.
    pub(crate) fn observe<T>(&self, result: &Result<T, SyncError>) {
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
        *lock(&self.relay) = status;
    }

    /// Whether anonymous traffic reached this server lately.
    pub(crate) fn relay_status(&self) -> RelayStatus {
        *lock(&self.relay)
    }

    pub(crate) fn set_relay_status(&self, status: RelayStatus) {
        *lock(&self.relay) = status;
    }

    /// Notices raised since the last call.
    fn take_notices(&self, notices: &mut Vec<Notice>) {
        if self.clock_skew.swap(false, Ordering::Relaxed) && !notices.contains(&Notice::ClockSkew) {
            notices.push(Notice::ClockSkew);
        }
        if self.key_mismatch.swap(false, Ordering::Relaxed)
            && !notices.contains(&Notice::RelayKeyMismatch)
        {
            notices.push(Notice::RelayKeyMismatch);
        }
    }

    /// pingers-sync's `prepare_credentials` on this server, with the build's code on offer and
    /// the server's parameters read: the account registered, the token stock, the membership
    /// tree and leaf, and the prover self-test started. Only a store failure is an
    /// error; a network failure or a refusal leaves credentialed requests waiting for a later
    /// call (each command's flush and each wake).
    ///
    /// # Errors
    ///
    /// [`ClientError::Sync`] with a store failure.
    pub(crate) async fn prepare_credentials(&self) -> Result<(), ClientError> {
        match self.account().await {
            Err(ClientError::Sync(SyncError::Store(error))) => Err(SyncError::Store(error).into()),
            Err(error @ (ClientError::Store(_) | ClientError::Records(_))) => Err(error),
            Ok(()) | Err(_) => Ok(()),
        }
    }

    /// The account on this server, made now if it is not yet: for a new Party or a join, which
    /// must say why the server will not take this device. A network failure is not an error
    /// here either: the Party is made, and its requests wait.
    ///
    /// # Errors
    ///
    /// [`ClientError::RegistrationCodeRequired`] if the server needs a registration code;
    /// [`ClientError::RegistrationCodeRefused`] if it refused the one offered;
    /// [`ClientError::Sync`] with a store failure.
    pub(crate) async fn account(&self) -> Result<(), ClientError> {
        if let Some(code) = &self.registration_code {
            self.engine
                .offer_registration_code(code.clone(), CodeSource::Build)
                .await?;
        }
        // The server's parameters first (once per process): they say which credentials it
        // accepts and its daily token limit, which the steps below size themselves by. A
        // network failure here leaves the defaults; only a store failure is an error.
        if let Err(error @ (ClientError::Store(_) | ClientError::Records(_))) =
            self.server_params().await
        {
            return Err(error);
        }
        let result = self.engine.prepare_credentials(WakeBudget::UNLIMITED).await;
        self.observe(&result);
        match result {
            Err(SyncError::RegistrationCodeRequired) => Err(ClientError::RegistrationCodeRequired),
            Err(SyncError::RegistrationCodeRefused) => Err(ClientError::RegistrationCodeRefused),
            Err(SyncError::Store(error)) => Err(SyncError::Store(error).into()),
            Ok(_) | Err(_) => Ok(()),
        }
    }

    /// Offers a registration code (from an invite link or typed by the user) and makes the
    /// account with it.
    ///
    /// # Errors
    ///
    /// As [`Link::account`].
    pub(crate) async fn offer_code(
        &self,
        code: SecretText,
        source: CodeSource,
    ) -> Result<(), ClientError> {
        self.engine.offer_registration_code(code, source).await?;
        self.account().await
    }
}

/// The registry: every server this device knows, loaded from the store on first use.
pub(crate) struct Servers {
    backend: Arc<Backend>,
    built_in: Option<Instance>,
    /// Which certificate authorities HTTPS trusts, for every server: the build's choice.
    trust: Trust,
    platform: Option<Arc<dyn Platform>>,
    links: RwLock<Option<Vec<Arc<Link>>>>,
    /// A server an invite named served keys its pins do not allow.
    key_mismatch: AtomicBool,
}

impl Servers {
    pub(crate) fn new(
        backend: Arc<Backend>,
        built_in: Option<Instance>,
        platform: Option<Arc<dyn Platform>>,
    ) -> Self {
        let trust = built_in
            .as_ref()
            .map_or(Trust::Mozilla, |instance| instance.trust.clone());
        Self {
            backend,
            built_in,
            trust,
            platform,
            links: RwLock::new(None),
            key_mismatch: AtomicBool::new(false),
        }
    }

    pub(crate) fn backend(&self) -> &Backend {
        &self.backend
    }

    /// Raises `Notice::RelayKeyMismatch` for a server that has no link (yet).
    pub(crate) fn raise_key_mismatch(&self) {
        self.key_mismatch.store(true, Ordering::Relaxed);
    }

    pub(crate) const fn trust(&self) -> &Trust {
        &self.trust
    }

    pub(crate) fn outside(&self) -> &Outside {
        &self.backend.outside
    }

    /// The app's built-in server, if the build names one.
    pub(crate) fn built_in(&self) -> Option<&ServerId> {
        self.built_in.as_ref().map(|instance| &instance.server)
    }

    /// The build's push relay and pins: the publisher's relay, whichever server lists it.
    pub(crate) fn build_push_relay(&self) -> Option<PushRelaySetup> {
        let instance = self.built_in.as_ref()?;
        Some(PushRelaySetup {
            origin: instance.push_relay.clone()?,
            pins: instance.push_relay_pins.clone(),
        })
    }

    /// Every link, loading them first if this is the first use.
    ///
    /// # Errors
    ///
    /// A store failure; [`ClientError::Transport`] if the built-in server's relay or origin
    /// does not make a transport.
    pub(crate) async fn all(&self) -> Result<Vec<Arc<Link>>, ClientError> {
        self.load().await?;
        Ok(self
            .links
            .read()
            .await
            .as_ref()
            .cloned()
            .unwrap_or_default())
    }

    /// The link to `server`.
    ///
    /// # Errors
    ///
    /// [`Unavailable::UnknownServer`] if this device does not know it (or knows it without
    /// pins); as [`Servers::all`].
    pub(crate) async fn link(&self, server: &ServerId) -> Result<Arc<Link>, ClientError> {
        self.load().await?;
        self.links
            .read()
            .await
            .as_ref()
            .and_then(|links| links.iter().find(|link| link.server == *server).cloned())
            .ok_or_else(|| Unavailable::UnknownServer.into())
    }

    /// The link to the built-in server.
    ///
    /// # Errors
    ///
    /// [`Unavailable::Sync`] for a build without one (or without its pins); as [`Servers::all`].
    pub(crate) async fn built_in_link(&self) -> Result<Arc<Link>, ClientError> {
        let server = self.built_in().ok_or(Unavailable::Sync)?.clone();
        self.link(&server)
            .await
            .map_err(|_unknown| Unavailable::Sync.into())
    }

    /// The link to `server` without loading: for synchronous ports, after a wake loaded them.
    pub(crate) fn loaded(&self, server: &ServerId) -> Option<Arc<Link>> {
        self.links
            .try_read()
            .ok()?
            .as_ref()?
            .iter()
            .find(|link| link.server == *server)
            .cloned()
    }

    /// Records `record` and builds its link, replacing any link to the same server.
    ///
    /// # Errors
    ///
    /// A store failure; [`ClientError::Transport`]; [`Unavailable::UnknownServer`] if it has no
    /// gateway pins.
    pub(crate) async fn add(&self, record: ServerRecord) -> Result<Arc<Link>, ClientError> {
        self.load().await?;
        let stored = record.clone();
        self.backend
            .store(move |session, _clock, _rng| Ok(session.servers().put(&stored)?))
            .await?;
        let link = self.build(record)?.ok_or(Unavailable::UnknownServer)?;
        let link = Arc::new(link);
        if let Some(links) = self.links.write().await.as_mut() {
            links.retain(|known| known.server != link.server);
            links.push(Arc::clone(&link));
        }
        Ok(link)
    }

    /// Notices raised on any server since the last call (`Notice::ClockSkew`,
    /// `Notice::RelayKeyMismatch`).
    pub(crate) fn take_notices(&self) -> Vec<Notice> {
        let mut notices = Vec::new();
        if self.key_mismatch.swap(false, Ordering::Relaxed) {
            notices.push(Notice::RelayKeyMismatch);
        }
        if let Ok(links) = self.links.try_read()
            && let Some(links) = links.as_ref()
        {
            for link in links {
                link.take_notices(&mut notices);
            }
        }
        notices
    }

    async fn load(&self) -> Result<(), ClientError> {
        if self.links.read().await.is_some() {
            return Ok(());
        }
        let mut guard = self.links.write().await;
        if guard.is_some() {
            return Ok(());
        }
        let built_in = self.built_in.as_ref().map(Instance::record);
        let records = self
            .backend
            .store(move |session, _clock, _rng| {
                if let Some(record) = &built_in {
                    // A build without gateway pins keeps the pins a setup link or an invite
                    // recorded for its server; otherwise the build's pins are written again.
                    let keep = record.gateway_pins.is_empty()
                        && session
                            .servers()
                            .get(&record.server)?
                            .is_some_and(|stored| !stored.gateway_pins.is_empty());
                    if !keep {
                        // Likewise a build without map host pins keeps those a setup link or
                        // an invite recorded.
                        let mut record = record.clone();
                        if record.map_host_pins.is_empty()
                            && let Some(stored) = session.servers().get(&record.server)?
                        {
                            record.map_host_pins = stored.map_host_pins;
                        }
                        session.servers().put(&record)?;
                    }
                }
                Ok(session.servers().list()?)
            })
            .await?;
        let mut links = Vec::new();
        for record in records {
            if let Some(link) = self.build(record)? {
                links.push(Arc::new(link));
            }
        }
        *guard = Some(links);
        Ok(())
    }

    /// A link over `record`, or `None` if it has no gateway pins.
    fn build(&self, record: ServerRecord) -> Result<Option<Link>, ClientError> {
        let Ok(pins) = KeyPins::new(record.gateway_pins.iter().copied()) else {
            return Ok(None);
        };
        let outside = self.outside();
        let route = network::Route {
            server: &record.server,
            relays: &record.ohttp_relays,
            pin: KeyPin::Pinned(pins),
        };
        let transport = network::router(route, &self.trust, outside)?;
        let engine = network::engine(&record.server, Arc::clone(&transport), &self.backend)?;
        let built_in = self
            .built_in
            .as_ref()
            .filter(|instance| instance.server == record.server);
        let push_relay = record.push_relay.clone().map(|origin| PushRelaySetup {
            pins: self.push_relay_pins(&origin, &record),
            origin,
        });
        Ok(Some(Link {
            server: record.server.clone(),
            engine,
            transport,
            relay: Mutex::new(RelayStatus::Unreachable),
            clock_skew: AtomicBool::new(false),
            key_mismatch: AtomicBool::new(false),
            learned: Learned::new(push_relay, self.platform.clone()),
            registration_code: built_in.and_then(|instance| instance.registration_code.clone()),
            record,
        }))
    }

    /// The pins of push relay `origin` for `record`'s server: the build's when it is the
    /// build's own push relay (the same service whichever server lists it), else the server's;
    /// for a build that pins none, those an invite or a setup link recorded for the server.
    fn push_relay_pins(&self, origin: &str, record: &ServerRecord) -> Option<KeyPins> {
        if let Some(build) = self.build_push_relay()
            && super::push::same_origin(&build.origin, origin)
            && build.pins.is_some()
        {
            return build.pins;
        }
        KeyPins::new(record.push_relay_pins.iter().copied()).ok()
    }
}

/// What a Party's settings page shows about its server.
#[derive(Clone, Debug)]
pub(crate) struct ServerView {
    pub(crate) server_operator: Option<Operator>,
    pub(crate) relay_host: Option<Host>,
    pub(crate) relay_status: RelayStatus,
    pub(crate) push_relay_kind: PushRelayKind,
    pub(crate) push_relay_operator: Option<Operator>,
    pub(crate) push_relay_host: Option<Host>,
    pub(crate) keys: KeyTrust,
}

/// A URL's host (and port), as screens name a server: "relay.quietpath.net".
pub(crate) fn host_of(url: &str) -> Option<Host> {
    let rest = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
    Host::new(rest.split('/').next().unwrap_or(rest)).ok()
}

impl Link {
    /// What this link knows, for the Party settings page. `build_push` is the build's push
    /// relay: the official one.
    pub(crate) fn view(&self, build_push: Option<&PushRelaySetup>) -> ServerView {
        let operators = self.operators();
        let push_origin = self.learned.push_relay_origin();
        let official = match (push_origin, build_push) {
            (Some(origin), Some(build)) => super::push::same_origin(origin, &build.origin),
            (None, _) => true,
            (Some(_), None) => false,
        };
        ServerView {
            server_operator: operators.as_ref().map(|known| known.server.clone()),
            relay_host: self
                .record
                .ohttp_relays
                .first()
                .and_then(|relay| host_of(relay)),
            relay_status: self.relay_status(),
            push_relay_kind: match push_origin.and_then(host_of) {
                Some(host) if !official => PushRelayKind::Custom(host),
                _ => PushRelayKind::Official,
            },
            push_relay_operator: operators.and_then(|known| known.push_relay),
            push_relay_host: push_origin.and_then(host_of),
            keys: trust_view(self.record.trust),
        }
    }
}

/// How a server's keys were trusted, as the snapshot shows it.
pub(crate) const fn trust_view(trust: ServerTrust) -> KeyTrust {
    match trust {
        ServerTrust::BuiltIn => KeyTrust::BuiltIn,
        ServerTrust::Invite => KeyTrust::FromInvite,
        ServerTrust::Confirmed => KeyTrust::Confirmed,
    }
}
