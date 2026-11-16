// SPDX-License-Identifier: AGPL-3.0-only
//! Group wakes against removals: a wake that comes due after its subscription was deleted is
//! not sent; a subscription delete, a commit's `subscriptions_removed` and a group delete each
//! wait for the pushes in flight that they end; and after a `410 Gone` the subscription is
//! never pushed again, even by a wake already queued behind it.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use pingers_delivery::{Delivery, GroupLifetime, GroupStart, SqliteDeliveryStore, Wake};
use pingers_mailbox::{PushRelayUrl, RelayAnswer, RelayCredential, RelayTarget};
use pingers_proto::{
    AppMessage, CommitRequest, CreateGroup, Delivery as Mode, Epoch, GroupAuthKey, GroupId,
    MlsMessageBytes, PushHandle, PushRelayKeyId, PushRequest, RelayId, RelayRef, SealedPush,
    SealedPushBlob, Subscription, SubscriptionId, TimeToLive, Urgency, WireDecode as _,
};
use pingers_time::{ManualClock, UnixSeconds};
use tokio::sync::{Notify, mpsc};

use super::{STEP, TestError, TestResult, rng};
use crate::tasks::{GroupPusher, HttpPushRelay, WakeOutcome};

/// What the test relay saw, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    /// A push for the subscription with this handle arrived.
    Arrived(PushHandle),
    /// The relay answered it.
    Answered,
    /// The test's delete returned.
    Deleted,
}

type Log = Arc<Mutex<Vec<Event>>>;

fn record(log: &Log, event: Event) {
    log.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(event);
}

