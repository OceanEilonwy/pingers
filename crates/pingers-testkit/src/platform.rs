// SPDX-License-Identifier: AGPL-3.0-only
//! A scriptable stand-in for the Android shell.

use core::time::Duration;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pingers_gate::FocusState;
use pingers_geo::{Circle, Coordinate, Fix};
use pingers_proto::{PushPlatform, PushToken};
use pingers_time::{ManualClock, MonotonicTime, Timer as _};
use zeroize::Zeroizing;

/// How hard the platform should try for a fix (mirrors `pingers_client::FixQuality`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FixQuality {
    /// Balanced power and accuracy.
    Balanced,
    /// GPS quality.
    High,
}

/// The location mode the shell runs (mirrors `pingers_client::LocationMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationMode {
    /// Region monitoring only. The mode a fresh [`FakePlatform`] starts in.
    Idle,
    /// Inside a Spot alone.
    Dwell,
    /// In a Meetup, app in the background.
    MeetupBackground,
    /// In a Meetup with the compass open.
    MeetupForeground,
}

/// An OS geofence the core registered (mirrors `pingers_client::OsRegion`).
#[derive(Clone, Debug, PartialEq)]
pub struct Geofence {
    /// The core's opaque id, reported back with each transition.
    pub id: String,
    /// The circle.
    pub region: Circle,
}

/// The device's push registration (mirrors `pingers_client::PushTokens`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRegistration {
    /// FCM or a UnifiedPush distributor.
    pub platform: PushPlatform,
    /// The token or endpoint.
    pub token: PushToken,
}

/// One call to [`FakePlatform::request_fix`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixRequest {
    /// The quality asked for.
    pub quality: FixQuality,
    /// How long the core is prepared to wait.
    pub timeout: Duration,
    /// Monotonic time of the request.
    pub requested_at: MonotonicTime,
}

/// The background wake the core scheduled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WakeRequest {
    /// The delay the core asked for.
    pub after: Duration,
    /// Monotonic time at which the wake is due: request time plus `after`.
    pub due: MonotonicTime,
}

/// Whether the secure store answers (the Keystore can be locked before first unlock).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StoreAvailability {
    /// Reads and writes succeed.
    Available,
    /// Every call fails with [`SecureStoreError::Unavailable`].
    Locked,
}

/// Why a secure-store call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SecureStoreError {
    /// The test set [`StoreAvailability::Locked`].
    #[error("secure store unavailable")]
    Unavailable,
}

/// A scriptable fake of the platform the core runs on.
///
/// Clones share state, so a test keeps one handle to script and inspect while the code under
/// test holds another. Nothing happens on its own: fixes are answered from a script, wakes are
/// recorded with their due time on the shared [`ManualClock`], and the test decides when to
/// deliver them.
///
/// - **Fixes**: [`script_fix`](Self::script_fix) queues fixes; each
///   [`request_fix`](Self::request_fix) records the request and moves the next scripted fix to
///   the answers that [`take_answered_fixes`](Self::take_answered_fixes) hands to the core's
///   location entry point. An empty script means the request times out.
/// - **Focus/DND**: [`set_focus`](Self::set_focus), read fresh by [`focus`](Self::focus).
/// - **Wakes**: [`schedule_wake`](Self::schedule_wake) replaces the pending wake (WorkManager
///   unique work); [`take_due_wake`](Self::take_due_wake) returns it once the clock reaches it.
/// - **OS geofences**: [`set_os_geofences`](Self::set_os_geofences) replaces the registered set;
///   [`regions_containing`](Self::regions_containing) tells a test which transitions the OS
///   would report for a position.
/// - **Secure store**: an in-memory map that can be [locked](StoreAvailability::Locked).
#[derive(Clone, Debug)]
pub struct FakePlatform {
    clock: ManualClock,
    state: Arc<Mutex<State>>,
}

