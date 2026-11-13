// SPDX-License-Identifier: AGPL-3.0-only
//! Wall-clock time.

use std::time::SystemTime;

use crate::{TimeError, UnixSeconds};

/// The source of wall-clock time: for buckets, expiries and anything compared with the server.
///
/// Wall time can jump (the user changes it, or the core corrects it by `server_time`), so use
/// a [`crate::Timer`] for timeouts and cadences instead.
pub trait Clock: Send + Sync {
    /// The current wall-clock time.
    fn now(&self) -> UnixSeconds;
}

impl<C: Clock + ?Sized> Clock for &C {
    fn now(&self) -> UnixSeconds {
        (**self).now()
    }
}

impl<C: Clock + ?Sized> Clock for std::sync::Arc<C> {
    fn now(&self) -> UnixSeconds {
        (**self).now()
    }
}

/// The device's or server's real wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl SystemClock {
    /// Reads the system clock, reporting a clock set before 1970 instead of hiding it.
    ///
    /// # Errors
    ///
    /// [`TimeError::BeforeEpoch`] if the system clock reads earlier than the Unix epoch.
    pub fn try_now(&self) -> Result<UnixSeconds, TimeError> {
        #[expect(
            clippy::disallowed_methods,
            reason = "SystemClock is the one production reader of the system wall clock"
        )]
        let now = SystemTime::now();
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map(|since_epoch| UnixSeconds::new(since_epoch.as_secs()))
            .map_err(|_before_epoch| TimeError::BeforeEpoch)
    }
}

impl Clock for SystemClock {
    /// The current time; a clock set before 1970 reads as the epoch, which every expiry check
    /// treats as "long ago" and the skew check in the core reports.
    fn now(&self) -> UnixSeconds {
        self.try_now().unwrap_or(UnixSeconds::EPOCH)
    }
}
