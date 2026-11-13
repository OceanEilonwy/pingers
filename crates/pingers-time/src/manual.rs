// SPDX-License-Identifier: AGPL-3.0-only
//! A clock that only moves when a test moves it.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::{Clock, MonotonicTime, Sleep, Timer, UnixSeconds};

/// A [`Clock`] and [`Timer`] for tests: time stands still until [`ManualClock::advance`].
///
/// Clones share one clock, so a test can keep a handle and give another to the code under test.
/// Advancing wakes every [`Timer::sleep`] whose deadline has been reached.
#[derive(Clone, Debug)]
pub struct ManualClock {
    state: Arc<Mutex<State>>,
}

#[derive(Debug)]
struct State {
    /// Wall time since the epoch, kept with sub-second precision so small advances add up.
    wall: Duration,
    monotonic: MonotonicTime,
    sleepers: Vec<Sleeper>,
}

#[derive(Debug)]
struct Sleeper {
    deadline: MonotonicTime,
    waker: Waker,
}

impl ManualClock {
    /// A clock reading `start` on the wall and [`MonotonicTime::ORIGIN`] on the timer.
    #[must_use]
    pub fn new(start: UnixSeconds) -> Self {
        let state = State {
            wall: Duration::from_secs(start.get()),
            monotonic: MonotonicTime::ORIGIN,
            sleepers: Vec::new(),
        };
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Moves both wall and monotonic time forward by `duration` and wakes due sleepers.
    pub fn advance(&self, duration: Duration) {
        let due = {
            let mut state = self.lock();
            state.wall = state.wall.saturating_add(duration);
            state.monotonic = state.monotonic.saturating_add(duration);
            let now = state.monotonic;
            let (due, waiting): (Vec<Sleeper>, Vec<Sleeper>) = state
                .sleepers
                .drain(..)
                .partition(|sleeper| sleeper.deadline <= now);
            state.sleepers = waiting;
            due
        };
        for sleeper in due {
            sleeper.waker.wake();
        }
    }

    /// Sets the wall clock to `instant` without moving monotonic time, as when the user or a
    /// server-time correction changes the device clock.
    pub fn set_wall(&self, instant: UnixSeconds) {
        self.lock().wall = Duration::from_secs(instant.get());
    }

    /// Whether some task is parked on a [`Timer::sleep_until`] for exactly `deadline`.
    ///
    /// Lets a test wait until the code under test has reached a timer before moving time, so
    /// an [`advance`](Self::advance) cannot race ahead of the deadline being computed. A sleep
    /// dropped before its deadline stays registered until an advance passes it, so pick a
    /// deadline only the awaited timer uses.
    #[must_use]
    pub fn is_sleeping_until(&self, deadline: MonotonicTime) -> bool {
        self.lock()
            .sleepers
            .iter()
            .any(|sleeper| sleeper.deadline == deadline)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A test that panicked while holding the lock has already failed; keep the state usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for ManualClock {
    fn now(&self) -> UnixSeconds {
        UnixSeconds::new(self.lock().wall.as_secs())
    }
}

impl Timer for ManualClock {
    fn now(&self) -> MonotonicTime {
        self.lock().monotonic
    }

    fn sleep_until(&self, deadline: MonotonicTime) -> Sleep<'_> {
        Box::pin(ManualSleep {
            clock: self.clone(),
            deadline,
        })
    }
}

/// The future behind [`ManualClock`]'s [`Timer::sleep_until`].
struct ManualSleep {
    clock: ManualClock,
    deadline: MonotonicTime,
}

impl Future for ManualSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let mut state = self.clock.lock();
        if state.monotonic >= self.deadline {
            return Poll::Ready(());
        }
        let already_registered = state.sleepers.iter().any(|sleeper| {
            sleeper.deadline == self.deadline && sleeper.waker.will_wake(context.waker())
        });
        if !already_registered {
            state.sleepers.push(Sleeper {
                deadline: self.deadline,
                waker: context.waker().clone(),
            });
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    use super::*;

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn poll_once(sleep: &mut Sleep<'_>, waker: &Waker) -> Poll<()> {
        sleep.as_mut().poll(&mut Context::from_waker(waker))
    }

    #[test]
    fn time_stands_still_until_advanced() {
        let clock = ManualClock::new(UnixSeconds::new(1_000));
        assert_eq!(Clock::now(&clock), UnixSeconds::new(1_000));
        assert_eq!(Timer::now(&clock), MonotonicTime::ORIGIN);
        clock.advance(Duration::from_secs(5));
        assert_eq!(Clock::now(&clock), UnixSeconds::new(1_005));
        assert_eq!(
            Timer::now(&clock),
            MonotonicTime::from_elapsed(Duration::from_secs(5))
        );
    }

    #[test]
    fn sub_second_advances_accumulate_on_the_wall_clock() {
        let clock = ManualClock::new(UnixSeconds::new(0));
        clock.advance(Duration::from_millis(600));
        clock.advance(Duration::from_millis(600));
        assert_eq!(Clock::now(&clock), UnixSeconds::new(1));
    }

    #[test]
    fn setting_the_wall_clock_leaves_monotonic_time_alone() {
        let clock = ManualClock::new(UnixSeconds::new(1_000));
        clock.set_wall(UnixSeconds::new(10));
        assert_eq!(Clock::now(&clock), UnixSeconds::new(10));
        assert_eq!(Timer::now(&clock), MonotonicTime::ORIGIN);
    }

    #[test]
    fn sleep_completes_only_once_its_deadline_is_reached() {
        let clock = ManualClock::new(UnixSeconds::EPOCH);
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        let mut sleep = clock.sleep(Duration::from_secs(10));

        assert_eq!(poll_once(&mut sleep, &waker), Poll::Pending);
        clock.advance(Duration::from_secs(9));
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);
        assert_eq!(poll_once(&mut sleep, &waker), Poll::Pending);

        clock.advance(Duration::from_secs(1));
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert_eq!(poll_once(&mut sleep, &waker), Poll::Ready(()));
    }

    #[test]
    fn repeated_polls_register_one_waker() {
        let clock = ManualClock::new(UnixSeconds::EPOCH);
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        let mut sleep = clock.sleep(Duration::from_secs(1));
        assert_eq!(poll_once(&mut sleep, &waker), Poll::Pending);
        assert_eq!(poll_once(&mut sleep, &waker), Poll::Pending);
        clock.advance(Duration::from_secs(1));
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_parked_sleep_is_visible_until_its_deadline_passes() {
        let clock = ManualClock::new(UnixSeconds::EPOCH);
        let deadline = MonotonicTime::from_elapsed(Duration::from_secs(4));
        let mut sleep = clock.sleep_until(deadline);
        assert!(!clock.is_sleeping_until(deadline), "not polled yet");
        assert_eq!(poll_once(&mut sleep, Waker::noop()), Poll::Pending);
        assert!(clock.is_sleeping_until(deadline));
        let earlier = MonotonicTime::from_elapsed(Duration::from_secs(3));
        assert!(!clock.is_sleeping_until(earlier));
        clock.advance(Duration::from_secs(4));
        assert!(!clock.is_sleeping_until(deadline));
    }

    #[test]
    fn sleeping_until_a_past_deadline_completes_at_once() {
        let clock = ManualClock::new(UnixSeconds::EPOCH);
        clock.advance(Duration::from_secs(3));
        let mut sleep = clock.sleep_until(MonotonicTime::ORIGIN);
        assert_eq!(poll_once(&mut sleep, Waker::noop()), Poll::Ready(()));
    }
}
