// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use core::time::Duration;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Mutex;

use pingers_crypto::{MailboxReadKey, mailbox_id, push_bind_for_mailbox};
use pingers_proto::{
    KeyConfigHash, MailboxSpec, PushHandle, PushRelayKeyId, RelayId, RelayRef, RelayUrl,
    SealedPush, SealedPushBlob, SizeClass, TimeToLive, Urgency,
};
use pingers_testkit::{ManualClock, TestRng, seeded_rng, test_clock};
use pingers_time::UnixSeconds;
use zeroize::Zeroizing;

use super::*;
use crate::relay::{
    ListedRelays, PushRelay, RelayAnswer, RelayCredential, RelayTarget, RelayUnreachable,
};
use crate::storage::MemoryStorage;
use crate::wake::{Forwarded, QUIET_DELAY_MAX, forward};

/// Why a test failed outside an assertion.
#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Mailbox(#[from] MailboxError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error("expected a push job")]
    NoWake,
    #[error("future did not complete")]
    Pending,
}

/// A directory with one relay that a test can switch off.
#[derive(Clone)]
struct Directory {
    enabled: Rc<Cell<bool>>,
}

impl RelayDirectory for Directory {
    fn resolve(&self, relay: &RelayRef) -> Option<RelayTarget> {
        if !self.enabled.get() {
            return None;
        }
        ListedRelays::new([(RelayId::new(1), target().ok()?)]).resolve(relay)
    }
}

/// Counts the posts it is asked about and refuses once `allow` reaches zero.
struct CountingLimit {
    allow: Rc<Cell<u32>>,
    asked: Rc<Cell<u32>>,
}

impl PostLimit for CountingLimit {
    fn admit(&mut self, _mailbox: &MailboxId, _now: UnixSeconds) -> Admission {
        self.asked.set(self.asked.get().saturating_add(1));
        match self.allow.get().checked_sub(1) {
            Some(left) => {
                self.allow.set(left);
                Admission::Admit
            }
            None => Admission::Refuse,
        }
    }
}

type Service = Mailboxes<MemoryStorage, ManualClock, Directory, CountingLimit>;

struct Fixture {
    clock: ManualClock,
    service: Service,
    relay_enabled: Rc<Cell<bool>>,
    allow: Rc<Cell<u32>>,
    asked: Rc<Cell<u32>>,
    rng: TestRng,
}

impl Fixture {
    fn new(max_messages: usize) -> Self {
        let clock = test_clock();
        let relay_enabled = Rc::new(Cell::new(true));
        let allow = Rc::new(Cell::new(u32::MAX));
        let asked = Rc::new(Cell::new(0));
        let service = Mailboxes::new(MailboxParts {
            storage: MemoryStorage::new(),
            clock: clock.clone(),
            directory: Directory {
                enabled: Rc::clone(&relay_enabled),
            },
            limit: CountingLimit {
                allow: Rc::clone(&allow),
                asked: Rc::clone(&asked),
            },
            rules: MailboxRules {
                max_messages: NonZeroUsize::new(max_messages).unwrap_or(NonZeroUsize::MIN),
            },
        });
        Self {
            clock,
            service,
            relay_enabled,
            allow,
            asked,
            rng: seeded_rng(3),
        }
    }

    fn post(&mut self, id: &MailboxId, body: &Padded) -> Result<Posted, MailboxError> {
        self.service.post(id, body, &mut self.rng)
    }
}

fn target() -> Result<RelayTarget, pingers_proto::ProtoError> {
    Ok(RelayTarget {
        url: RelayUrl::new(String::from("https://push.example"))?,
        credential: RelayCredential::new(String::from("credential")),
    })
}

fn read_key(fill: u8) -> MailboxReadKey {
    MailboxReadKey::from_bytes(Zeroizing::new([fill; 32]))
}

fn sealed_push(urgency: Urgency) -> SealedPush {
    SealedPush {
        relay: RelayRef::Listed(RelayId::new(1)),
        key_id: PushRelayKeyId::new(4),
        blob: SealedPushBlob::from_bytes([0x5a; 320]),
        urgency,
        handle: PushHandle::from_bytes([0x11; 8]),
    }
}

fn spec(fill: u8, ttl: Duration, push: Option<SealedPush>) -> Result<MailboxSpec, TestError> {
    Ok(MailboxSpec {
        mailbox_id: mailbox_id(&read_key(fill)),
        time_to_live: TimeToLive::new(ttl)?,
        push,
    })
}

fn hour() -> Duration {
    Duration::from_hours(1)
}

fn knock() -> Result<Padded, TestError> {
    Ok(Padded::pad(b"knock", SizeClass::Bytes1024)?)
}

