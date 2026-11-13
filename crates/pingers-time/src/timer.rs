// SPDX-License-Identifier: AGPL-3.0-only
//! Monotonic time: timeouts, cadences and sleeping.

use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

/// A reading of a monotonic clock, as time elapsed since an origin chosen by the [`Timer`].
///
/// Readings from different timers are not comparable. Monotonic time never jumps, so deadlines
/// built from it survive wall-clock corrections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonotonicTime(Duration);

impl MonotonicTime {
    /// The timer's origin.
    pub const ORIGIN: Self = Self(Duration::ZERO);

    /// A reading `elapsed` after the timer's origin. For [`Timer`] implementations.
    #[must_use]
    pub const fn from_elapsed(elapsed: Duration) -> Self {
        Self(elapsed)
    }

    /// Time elapsed since the timer's origin.
    #[must_use]
    pub const fn elapsed(self) -> Duration {
        self.0
    }

    /// The reading `duration` later, or `None` on overflow.
    #[must_use]
    pub const fn checked_add(self, duration: Duration) -> Option<Self> {
        match self.0.checked_add(duration) {
            Some(elapsed) => Some(Self(elapsed)),
            None => None,
        }
    }

    /// The reading `duration` later, saturating at the largest representable reading.
    #[must_use]
    pub const fn saturating_add(self, duration: Duration) -> Self {
        Self(self.0.saturating_add(duration))
    }

    /// How long after `earlier` this reading is, or zero if it is not after it.
    #[must_use]
    pub const fn saturating_duration_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}

/// A future that completes when a [`Timer`] reaches a deadline.
///
/// Dropping it cancels the sleep, so it is cancellation-safe in `select!`.
pub type Sleep<'timer> = Pin<Box<dyn Future<Output = ()> + Send + 'timer>>;

/// The source of monotonic time: for timeouts, cadences and silence timers.
///
/// Sans-IO crates (L0, L1) only call [`Timer::now`] and compare deadlines; they never sleep.
/// The async crates (L2 and up) sleep through [`Timer::sleep`], so a test can drive them with
/// a manual clock instead of waiting.
pub trait Timer: Send + Sync {
    /// The current monotonic reading.
    fn now(&self) -> MonotonicTime;

    /// A future that completes once [`Timer::now`] has reached `deadline`. Completes
    /// immediately if it already has.
    fn sleep_until(&self, deadline: MonotonicTime) -> Sleep<'_>;

    /// A future that completes once `duration` has elapsed on this timer.
    fn sleep(&self, duration: Duration) -> Sleep<'_> {
        self.sleep_until(self.now().saturating_add(duration))
    }
}

impl<T: Timer + ?Sized> Timer for &T {
    fn now(&self) -> MonotonicTime {
        (**self).now()
    }

    fn sleep_until(&self, deadline: MonotonicTime) -> Sleep<'_> {
        (**self).sleep_until(deadline)
    }
}

impl<T: Timer + ?Sized> Timer for std::sync::Arc<T> {
    fn now(&self) -> MonotonicTime {
        (**self).now()
    }

    fn sleep_until(&self, deadline: MonotonicTime) -> Sleep<'_> {
        (**self).sleep_until(deadline)
    }
}