fn events(log: &Log) -> Vec<Event> {
    log.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// A push relay on loopback answering each push with `status`; with a `gate`, each answer
/// waits for one `notify_one` on it. Each arrival is also sent on the returned receiver.
async fn relay(
    status: StatusCode,
    gate: Option<Arc<Notify>>,
    log: &Log,
) -> Result<(SocketAddr, mpsc::UnboundedReceiver<()>), TestError> {
    let (arrived, arrivals) = mpsc::unbounded_channel();
    let log = Arc::clone(log);
    let router = Router::new().route(
        "/v1/push",
        post(move |body: axum::body::Bytes| {
            let (log, gate, arrived) = (Arc::clone(&log), gate.clone(), arrived.clone());
            async move {
                if let Ok(request) = PushRequest::decode(&body) {
                    record(&log, Event::Arrived(request.handle));
                }
                let _ = arrived.send(());
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                record(&log, Event::Answered);
                status
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((address, arrivals))
}

fn handle(tag: u8) -> PushHandle {
    PushHandle::from_bytes([tag; 8])
}

fn subscription(tag: u8) -> Result<Subscription, TestError> {
    Ok(Subscription {
        id: SubscriptionId::from_bytes([tag; 16]),
        push: SealedPush {
            relay: RelayRef::Listed(RelayId::new(1)),
            key_id: PushRelayKeyId::new(1),
            blob: SealedPushBlob::from_bytes([tag; 320]),
            urgency: Urgency::Quiet,
            handle: handle(tag),
        },
        time_to_live: TimeToLive::new(Duration::from_hours(2))?,
        owner: pingers_proto::SubscriptionOwner::from_bytes([7; 32]),
    })
}

/// A group with subscriptions 1 and 2, a pusher to the relay at `relay`, and the wakes one
/// alert makes.
struct World {
    pusher: GroupPusher,
    group: GroupId,
    target: RelayTarget,
    wakes: Vec<Wake>,
}

impl World {
    fn new(relay: SocketAddr) -> Result<Self, TestError> {
        let clock = ManualClock::new(UnixSeconds::new(1_000_000));
        let groups = Delivery::new(
            SqliteDeliveryStore::open_in_memory()?,
            Arc::new(clock.clone()),
        );
        let group = GroupId::from_bytes([7; 32]);
        groups.create_group(
            CreateGroup {
                group_id: group,
                auth_key: GroupAuthKey::from_bytes([1; 32]),
                subscriptions: vec![subscription(1)?, subscription(2)?],
                state: None,
            },
            GroupStart {
                lifetime: GroupLifetime::Meetup,
                epoch: Epoch::new(0),
            },
        )?;
        let wakes = groups
            .post(
                &group,
                AppMessage {
                    epoch: Epoch::new(0),
                    body: MlsMessageBytes::from_bytes(vec![9; 8]),
                    delivery: Mode::Stored,
                    wake: Some(Urgency::Alert),
                },
                &mut rng(1),
            )?
            .wakes;
        let http = HttpPushRelay::new(Arc::new(clock), STEP, Vec::new())?;
        Ok(Self {
            pusher: GroupPusher::new(Arc::new(http), Arc::new(groups)),
            group,
            target: RelayTarget {
                url: PushRelayUrl::new(format!("http://{relay}"))?,
                credential: RelayCredential::new("credential".to_owned()),
            },
            wakes,
        })
    }

    fn wake(&self, tag: u8) -> Result<Wake, TestError> {
        self.wakes
            .iter()
            .find(|wake| wake.subscription == SubscriptionId::from_bytes([tag; 16]))
            .cloned()
            .ok_or_else(|| "no wake for that subscription".into())
    }

    async fn send(&self, tag: u8) -> Result<WakeOutcome, TestError> {
        let wake = self.wake(tag)?;
        Ok(tokio::time::timeout(STEP, self.pusher.send(self.group, wake, &self.target)).await?)
    }

    /// Deletes subscription `tag` as the DELETE route does, on the blocking pool.
    fn delete(
        &self,
        tag: u8,
    ) -> tokio::task::JoinHandle<Result<(), pingers_delivery::DeliveryError>> {
        let (pusher, group) = (self.pusher.clone(), self.group);
        tokio::task::spawn_blocking(move || {
            pusher.remove_subscription(&group, &SubscriptionId::from_bytes([tag; 16]))
        })
    }

    /// Commits epoch 0 removing the subscriptions `removed`, as the commit route does, on the
    /// blocking pool, and logs [`Event::Deleted`] the moment it returns.
    fn commit(
        &self,
        removed: &[u8],
        log: &Log,
    ) -> tokio::task::JoinHandle<Result<(), pingers_delivery::DeliveryError>> {
        let (pusher, group, log) = (self.pusher.clone(), self.group, Arc::clone(log));
        let request = CommitRequest {
            epoch: Epoch::new(0),
            commit: MlsMessageBytes::from_bytes(vec![3; 8]),
            next_auth_key: GroupAuthKey::from_bytes([2; 32]),
            state: None,
            subscriptions_added: Vec::new(),
            subscriptions_removed: removed
                .iter()
                .map(|tag| SubscriptionId::from_bytes([*tag; 16]))
                .collect(),
            wake: Urgency::Quiet,
        };
        tokio::task::spawn_blocking(move || {
            let _outcome = pusher.commit(&group, request, &mut rng(2))?;
            record(&log, Event::Deleted);
            Ok(())
        })
    }

    /// Deletes the group as the DELETE route does, on the blocking pool, and logs
    /// [`Event::Deleted`] the moment it returns.
    fn delete_group(
        &self,
        log: &Log,
    ) -> tokio::task::JoinHandle<Result<(), pingers_delivery::DeliveryError>> {
        let (pusher, group, log) = (self.pusher.clone(), self.group, Arc::clone(log));
        tokio::task::spawn_blocking(move || {
            pusher.delete_group(&group)?;
            record(&log, Event::Deleted);
            Ok(())
        })
    }

    /// Starts sending the wake for `tag` and returns once the relay holds it, unanswered.
    async fn push_held_at_relay(
        &self,
        tag: u8,
        arrivals: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<tokio::task::JoinHandle<WakeOutcome>, TestError> {
        let (pusher, group, target, wake) = (
            self.pusher.clone(),
            self.group,
            self.target.clone(),
            self.wake(tag)?,
        );
        let sending = tokio::spawn(async move { pusher.send(group, wake, &target).await });
        tokio::time::timeout(STEP, arrivals.recv())
            .await?
            .ok_or("the relay stopped")?;
        Ok(sending)
    }
}

/// Waits until `count` hold or await the turn `waiting` reports.
async fn until_waiting(count: usize, waiting: impl Fn() -> usize) -> TestResult {
    tokio::time::timeout(STEP, async {
        while waiting() < count {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_removing_a_subscription_waits_for_its_push_in_flight() -> TestResult {
    let log = Log::default();
    let gate = Arc::new(Notify::new());
    let (address, mut arrivals) =
        relay(StatusCode::ACCEPTED, Some(Arc::clone(&gate)), &log).await?;
    let world = World::new(address)?;
    let sending = world.push_held_at_relay(1, &mut arrivals).await?;
    // Member 1 is removed while its push is at the relay: the commit must not be answered
    // before that push is.
    let committing = world.commit(&[1, 1], &log);
    until_waiting(2, || {
        world
            .pusher
            .waiting(world.group, SubscriptionId::from_bytes([1; 16]))
    })
    .await?;
    gate.notify_one();
    tokio::time::timeout(STEP, committing).await???;
    assert_eq!(
        tokio::time::timeout(STEP, sending).await??,
        WakeOutcome::Sent(Ok(RelayAnswer::Accepted))
    );
    assert_eq!(
        events(&log),
        vec![Event::Arrived(handle(1)), Event::Answered, Event::Deleted]
    );
    // The removed member is not woken again; the other still is.
    assert_eq!(world.send(1).await?, WakeOutcome::NotSubscribed);
    gate.notify_one();
    assert_eq!(
        world.send(2).await?,
        WakeOutcome::Sent(Ok(RelayAnswer::Accepted))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_waits_only_for_the_subscriptions_it_removes() -> TestResult {
    let log = Log::default();
    let gate = Arc::new(Notify::new());
    let (address, mut arrivals) =
        relay(StatusCode::ACCEPTED, Some(Arc::clone(&gate)), &log).await?;
    let world = World::new(address)?;
    let sending = world.push_held_at_relay(1, &mut arrivals).await?;
    // Removing member 2 does not wait for member 1's push, which is still at the relay.
    tokio::time::timeout(STEP, world.commit(&[2], &log)).await???;
    gate.notify_one();
    assert_eq!(
        tokio::time::timeout(STEP, sending).await??,
        WakeOutcome::Sent(Ok(RelayAnswer::Accepted))
    );
    assert_eq!(
        events(&log),
        vec![Event::Arrived(handle(1)), Event::Deleted, Event::Answered]
    );
    assert_eq!(world.send(2).await?, WakeOutcome::NotSubscribed);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_delete_waits_for_every_push_in_flight() -> TestResult {
    let log = Log::default();
    let gate = Arc::new(Notify::new());
    let (address, mut arrivals) =
        relay(StatusCode::ACCEPTED, Some(Arc::clone(&gate)), &log).await?;
    let world = World::new(address)?;
    let first = world.push_held_at_relay(1, &mut arrivals).await?;
    let second = world.push_held_at_relay(2, &mut arrivals).await?;
    let deleting = world.delete_group(&log);
    // Both pushes hold a share of the group's turn; the delete waits behind them.
    until_waiting(3, || world.pusher.group_waiting(world.group)).await?;
    gate.notify_one();
    gate.notify_one();
    tokio::time::timeout(STEP, deleting).await???;
    for sending in [first, second] {
        assert_eq!(
            tokio::time::timeout(STEP, sending).await??,
            WakeOutcome::Sent(Ok(RelayAnswer::Accepted))
        );
    }
    let seen = events(&log);
    assert_eq!(seen.last(), Some(&Event::Deleted));
    assert_eq!(
        seen.iter()
            .filter(|event| **event == Event::Answered)
            .count(),
        2
    );
    // Nothing is pushed for the deleted group afterwards.
    assert_eq!(world.send(1).await?, WakeOutcome::NotSubscribed);
    assert_eq!(world.send(2).await?, WakeOutcome::NotSubscribed);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wake_that_comes_due_after_its_subscription_was_deleted_is_not_sent() -> TestResult {
    let log = Log::default();
    let (address, _arrivals) = relay(StatusCode::ACCEPTED, None, &log).await?;
    let world = World::new(address)?;
    // The wakes were made (as a quiet one is, up to 30 s before it is sent); then member 1
    // deletes its subscription and is answered.
    world.delete(1).await??;
    assert_eq!(world.send(1).await?, WakeOutcome::NotSubscribed);
    assert_eq!(
        world.send(2).await?,
        WakeOutcome::Sent(Ok(RelayAnswer::Accepted))
    );
    assert_eq!(
        events(&log),
        vec![Event::Arrived(handle(2)), Event::Answered]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_a_410_a_wake_queued_behind_it_is_not_sent() -> TestResult {
    let log = Log::default();
    let (address, _arrivals) = relay(StatusCode::GONE, None, &log).await?;
    let world = World::new(address)?;
    // Two wakes for one subscription at once, as two alerts in quick succession make: the
    // first to go gets 410 and deletes the subscription before the second's turn comes.
    let (first, second) = tokio::join!(world.send(1), world.send(1));
    let mut outcomes = [first?, second?];
    outcomes.sort_by_key(|outcome| *outcome != WakeOutcome::NotSubscribed);
    assert_eq!(
        outcomes,
        [
            WakeOutcome::NotSubscribed,
            WakeOutcome::Sent(Ok(RelayAnswer::Gone))
        ]
    );
    assert_eq!(world.send(1).await?, WakeOutcome::NotSubscribed);
    assert_eq!(
        events(&log),
        vec![Event::Arrived(handle(1)), Event::Answered]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_is_answered_only_after_the_push_in_flight() -> TestResult {
    let log = Log::default();
    let gate = Arc::new(Notify::new());
    let (address, mut arrivals) =
        relay(StatusCode::ACCEPTED, Some(Arc::clone(&gate)), &log).await?;
    let world = World::new(address)?;
    let sending = {
        let (pusher, group, target, wake) = (
            world.pusher.clone(),
            world.group,
            world.target.clone(),
            world.wake(1)?,
        );
        tokio::spawn(async move { pusher.send(group, wake, &target).await })
    };
    tokio::time::timeout(STEP, arrivals.recv())
        .await?
        .ok_or("the relay stopped")?;
    // The push is at the relay, unanswered. The delete starts and must wait for it: hold the
    // relay until the delete is queued for the subscription's turn.
    let deleting = world.delete(1);
    tokio::time::timeout(STEP, async {
        while world
            .pusher
            .waiting(world.group, SubscriptionId::from_bytes([1; 16]))
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    gate.notify_one();
    tokio::time::timeout(STEP, deleting).await???;
    record(&log, Event::Deleted);
    assert_eq!(
        tokio::time::timeout(STEP, sending).await??,
        WakeOutcome::Sent(Ok(RelayAnswer::Accepted))
    );
    assert_eq!(
        events(&log),
        vec![Event::Arrived(handle(1)), Event::Answered, Event::Deleted]
    );
    assert_eq!(world.send(1).await?, WakeOutcome::NotSubscribed);
    Ok(())
}