#[test]
fn registered_mailbox_receives_posts_in_order() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    let first = fixture.post(&spec.mailbox_id, &knock()?)?;
    let large = Padded::pad(b"welcome", SizeClass::Bytes32768)?;
    let second = fixture.post(&spec.mailbox_id, &large)?;
    assert_eq!(first.sequence, SequenceNumber::new(1));
    assert_eq!(second.sequence, SequenceNumber::new(2));
    let messages = fixture.service.read(&spec.mailbox_id, &read_key(1), None)?;
    assert_eq!(messages.messages.len(), 2);
    let later = fixture
        .service
        .read(&spec.mailbox_id, &read_key(1), Some(first.sequence))?;
    assert_eq!(
        later.messages.first().map(|message| message.body.clone()),
        Some(large)
    );
    Ok(())
}

#[test]
fn lifetime_of_thirty_days_is_accepted_and_longer_refused() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let thirty_days = Duration::from_hours(30 * 24);
    fixture.service.register(&spec(1, thirty_days, None)?)?;
    let longer = spec(2, thirty_days.saturating_add(Duration::from_secs(1)), None)?;
    assert!(matches!(
        fixture.service.register(&longer),
        Err(MailboxError::LifetimeTooLong(_))
    ));
    assert_eq!(
        fixture
            .service
            .storage()
            .mailbox(&longer.mailbox_id)
            .ok()
            .flatten(),
        None
    );
    Ok(())
}

#[test]
fn push_to_a_relay_the_server_does_not_use_is_refused() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let unlisted = SealedPush {
        relay: RelayRef::Listed(RelayId::new(2)),
        ..sealed_push(Urgency::Alert)
    };
    assert!(matches!(
        fixture.service.register(&spec(1, hour(), Some(unlisted))?),
        Err(MailboxError::UnknownRelay)
    ));
    let custom = SealedPush {
        relay: RelayRef::Custom {
            url: RelayUrl::new(String::from("https://mine.example"))?,
            key_config_hash: KeyConfigHash::from_bytes([0; 32]),
        },
        ..sealed_push(Urgency::Alert)
    };
    assert!(matches!(
        fixture.service.register(&spec(2, hour(), Some(custom))?),
        Err(MailboxError::UnknownRelay)
    ));
    fixture
        .service
        .register(&spec(3, hour(), Some(sealed_push(Urgency::Alert)))?)?;
    Ok(())
}

#[test]
fn live_mailbox_id_cannot_be_registered_twice() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    fixture.service.register(&spec(1, hour(), None)?)?;
    assert!(matches!(
        fixture.service.register(&spec(1, hour(), None)?),
        Err(MailboxError::AlreadyExists)
    ));
    Ok(())
}

#[test]
fn expired_mailbox_id_can_be_registered_again_and_starts_empty() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    fixture.post(&spec.mailbox_id, &knock()?)?;
    fixture.clock.advance(hour());
    fixture.service.register(&spec)?;
    assert!(
        fixture
            .service
            .read(&spec.mailbox_id, &read_key(1), None)?
            .messages
            .is_empty()
    );
    Ok(())
}

#[test]
fn mailbox_expires_after_its_lifetime() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    fixture
        .clock
        .advance(hour().saturating_sub(Duration::from_secs(1)));
    fixture.post(&spec.mailbox_id, &knock()?)?;
    fixture.clock.advance(Duration::from_secs(1));
    assert!(matches!(
        fixture.post(&spec.mailbox_id, &knock()?),
        Err(MailboxError::NotFound)
    ));
    assert!(matches!(
        fixture.service.read(&spec.mailbox_id, &read_key(1), None),
        Err(MailboxError::NotFound)
    ));
    fixture.service.expire()?;
    assert_eq!(
        fixture
            .service
            .storage()
            .mailbox(&spec.mailbox_id)
            .ok()
            .flatten(),
        None
    );
    Ok(())
}

#[test]
fn expire_keeps_live_mailboxes() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    fixture.service.expire()?;
    assert!(
        fixture
            .service
            .storage()
            .mailbox(&spec.mailbox_id)
            .ok()
            .flatten()
            .is_some()
    );
    Ok(())
}

#[test]
fn post_must_be_a_mailbox_size_class() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    let frame_sized = Padded::pad(b"knock", SizeClass::Bytes256)?;
    assert!(matches!(
        fixture.post(&spec.mailbox_id, &frame_sized),
        Err(MailboxError::NotAMailboxClass(_))
    ));
    for class in SizeClass::MAILBOX_POST {
        fixture.post(&spec.mailbox_id, &Padded::pad(b"knock", class)?)?;
    }
    Ok(())
}

#[test]
fn post_to_an_unknown_mailbox_is_refused() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    assert!(matches!(
        fixture.post(&MailboxId::from_bytes([1; 16]), &knock()?),
        Err(MailboxError::NotFound)
    ));
    Ok(())
}