#[derive(Debug)]
struct State {
    scripted_fixes: VecDeque<Fix>,
    answered_fixes: VecDeque<Fix>,
    fix_requests: Vec<FixRequest>,
    focus: FocusState,
    pending_wake: Option<WakeRequest>,
    wake_history: Vec<WakeRequest>,
    geofences: Vec<Geofence>,
    geofence_updates: usize,
    location_modes: Vec<LocationMode>,
    secure_store: HashMap<String, Zeroizing<Vec<u8>>>,
    store_availability: StoreAvailability,
    push: Option<PushRegistration>,
}

impl FakePlatform {
    /// A platform on `clock`: no scripted fixes, Focus off, no wake pending, no geofences,
    /// [`LocationMode::Idle`], an empty unlocked secure store and no push registration.
    #[must_use]
    pub fn new(clock: ManualClock) -> Self {
        let state = State {
            scripted_fixes: VecDeque::new(),
            answered_fixes: VecDeque::new(),
            fix_requests: Vec::new(),
            focus: FocusState::Off,
            pending_wake: None,
            wake_history: Vec::new(),
            geofences: Vec::new(),
            geofence_updates: 0,
            location_modes: Vec::new(),
            secure_store: HashMap::new(),
            store_availability: StoreAvailability::Available,
            push: None,
        };
        Self {
            clock,
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// The clock the platform stamps requests with.
    #[must_use]
    pub fn clock(&self) -> &ManualClock {
        &self.clock
    }

    // Fixes.

    /// Queues `fix` as the answer to a future [`request_fix`](Self::request_fix).
    pub fn script_fix(&self, fix: Fix) {
        self.lock().scripted_fixes.push_back(fix);
    }

    /// Queues several fixes, answered in order.
    pub fn script_fixes(&self, fixes: impl IntoIterator<Item = Fix>) {
        self.lock().scripted_fixes.extend(fixes);
    }

    /// How many scripted fixes have not been requested yet.
    #[must_use]
    pub fn scripted_fixes_remaining(&self) -> usize {
        self.lock().scripted_fixes.len()
    }

    /// The core asks for a fix: records the request and answers it with the next scripted fix,
    /// if any (otherwise the request times out and nothing is answered).
    pub fn request_fix(&self, quality: FixQuality, timeout: Duration) {
        let requested_at = self.clock.now();
        let mut state = self.lock();
        state.fix_requests.push(FixRequest {
            quality,
            timeout,
            requested_at,
        });
        if let Some(fix) = state.scripted_fixes.pop_front() {
            state.answered_fixes.push_back(fix);
        }
    }

    /// The fixes that answered requests so far, oldest first, removed from the platform. The
    /// harness delivers them to the core's location entry point.
    #[must_use]
    pub fn take_answered_fixes(&self) -> Vec<Fix> {
        self.lock().answered_fixes.drain(..).collect()
    }

    /// Every fix request so far, oldest first.
    #[must_use]
    pub fn fix_requests(&self) -> Vec<FixRequest> {
        self.lock().fix_requests.clone()
    }

    // Focus.

    /// Sets the interruption filter the core will read next.
    pub fn set_focus(&self, focus: FocusState) {
        self.lock().focus = focus;
    }

    /// The current interruption filter.
    #[must_use]
    pub fn focus(&self) -> FocusState {
        self.lock().focus
    }

    // Wakes.

    /// The core schedules its next background wake `after` from now, replacing any pending
    /// one.
    pub fn schedule_wake(&self, after: Duration) {
        let request = WakeRequest {
            after,
            due: self.clock.now().saturating_add(after),
        };
        let mut state = self.lock();
        state.pending_wake = Some(request);
        state.wake_history.push(request);
    }

    /// The pending wake, if the core scheduled one that has not been taken.
    #[must_use]
    pub fn pending_wake(&self) -> Option<WakeRequest> {
        self.lock().pending_wake
    }

    /// Removes and returns the pending wake if the clock has reached its due time.
    #[must_use]
    pub fn take_due_wake(&self) -> Option<WakeRequest> {
        let now = self.clock.now();
        let mut state = self.lock();
        let due = state.pending_wake.filter(|wake| wake.due <= now)?;
        state.pending_wake = None;
        Some(due)
    }

    /// Every wake the core scheduled, including replaced ones, oldest first.
    #[must_use]
    pub fn wake_history(&self) -> Vec<WakeRequest> {
        self.lock().wake_history.clone()
    }

    // OS geofences.

    /// The core replaces all registered OS geofences.
    pub fn set_os_geofences(&self, geofences: &[Geofence]) {
        let mut state = self.lock();
        state.geofences = geofences.to_vec();
        state.geofence_updates = state.geofence_updates.saturating_add(1);
    }

    /// The geofences registered now.
    #[must_use]
    pub fn os_geofences(&self) -> Vec<Geofence> {
        self.lock().geofences.clone()
    }

    /// How many times the core replaced the geofence set.
    #[must_use]
    pub fn geofence_updates(&self) -> usize {
        self.lock().geofence_updates
    }

    /// The ids of the registered geofences that contain `position`: the regions the OS would
    /// report as entered if the device moved there from outside them.
    #[must_use]
    pub fn regions_containing(&self, position: Coordinate) -> Vec<String> {
        self.lock()
            .geofences
            .iter()
            .filter(|geofence| geofence.region.contains(position))
            .map(|geofence| geofence.id.clone())
            .collect()
    }

    // Location mode.

    /// The core switches the location mode.
    pub fn set_location_mode(&self, mode: LocationMode) {
        self.lock().location_modes.push(mode);
    }

    /// The current location mode.
    #[must_use]
    pub fn location_mode(&self) -> LocationMode {
        self.lock()
            .location_modes
            .last()
            .copied()
            .unwrap_or(LocationMode::Idle)
    }

    /// Every mode switch so far, oldest first.
    #[must_use]
    pub fn location_mode_history(&self) -> Vec<LocationMode> {
        self.lock().location_modes.clone()
    }

    // Secure store.

    /// Reads a value from the secure store.
    ///
    /// # Errors
    ///
    /// [`SecureStoreError::Unavailable`] while the store is locked.
    pub fn secure_store_get(
        &self,
        key: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SecureStoreError> {
        let state = self.lock();
        state.check_store()?;
        Ok(state.secure_store.get(key).cloned())
    }

    /// Writes a value to the secure store, replacing any previous value.
    ///
    /// # Errors
    ///
    /// [`SecureStoreError::Unavailable`] while the store is locked.
    pub fn secure_store_put(&self, key: &str, value: &[u8]) -> Result<(), SecureStoreError> {
        let mut state = self.lock();
        state.check_store()?;
        state
            .secure_store
            .insert(key.to_owned(), Zeroizing::new(value.to_vec()));
        Ok(())
    }

    /// Locks or unlocks the secure store.
    pub fn set_store_availability(&self, availability: StoreAvailability) {
        self.lock().store_availability = availability;
    }

    // Push.

    /// Sets the push registration the core will read.
    pub fn set_push_registration(&self, registration: Option<PushRegistration>) {
        self.lock().push = registration;
    }

    /// The current push registration.
    #[must_use]
    pub fn push_registration(&self) -> Option<PushRegistration> {
        self.lock().push.clone()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A test that panicked while holding the lock has already failed; keep the state usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn check_store(&self) -> Result<(), SecureStoreError> {
        match self.store_availability {
            StoreAvailability::Available => Ok(()),
            StoreAvailability::Locked => Err(SecureStoreError::Unavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_geo::{GeoError, Meters};
    use pingers_time::UnixSeconds;

    use super::*;
    use crate::{TEST_START, test_clock};

    fn fix(latitude: f64) -> Result<Fix, GeoError> {
        Ok(Fix {
            coordinate: Coordinate::new(latitude, 0.0)?,
            accuracy: Meters::from_whole(10),
            taken_at: TEST_START,
        })
    }

    fn circle(id: &str, latitude: f64) -> Result<Geofence, GeoError> {
        Ok(Geofence {
            id: id.to_owned(),
            region: Circle::new(Coordinate::new(latitude, 0.0)?, Meters::from_whole(200))?,
        })
    }

    #[test]
    fn a_fresh_platform_is_quiet() {
        let platform = FakePlatform::new(test_clock());
        assert_eq!(platform.focus(), FocusState::Off);
        assert_eq!(platform.location_mode(), LocationMode::Idle);
        assert_eq!(platform.pending_wake(), None);
        assert_eq!(platform.geofence_updates(), 0);
        assert!(platform.os_geofences().is_empty());
        assert_eq!(platform.push_registration(), None);
        assert!(platform.fix_requests().is_empty());
    }

    #[test]
    fn requests_are_answered_from_the_script_in_order() -> Result<(), GeoError> {
        let platform = FakePlatform::new(test_clock());
        platform.script_fixes([fix(1.0)?, fix(2.0)?]);
        assert_eq!(platform.scripted_fixes_remaining(), 2);
        platform.request_fix(FixQuality::Balanced, Duration::from_secs(30));
        assert_eq!(platform.scripted_fixes_remaining(), 1);
        platform.request_fix(FixQuality::High, Duration::from_secs(5));
        assert_eq!(platform.scripted_fixes_remaining(), 0);
        assert_eq!(platform.take_answered_fixes(), [fix(1.0)?, fix(2.0)?]);
        assert!(platform.take_answered_fixes().is_empty());
        Ok(())
    }

    #[test]
    fn a_request_with_an_empty_script_times_out_but_is_recorded() {
        let clock = test_clock();
        let platform = FakePlatform::new(clock.clone());
        clock.advance(Duration::from_secs(7));
        platform.request_fix(FixQuality::High, Duration::from_secs(5));
        assert!(platform.take_answered_fixes().is_empty());
        assert_eq!(
            platform.fix_requests(),
            [FixRequest {
                quality: FixQuality::High,
                timeout: Duration::from_secs(5),
                requested_at: MonotonicTime::from_elapsed(Duration::from_secs(7)),
            }]
        );
    }

    #[test]
    fn a_fix_scripted_after_a_timed_out_request_waits_for_the_next() -> Result<(), GeoError> {
        let platform = FakePlatform::new(test_clock());
        platform.request_fix(FixQuality::Balanced, Duration::from_secs(1));
        platform.script_fix(fix(3.0)?);
        assert!(platform.take_answered_fixes().is_empty());
        platform.request_fix(FixQuality::Balanced, Duration::from_secs(1));
        assert_eq!(platform.take_answered_fixes(), [fix(3.0)?]);
        Ok(())
    }

    #[test]
    fn focus_is_read_fresh() {
        let platform = FakePlatform::new(test_clock());
        let core_handle = platform.clone();
        platform.set_focus(FocusState::On);
        assert_eq!(core_handle.focus(), FocusState::On);
    }

    #[test]
    fn a_wake_is_due_only_once_the_clock_reaches_it() {
        let clock = test_clock();
        let platform = FakePlatform::new(clock.clone());
        platform.schedule_wake(Duration::from_mins(15));
        assert_eq!(
            platform.pending_wake().map(|wake| wake.due),
            Some(MonotonicTime::from_elapsed(Duration::from_mins(15)))
        );
        clock.advance(Duration::from_secs(899));
        assert_eq!(platform.take_due_wake(), None);
        clock.advance(Duration::from_secs(1));
        assert_eq!(
            platform.take_due_wake().map(|wake| wake.after),
            Some(Duration::from_mins(15))
        );
        assert_eq!(platform.pending_wake(), None);
        assert_eq!(platform.take_due_wake(), None);
    }

    #[test]
    fn a_new_wake_replaces_the_pending_one_but_history_keeps_both() {
        let platform = FakePlatform::new(test_clock());
        platform.schedule_wake(Duration::from_mins(15));
        platform.schedule_wake(Duration::from_mins(1));
        assert_eq!(
            platform.pending_wake().map(|wake| wake.after),
            Some(Duration::from_mins(1))
        );
        let history: Vec<Duration> = platform
            .wake_history()
            .iter()
            .map(|wake| wake.after)
            .collect();
        assert_eq!(history, [Duration::from_mins(15), Duration::from_mins(1)]);
    }

    #[test]
    fn geofences_are_replaced_and_counted() -> Result<(), GeoError> {
        let platform = FakePlatform::new(test_clock());
        platform.set_os_geofences(&[circle("a", 0.0)?, circle("b", 1.0)?]);
        platform.set_os_geofences(&[circle("c", 2.0)?]);
        assert_eq!(platform.os_geofences(), [circle("c", 2.0)?]);
        assert_eq!(platform.geofence_updates(), 2);
        Ok(())
    }

    #[test]
    fn regions_containing_reports_only_regions_around_the_position() -> Result<(), GeoError> {
        let platform = FakePlatform::new(test_clock());
        platform.set_os_geofences(&[circle("near", 0.0)?, circle("far", 1.0)?]);
        assert_eq!(
            platform.regions_containing(Coordinate::new(0.0, 0.0)?),
            ["near"]
        );
        assert!(
            platform
                .regions_containing(Coordinate::new(0.5, 0.0)?)
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn location_mode_follows_the_latest_switch() {
        let platform = FakePlatform::new(test_clock());
        platform.set_location_mode(LocationMode::Dwell);
        platform.set_location_mode(LocationMode::MeetupBackground);
        assert_eq!(platform.location_mode(), LocationMode::MeetupBackground);
        assert_eq!(
            platform.location_mode_history(),
            [LocationMode::Dwell, LocationMode::MeetupBackground]
        );
    }

    #[test]
    fn secure_store_round_trips_and_overwrites() -> Result<(), SecureStoreError> {
        let platform = FakePlatform::new(test_clock());
        assert_eq!(platform.secure_store_get("key")?, None);
        platform.secure_store_put("key", b"first")?;
        platform.secure_store_put("key", b"second")?;
        assert_eq!(
            platform
                .secure_store_get("key")?
                .as_deref()
                .map(Vec::as_slice),
            Some(&b"second"[..])
        );
        assert_eq!(platform.secure_store_get("other")?, None);
        Ok(())
    }

    #[test]
    fn a_locked_store_refuses_reads_and_writes_until_unlocked() -> Result<(), SecureStoreError> {
        let platform = FakePlatform::new(test_clock());
        platform.secure_store_put("key", b"kept")?;
        platform.set_store_availability(StoreAvailability::Locked);
        assert_eq!(
            platform.secure_store_get("key"),
            Err(SecureStoreError::Unavailable)
        );
        assert_eq!(
            platform.secure_store_put("key", b"lost"),
            Err(SecureStoreError::Unavailable)
        );
        platform.set_store_availability(StoreAvailability::Available);
        assert_eq!(
            platform
                .secure_store_get("key")?
                .as_deref()
                .map(Vec::as_slice),
            Some(&b"kept"[..])
        );
        Ok(())
    }

    #[test]
    fn push_registration_can_be_set_and_cleared() {
        let platform = FakePlatform::new(test_clock());
        let registration = PushRegistration {
            platform: PushPlatform::Fcm,
            token: PushToken::from_bytes(b"token".to_vec()),
        };
        platform.set_push_registration(Some(registration.clone()));
        assert_eq!(platform.push_registration(), Some(registration));
        platform.set_push_registration(None);
        assert_eq!(platform.push_registration(), None);
    }

    #[test]
    fn the_platform_shares_the_callers_clock() {
        let clock = ManualClock::new(UnixSeconds::new(5));
        let platform = FakePlatform::new(clock.clone());
        clock.advance(Duration::from_secs(3));
        assert_eq!(
            pingers_time::Clock::now(platform.clock()),
            UnixSeconds::new(8)
        );
    }
}
