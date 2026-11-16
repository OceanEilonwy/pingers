// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]: the one async API. Every method calls one crate, then republishes the snapshot.
//!
//! This file opens the client and publishes snapshots; the commands are grouped by the crate
//! they reach: [`parties`] (pingers-parties), [`settings`] (pingers-wake and pingers-sync),
//! [`consent`] (location consent), [`meetups`] (pingers-meetup-session) and [`pipeline`] (the
//! shell's inputs to pingers-wake).

mod consent;
mod meetups;
mod parties;
mod pipeline;
mod settings;

use std::path::PathBuf;
use std::sync::Arc;

use pingers_store::{Database, DatabaseKey};
use tokio::sync::watch;
use zeroize::Zeroizing;

use crate::forward::{Forwarder, Forwarding, forward};
use crate::ports::Crates;
use crate::snapshot::ClientSnapshot;
use crate::{ClientError, Events, Instance, Outside, Platform, SecureStoreKey};

mod maps;

/// Where the client keeps its data.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// The SQLCipher database file.
    pub database: PathBuf,
    /// The instance to talk to, or `None` to stay offline.
    pub instance: Option<Instance>,
}

/// The Pingers client: the L3 crates wired to one [`Platform`] and one [`Events`] sink.
pub struct Client {
    platform: Arc<dyn Platform>,
    events: Arc<dyn Events>,
    crates: Crates,
    snapshot: watch::Sender<Arc<ClientSnapshot>>,
    /// Passes on what the live Meetups raise on their own.
    _meetup_events: Option<Forwarder>,
}

impl Client {
    /// Opens the store with the database key from the platform's secure store, builds the
    /// real crates over it ([`Crates::real`] with [`Outside::system`]), and publishes the
    /// first snapshot.
    ///
    /// # Errors
    ///
    /// As [`Client::open_with`].
    pub async fn open(
        config: ClientConfig,
        platform: Arc<dyn Platform>,
        events: Arc<dyn Events>,
    ) -> Result<Self, ClientError> {
        let crates_platform = Arc::clone(&platform);
        let instance = config.instance.clone();
        Self::open_with(config, platform, events, move |database| {
            Crates::real(
                database,
                crates_platform,
                Outside::system(),
                instance.as_ref(),
            )
        })
        .await
    }

    /// Opens the store as [`Client::open`] does, but builds the crates over it with `build`
    /// (tests pass testkit's `FakeCrates`, or [`Crates::real`] with a manual clock).
    ///
    /// # Errors
    ///
    /// [`ClientError::Platform`], [`ClientError::NoDatabaseKey`] or
    /// [`ClientError::MalformedDatabaseKey`] for the key; [`ClientError::Store`] if the
    /// database does not open; whatever `build` or a crate's snapshot section returns.
    pub async fn open_with(
        config: ClientConfig,
        platform: Arc<dyn Platform>,
        events: Arc<dyn Events>,
        build: impl FnOnce(Database) -> Result<Crates, ClientError>,
    ) -> Result<Self, ClientError> {
        let key = database_key(platform.as_ref())?;
        let database = tokio::task::spawn_blocking(move || Database::open(&config.database, key))
            .await
            .map_err(|_panicked| ClientError::Internal)??;
        let crates = build(database)?;
        let snapshot = watch::channel(Arc::new(assemble(&crates).await?)).0;
        let meetup_events = crates.meetups.take_events().map(|incoming| {
            forward(Forwarding {
                incoming,
                crates: crates.clone(),
                snapshot: snapshot.clone(),
                events: Arc::clone(&events),
            })
        });
        Ok(Self {
            platform,
            events,
            crates,
            snapshot,
            _meetup_events: meetup_events,
        })
    }

    /// The latest published snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Arc<ClientSnapshot> {
        self.snapshot.borrow().clone()
    }

    /// A receiver that sees every snapshot published from now on.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Arc<ClientSnapshot>> {
        self.snapshot.subscribe()
    }

    /// Reads every crate's section and publishes a new snapshot.
    ///
    /// # Errors
    ///
    /// Whatever a crate's section returns; the previous snapshot stays published.
    pub async fn publish_snapshot(&self) -> Result<Arc<ClientSnapshot>, ClientError> {
        let snapshot = Arc::new(assemble(&self.crates).await?);
        self.snapshot.send_replace(snapshot.clone());
        Ok(snapshot)
    }

    /// Republishes after a command that succeeded.
    async fn published<T>(&self, result: Result<T, ClientError>) -> Result<T, ClientError> {
        let value = result?;
        self.publish_snapshot().await?;
        Ok(value)
    }
}

/// The SQLCipher key from the secure store.
fn database_key(platform: &dyn Platform) -> Result<DatabaseKey, ClientError> {
    let stored = platform
        .secure_store_get(&SecureStoreKey::database())?
        .ok_or(ClientError::NoDatabaseKey)?;
    let mut bytes = Zeroizing::new([0_u8; DatabaseKey::LENGTH]);
    if stored.len() != bytes.len() {
        return Err(ClientError::MalformedDatabaseKey);
    }
    bytes.copy_from_slice(&stored);
    Ok(DatabaseKey::from_bytes(bytes))
}

/// One snapshot from the four crates' sections.
pub(crate) async fn assemble(crates: &Crates) -> Result<ClientSnapshot, ClientError> {
    let parties = crates.parties.section().await?;
    let pipeline = crates.wake.section().await?;
    let server = crates.sync.section().await?;
    let meetups = crates.meetups.section().await?;
    let mut party_list = parties.parties;
    // Every Party is on the instance's server until Parties can choose their server.
    if let Some(operators) = &server.operators {
        for party in &mut party_list {
            party.config.declare(operators);
        }
    }
    Ok(ClientSnapshot {
        settings: pipeline.settings,
        gate: pipeline.gate,
        parties: party_list,
        zones: pipeline.zones,
        meetups: meetups.meetups,
        alert: meetups.alert,
        location_requests: meetups.location_requests,
        ended_meetup: meetups.ended_meetup,
        joining: parties.joining,
        display_name: parties.display_name,
        own_position: pipeline.own_position,
        fallbacks: server.fallbacks,
        relay: server.relay,
        map_host: server.map_host,
        operators: server.operators,
    })
}
