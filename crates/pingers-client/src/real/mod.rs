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
//! - `sync`: pingers-sync has not merged. [`SyncPort`](crate::ports::SyncPort), pingers-wake's
//!   `ServerPort` and pingers-meetup-session's `GroupNetwork` are explicit `Unavailable`
//!   stubs, and the `Outbound` values pingers-parties returns wait in memory for it.
//! - `records`: the client's own records in the store's key-value table.

mod meetups;
mod network;
mod parties;
mod records;
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

pub use network::Instance;

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
        }
    }
}

impl Crates {
    /// The four ports over the real crates, on the opened `database`. pingers-sync's parts
    /// answer [`Unavailable::Sync`](crate::Unavailable::Sync) until it merges.
    ///
    /// # Errors
    ///
    /// [`ClientError::Transport`] if the instance's relay or server URL is not usable.
    pub fn real(
        database: Database,
        platform: Arc<dyn Platform>,
        outside: Outside,
        instance: Option<&Instance>,
    ) -> Result<Self, ClientError> {
        let transport = instance
            .map(|instance| network::router(instance, Arc::clone(&outside.timer), &outside))
            .transpose()?;
        let backend = Arc::new(Backend {
            database: Arc::new(database),
            outside,
            transport,
        });
        let outbox = Arc::new(sync::Outbox::default());
        let meetups = Arc::new(meetups::Meetups::new(Arc::clone(&backend)));
        Ok(Self {
            parties: Arc::new(parties::Parties::new(Arc::clone(&backend), outbox)),
            sync: Arc::new(sync::Sync),
            wake: Arc::new(wake::Wake::new(
                Arc::clone(&backend),
                platform,
                Arc::clone(&meetups),
            )),
            meetups,
        })
    }
}

/// The opened store and the outside world, shared by the adapters.
pub(crate) struct Backend {
    database: Arc<Database>,
    outside: Outside,
    /// The router over the configured instance, for pingers-sync's engine.
    #[expect(
        dead_code,
        reason = "WAITING(pingers-sync): its engine holds one store Session for its lifetime, \
                  locking out every other store step; it is built once it holds the Database"
    )]
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