#[test]
fn full_mailbox_refuses_posts_without_consulting_the_limit() -> Result<(), TestError> {
    let mut fixture = Fixture::new(2);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    fixture.post(&spec.mailbox_id, &knock()?)?;
    fixture.post(&spec.mailbox_id, &knock()?)?;
    assert!(matches!(
        fixture.post(&spec.mailbox_id, &knock()?),
        Err(MailboxError::Full)
    ));
    assert_eq!(fixture.asked.get(), 2);
    Ok(())
}

#[test]
fn refused_by_the_limit_stores_nothing() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    fixture.allow.set(1);
    fixture.post(&spec.mailbox_id, &knock()?)?;
    assert!(matches!(
        fixture.post(&spec.mailbox_id, &knock()?),
        Err(MailboxError::RateLimited)
    ));
    let messages = fixture.service.read(&spec.mailbox_id, &read_key(1), None)?;
    assert_eq!(messages.messages.len(), 1);
    Ok(())
}

#[test]
fn reading_or_deleting_needs_the_read_key() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    assert!(matches!(
        fixture.service.read(&spec.mailbox_id, &read_key(2), None),
        Err(MailboxError::WrongReadKey)
    ));
    assert!(matches!(
        fixture.service.delete(&spec.mailbox_id, &read_key(2)),
        Err(MailboxError::WrongReadKey)
    ));
    fixture.service.delete(&spec.mailbox_id, &read_key(1))?;
    assert!(matches!(
        fixture.service.read(&spec.mailbox_id, &read_key(1), None),
        Err(MailboxError::NotFound)
    ));
    assert!(matches!(
        fixture.service.delete(&spec.mailbox_id, &read_key(1)),
        Err(MailboxError::NotFound)
    ));
    Ok(())
}

#[test]
fn deleting_an_expired_mailbox_reports_it_absent() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    fixture.clock.advance(hour());
    assert!(matches!(
        fixture.service.delete(&spec.mailbox_id, &read_key(1)),
        Err(MailboxError::NotFound)
    ));
    Ok(())
}

#[test]
fn mailbox_without_a_push_wakes_nobody() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), None)?;
    fixture.service.register(&spec)?;
    assert_eq!(fixture.post(&spec.mailbox_id, &knock()?)?.wake, None);
    Ok(())
}

#[test]
fn alert_post_forwards_the_blob_as_registered_at_once() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), Some(sealed_push(Urgency::Alert)))?;
    fixture.service.register(&spec)?;
    let job = fixture
        .post(&spec.mailbox_id, &knock()?)?
        .wake
        .ok_or(TestError::NoWake)?;
    assert_eq!(job.mailbox, spec.mailbox_id);
    assert_eq!(job.target, target()?);
    assert_eq!(job.delay, Duration::ZERO);
    assert_eq!(
        job.request,
        PushRequest {
            // The key id the device sealed to, not one from server configuration.
            key_id: PushRelayKeyId::new(4),
            blob: SealedPushBlob::from_bytes([0x5a; 320]),
            bind: push_bind_for_mailbox(&spec.mailbox_id),
            urgency: Urgency::Alert,
            handle: PushHandle::from_bytes([0x11; 8]),
        }
    );
    Ok(())
}

#[test]
fn quiet_post_is_delayed_by_up_to_thirty_seconds() -> Result<(), TestError> {
    let mut fixture = Fixture::new(100);
    let spec = spec(1, hour(), Some(sealed_push(Urgency::Quiet)))?;
    fixture.service.register(&spec)?;
    let mut delays = Vec::new();
    for _ in 0..20 {
        let job = fixture
            .post(&spec.mailbox_id, &knock()?)?
            .wake
            .ok_or(TestError::NoWake)?;
        assert_eq!(job.request.urgency, Urgency::Quiet);
        assert!(job.delay <= QUIET_DELAY_MAX);
        delays.push(job.delay);
    }
    // Drawn per post: twenty draws from 30,001 values are not all equal.
    assert!(delays.iter().any(|delay| Some(delay) != delays.first()));
    Ok(())
}

#[test]
fn post_is_stored_even_if_its_relay_was_removed_from_the_configuration() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), Some(sealed_push(Urgency::Alert)))?;
    fixture.service.register(&spec)?;
    fixture.relay_enabled.set(false);
    let posted = fixture.post(&spec.mailbox_id, &knock()?)?;
    assert_eq!(posted.wake, None);
    assert_eq!(posted.sequence, SequenceNumber::new(1));
    Ok(())
}

/// A relay that answers every push the same way and records what it was sent.
struct FakeRelay {
    answer: Result<RelayAnswer, RelayUnreachable>,
    sent: Mutex<Vec<PushRequest>>,
}

