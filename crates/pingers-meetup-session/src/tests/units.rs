// SPDX-License-Identifier: AGPL-3.0-only
//! Pure helpers: reconnect backoff, error classes, the view's compass and ages.

use core::future::Future as _;
use core::task::{Context, Waker};
use core::time::Duration;

use pingers_crypto::AccountPublicKey;
use pingers_geo::{Coordinate, Fix, Meters};
use pingers_meetup::{
    CadenceMode, CompassTarget, ExactLocation, LocationVisibility, Observer, PeerLocation,
    coordinate_to_wire,
};
use pingers_mls::{MlsError, StorageError};
use pingers_proto::{GroupId, Lamport, LeafIndex, MeetPoint};
use pingers_store::StoreError;
use pingers_time::{Clock, ManualClock, UnixSeconds};

use super::{TestError, TestResult};
use crate::live::{Reconnect, grant_key_pair, sleep_until};
use crate::{ChannelState, MAX_RECONNECT_DELAY, MeetupView, NetworkError, PeerView, SessionError};

#[test]
fn reconnect_is_immediate_once_then_backs_off_to_a_minute() {
    let delays: Vec<Duration> = (1..=9).map(Reconnect::delay).collect();
    let seconds: Vec<u64> = delays.iter().map(Duration::as_secs).collect();
    assert_eq!(seconds, vec![0, 1, 2, 4, 8, 16, 32, 60, 60]);
    assert_eq!(Reconnect::delay(u32::MAX), MAX_RECONNECT_DELAY);
}

/// A wall clock that moves the shared manual clock on by `jump` each time it is read, as if
/// that much time passed just after the reading.
struct JumpingClock {
    clock: ManualClock,
    jump: Duration,
}

impl Clock for JumpingClock {
    fn now(&self) -> UnixSeconds {
        let now = self.clock.now();
        self.clock.advance(self.jump);
        now
    }
}

#[test]
fn time_passing_while_a_sleep_starts_does_not_push_its_deadline_back() {
    let clock = ManualClock::new(NOW);
    let jumping = JumpingClock {
        clock: clock.clone(),
        jump: Duration::from_secs(5),
    };
    let due = NOW.checked_add(Duration::from_secs(10));
    let mut sleep = core::pin::pin!(sleep_until(&jumping, &clock, due));
    let mut context = Context::from_waker(Waker::noop());
    // Reading the wall clock moved time to 5 s: not due yet.
    assert!(sleep.as_mut().poll(&mut context).is_pending());
    // At 10 s it is due. Had the timer been read after the wall clock, the 5 s that passed in
    // between would have moved the deadline to 15 s.
    clock.advance(Duration::from_secs(5));
    assert!(sleep.as_mut().poll(&mut context).is_ready());
}

#[test]
fn a_sleep_without_a_deadline_never_ends() {
    let clock = ManualClock::new(NOW);
    let mut sleep = core::pin::pin!(sleep_until(&clock, &clock, None));
    let mut context = Context::from_waker(Waker::noop());
    clock.advance(Duration::from_hours(24));
    assert!(sleep.as_mut().poll(&mut context).is_pending());
}

/// Creating and joining run inside pingers-wake's hand-off, whose futures must be `Send`; a
/// non-`Sync` value (the random generator) borrowed across an await once broke that. The
/// check is at compile time: `check` is never called.
#[test]
fn creating_and_joining_are_send() {
    fn send<T: Send>(_future: T) {}
    fn check(runtime: &crate::Runtime, creation: crate::CreateMeetup, joining: crate::JoinMeetup) {
        send(crate::create(
            runtime,
            creation,
            Box::new(pingers_testkit::seeded_rng(1)),
        ));
        send(crate::join(
            runtime,
            joining,
            Box::new(pingers_testkit::seeded_rng(2)),
        ));
    }
    let _: fn(&crate::Runtime, crate::CreateMeetup, crate::JoinMeetup) = check;
}

#[test]
fn only_storage_failures_stop_a_meetup() {
    assert!(SessionError::Store(StoreError::Busy).is_fatal());
    assert!(SessionError::Mls(MlsError::Storage(StorageError::Failed)).is_fatal());
    assert!(SessionError::Internal.is_fatal());
    assert!(!SessionError::Mls(MlsError::InvalidMessage).is_fatal());
    assert!(!SessionError::Network(NetworkError::Unavailable).is_fatal());
    assert!(!SessionError::CommitConflict.is_fatal());
    assert!(!SessionError::Timeout.is_fatal());
}

const NOW: UnixSeconds = UnixSeconds::new(1_800_000_100);
const SEEN: LeafIndex = LeafIndex::new(1);
const HIDDEN: LeafIndex = LeafIndex::new(2);

fn place(latitude: f64) -> Result<Coordinate, TestError> {
    Ok(Coordinate::new(latitude, -0.1246)?)
}

