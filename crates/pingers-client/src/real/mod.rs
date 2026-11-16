// SPDX-License-Identifier: AGPL-3.0-only
//! The ports over the real L3 crates, and [`Crates::real`], which builds them.
//!
//! The adapters live here because the L3 crates cannot depend upward. Each one only converts
//! types and calls its crate:
//!
//! - `parties`: [`PartiesPort`](crate::ports::PartiesPort) over pingers-parties'
//!   `PartyLifecycle`.
//! - `wake`: [`WakePort`](crate::ports::WakePort) over pingers-wake's `WakeState`, with
//!   `wake_ports` implementing pingers-wake's own `DevicePort` (the shell) and `PartyView`
//!   (pingers-parties), and `wake_codec` storing its `WakeSnapshot`.
//! - `meetups`: [`MeetupsPort`](crate::ports::MeetupsPort) and pingers-wake's `MeetupPort`
//!   over pingers-meetup-session's handles.
//! - `sync`: [`SyncPort`](crate::ports::SyncPort), pingers-wake's `ServerPort` and
//!   pingers-meetup-session's `GroupNetwork` over pingers-sync's engine, and the outbox that
//!   sends pingers-parties' `Outbound` values; `party_log` is a Party's log for the engine.
//! - `network`: the instance settings, and the router and engine built from them.
//! - `records`: the client's own records in the store's key-value table.

mod join;
mod maps;
mod meetups;
mod network;
mod parties;
mod party_log;
mod push;
mod records;
mod registrations;
mod setup;
mod sync;
mod wake;
mod wake_codec;
mod wake_ports;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use getrandom::SysRng;
use pingers_meetup_session::SessionRng;
use pingers_store::{Database, Session};
use pingers_time::{Clock, SystemClock, Timer};
use pingers_transport::TokioTimer;
use rand_core::UnwrapErr;

pub use network::{Instance, Trust};

use crate::ports::Crates;
use crate::{ClientError, Platform};

/// What the real crates take from outside the core: time and randomness. [`Outside::system`]
/// in production; tests pass a manual clock and a seeded RNG.
#[derive(Clone)]
pub struct Outside {
    /// Wall time.
    pub clock: Arc<dyn Clock>,
    /// Monotonic time, for the wake budget and Meetup timers.
    pub timer: Arc<dyn Timer>,
    /// Makes a random generator for one operation.
    pub rng: Arc<dyn Fn() -> SessionRng + Send + Sync>,
    /// The prover self-test run before this device proves (design B.4.8): the design's 20 timed
    /// proofs in production. Tests on a manual timer measure nothing and pass a one-proof plan.
    pub prover_self_test: pingers_sync::SelfTestPlan,
}

impl Outside {
    /// The system clock, tokio's timer and the operating system's random generator. Must be
    /// called inside a tokio runtime.
    #[must_use]
    pub fn system() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            timer: Arc::new(TokioTimer::new()),
            rng: Arc::new(|| Box::new(UnwrapErr(SysRng))),
            prover_self_test: pingers_sync::SelfTestPlan::DESIGN,
        }
    }
}

impl Crates {
    /// The four ports over the real crates, on the opened `database`, talking to `instance`
    /// through pingers-sync. Without an instance the client stays offline: network calls
    /// answer [`Unavailable::Sync`](crate::Unavailable::Sync).
    ///
    /// # Errors
    ///
    /// [`ClientError::Transport`] if the instance's relay or server URL is not usable;
    /// [`ClientError::Store`] or [`ClientError::Parties`] if pingers-parties' old records
    /// cannot be moved into the store's tables.
    pub fn real(
        database: Database,
        platform: Arc<dyn Platform>,
        outside: Outside,
        instance: Option<&Instance>,
    ) -> Result<Self, ClientError> {
        // Per-Party state an older build kept as records moves into the store's tables first.
        {
            let session = database.session()?;
            pingers_parties::PartyLifecycle::new(&session, outside.clock.as_ref())
                .upgrade_records()?;
        }
        let transport = instance
            .map(|instance| network::router(instance, Arc::clone(&outside.timer), &outside))
            .transpose()?;
        let backend = Arc::new(Backend {
            database: Arc::new(database),
            outside,
            transport,
        });
        let engine = match (instance, &backend.transport) {
            (Some(instance), Some(transport)) => Some(network::engine(
                instance,
                Arc::clone(transport),
                &backend.database,
                &backend.outside,
            )?),
            _ => None,
        };
        let learned = push::Learned::new(
            instance.and_then(|instance| {
                instance
                    .push_relay
                    .clone()
                    .map(|origin| push::PushRelaySetup {
                        origin,
                        pins: instance.push_relay_pins.clone(),
                    })
            }),
            Some(Arc::clone(&platform)),
        );
        let online = engine.map(|engine| sync::Online {
            engine,
            registration_code: instance.and_then(|instance| instance.registration_code.clone()),
            gateway_pins: instance.and_then(|instance| instance.gateway_pins.clone()),
        });
        let net = Arc::new(sync::Net::new(Arc::clone(&backend), online, learned));
        let meetups = Arc::new(meetups::Meetups::new(
            Arc::clone(&backend),
            Arc::clone(&net),
        ));
        let roots = instance.map_or(Trust::Mozilla, |instance| instance.trust.clone());
        let maps = Arc::new(maps::Maps::new(
            Arc::clone(&backend),
            Arc::clone(&net),
            roots.roots()?,
        )?);
        Ok(Self {
            parties: Arc::new(parties::Parties::new(
                Arc::clone(&backend),
                Arc::clone(&net),
            )),
            sync: Arc::new(sync::Sync(Arc::clone(&net))),
            wake: Arc::new(wake::Wake::new(
                Arc::clone(&backend),
                platform,
                Arc::clone(&meetups),
                net,
            )),
            meetups,
            maps,
        })
    }
}

/// The opened store and the outside world, shared by the adapters.
pub(crate) struct Backend {
    database: Arc<Database>,
    outside: Outside,
    /// The router over the configured instance, for pingers-sync's engine and the live
    /// channels of Meetups.
    transport: Option<Arc<dyn pingers_transport::Transport>>,
}

impl Backend {
    /// Runs `job` on the blocking pool with a fresh store session (the cross-process lock is
    /// held only for the job), the clock and a random generator.
    async fn store<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Session, &dyn Clock, &mut SessionRng) -> Result<T, ClientError>
        + Send
        + 'static,
    ) -> Result<T, ClientError> {
        let database = Arc::clone(&self.database);
        let clock = Arc::clone(&self.outside.clock);
        let mut rng = (self.outside.rng)();
        tokio::task::spawn_blocking(move || {
            let session = database.session()?;
            job(&session, clock.as_ref(), &mut rng)
        })
        .await
        .map_err(|_panicked| ClientError::Internal)?
    }
}