impl FakeRelay {
    fn answering(answer: Result<RelayAnswer, RelayUnreachable>) -> Self {
        Self {
            answer,
            sent: Mutex::new(Vec::new()),
        }
    }

    fn sent(&self) -> usize {
        self.sent.lock().map_or(0, |sent| sent.len())
    }
}

impl PushRelay for FakeRelay {
    fn push(
        &self,
        _target: &RelayTarget,
        request: &PushRequest,
    ) -> impl Future<Output = Result<RelayAnswer, RelayUnreachable>> + Send {
        if let Ok(mut sent) = self.sent.lock() {
            sent.push(request.clone());
        }
        let answer = self.answer;
        async move { answer }
    }
}

/// Polls a future once with a waker that does nothing; the manual clock drives sleeps.
fn poll_once<F: Future>(future: core::pin::Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

fn shared(fixture: Fixture) -> (Mutex<Service>, ManualClock, TestRng) {
    (Mutex::new(fixture.service), fixture.clock, fixture.rng)
}

fn post_shared(
    service: &Mutex<Service>,
    id: &MailboxId,
    rng: &mut TestRng,
) -> Result<Option<PushJob>, TestError> {
    let mut service = service
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(service.post(id, &knock()?, rng)?.wake)
}

fn run(
    job: &PushJob,
    relay: &FakeRelay,
    clock: &ManualClock,
    service: &Mutex<Service>,
) -> Result<Forwarded, TestError> {
    let mut future = pin!(forward(job, relay, clock, service));
    match poll_once(future.as_mut()) {
        Poll::Ready(outcome) => Ok(outcome?),
        Poll::Pending => Err(TestError::Pending),
    }
}

#[test]
fn quiet_push_reaches_the_relay_only_after_its_delay() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), Some(sealed_push(Urgency::Quiet)))?;
    fixture.service.register(&spec)?;
    let (service, clock, mut rng) = shared(fixture);
    let job = post_shared(&service, &spec.mailbox_id, &mut rng)?.ok_or(TestError::NoWake)?;
    assert!(job.delay > Duration::from_millis(1));
    let relay = FakeRelay::answering(Ok(RelayAnswer::Accepted));

    let mut future = pin!(forward(&job, &relay, &clock, &service));
    assert!(poll_once(future.as_mut()).is_pending());
    clock.advance(job.delay.saturating_sub(Duration::from_millis(1)));
    assert!(poll_once(future.as_mut()).is_pending());
    assert_eq!(relay.sent(), 0);
    clock.advance(Duration::from_millis(1));
    assert!(matches!(
        poll_once(future.as_mut()),
        Poll::Ready(Ok(Forwarded::Sent))
    ));
    assert_eq!(relay.sent(), 1);
    Ok(())
}

#[test]
fn relay_answering_gone_deletes_the_blob() -> Result<(), TestError> {
    let mut fixture = Fixture::new(10);
    let spec = spec(1, hour(), Some(sealed_push(Urgency::Alert)))?;
    fixture.service.register(&spec)?;
    let (service, clock, mut rng) = shared(fixture);
    let job = post_shared(&service, &spec.mailbox_id, &mut rng)?.ok_or(TestError::NoWake)?;
    let relay = FakeRelay::answering(Ok(RelayAnswer::Gone));
    assert_eq!(run(&job, &relay, &clock, &service)?, Forwarded::BlobDeleted);

    // The mailbox and its message stay; the next post wakes nobody.
    assert_eq!(post_shared(&service, &spec.mailbox_id, &mut rng)?, None);
    let service = service
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let messages = service.read(&spec.mailbox_id, &read_key(1), None)?;
    assert_eq!(messages.messages.len(), 2);
    Ok(())
}

#[test]
fn other_relay_answers_keep_the_blob_and_queue_nothing() -> Result<(), TestError> {
    let answers = [
        Ok(RelayAnswer::Accepted),
        Ok(RelayAnswer::Rejected),
        Ok(RelayAnswer::RateLimited),
        Ok(RelayAnswer::Failed),
        Err(RelayUnreachable),
    ];
    for answer in answers {
        let mut fixture = Fixture::new(10);
        let spec = spec(1, hour(), Some(sealed_push(Urgency::Alert)))?;
        fixture.service.register(&spec)?;
        let (service, clock, mut rng) = shared(fixture);
        let job = post_shared(&service, &spec.mailbox_id, &mut rng)?.ok_or(TestError::NoWake)?;
        let relay = FakeRelay::answering(answer);
        let expected = if answer == Ok(RelayAnswer::Accepted) {
            Forwarded::Sent
        } else {
            Forwarded::NotSent
        };
        assert_eq!(run(&job, &relay, &clock, &service)?, expected);
        assert_eq!(relay.sent(), 1);
        assert!(post_shared(&service, &spec.mailbox_id, &mut rng)?.is_some());
    }
    Ok(())
}