fn peer(leaf: LeafIndex, position: Option<PeerLocation>) -> PeerView {
    PeerView {
        leaf,
        signature_key: AccountPublicKey::from_bytes([7; 32]),
        access: if position.is_some() {
            ExactLocation::Shared { expires: None }
        } else {
            ExactLocation::Private
        },
        holds_own_key: false,
        request_pending_until: None,
        position,
    }
}

/// A view where `SEEN` was at 51.5010 40 s ago and `HIDDEN` is private.
fn view(meet_point: Option<MeetPoint>) -> Result<MeetupView, TestError> {
    let taken_at = NOW
        .checked_sub(Duration::from_secs(40))
        .ok_or(TestError::Missing("time"))?;
    Ok(MeetupView {
        group: GroupId::from_bytes([1; 32]),
        own_leaf: LeafIndex::new(0),
        cadence: CadenceMode::Live,
        visibility: LocationVisibility::AskEachTime,
        channel: ChannelState::Connected,
        meet_point,
        peers: vec![
            peer(
                SEEN,
                Some(PeerLocation {
                    leaf: SEEN,
                    position: place(51.5010)?,
                    accuracy: Meters::new(5.0)?,
                    heading: None,
                    taken_at,
                }),
            ),
            peer(HIDDEN, None),
        ],
    })
}

fn observer() -> Result<Observer, TestError> {
    Ok(Observer {
        fix: Fix {
            coordinate: place(51.5000)?,
            accuracy: Meters::new(5.0)?,
            taken_at: NOW,
        },
        heading: None,
    })
}

#[test]
fn ages_count_from_when_the_position_was_taken() -> TestResult {
    let view = view(None)?;
    assert_eq!(view.position_age(SEEN, NOW), Some(Duration::from_secs(40)));
    assert_eq!(view.position_age(HIDDEN, NOW), None);
    assert_eq!(view.position_age(LeafIndex::new(9), NOW), None);
    Ok(())
}

#[test]
fn compass_points_only_at_known_positions() -> TestResult {
    let view = view(None)?;
    let observer = observer()?;
    let reading = view
        .compass(CompassTarget::Member(SEEN), &observer, NOW)
        .ok_or(TestError::Missing("a reading"))?;
    assert_eq!(reading.age, Duration::from_secs(40));
    assert!((reading.distance.get() - 111.0).abs() < 2.0);
    assert!(reading.bearing.degrees() < 1.0 || reading.bearing.degrees() > 359.0);
    assert_eq!(
        view.compass(CompassTarget::Member(HIDDEN), &observer, NOW),
        None
    );
    assert_eq!(view.compass(CompassTarget::MeetPoint, &observer, NOW), None);
    Ok(())
}

#[test]
fn compass_follows_the_meet_point() -> TestResult {
    let observer = observer()?;
    let fixed = view(Some(MeetPoint::Fixed {
        position: coordinate_to_wire(place(51.4990)?)?,
        label: None,
        lamport: Lamport::new(1),
    }))?;
    let reading = fixed
        .compass(CompassTarget::MeetPoint, &observer, NOW)
        .ok_or(TestError::Missing("a reading"))?;
    // A fixed place has no age, and lies south.
    assert_eq!(reading.age, Duration::ZERO);
    assert!((reading.bearing.degrees() - 180.0).abs() < 1.0);

    let at_member = view(Some(MeetPoint::AtMember {
        leaf: SEEN,
        lamport: Lamport::new(2),
    }))?;
    let reading = at_member
        .compass(CompassTarget::MeetPoint, &observer, NOW)
        .ok_or(TestError::Missing("a reading"))?;
    assert_eq!(reading.age, Duration::from_secs(40));

    let at_hidden = view(Some(MeetPoint::AtMember {
        leaf: HIDDEN,
        lamport: Lamport::new(2),
    }))?;
    assert_eq!(
        at_hidden.compass(CompassTarget::MeetPoint, &observer, NOW),
        None
    );
    let cleared = view(Some(MeetPoint::Cleared {
        lamport: Lamport::new(3),
    }))?;
    assert_eq!(
        cleared.compass(CompassTarget::MeetPoint, &observer, NOW),
        None
    );
    Ok(())
}

#[test]
fn grant_keys_follow_the_party_suite() -> TestResult {
    let mut rng: crate::SessionRng = Box::new(pingers_testkit::seeded_rng(5));
    let classical = grant_key_pair(pingers_mls::CipherSuitePolicy::Classical, &mut rng)?;
    assert_eq!(classical.public.as_bytes().len(), 32);
    let quantum = grant_key_pair(pingers_mls::CipherSuitePolicy::QuantumResistant, &mut rng)?;
    // X-Wing public keys are 1216 bytes; without the feature the Party's MLS calls refuse the
    // suite before a grant key is used.
    let expected = if cfg!(feature = "x-wing") { 1216 } else { 32 };
    assert_eq!(quantum.public.as_bytes().len(), expected);
    Ok(())
}
