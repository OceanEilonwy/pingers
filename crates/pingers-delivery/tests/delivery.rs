// SPDX-License-Identifier: AGPL-3.0-only
//! The delivery service's rules, run against every store implementation.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};

use pingers_crypto::push_bind_for_subscription;
use pingers_delivery::{
    CommitOutcome, Delivery, DeliveryError, DeliveryStore, IDLE_LIFETIME, MAX_QUIET_DELAY,
    MAX_SUBSCRIPTION_LIFETIME, MemoryDeliveryStore, PAGE_ENTRIES, Wake,
};
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, ContentType, CreateGroup, Delivery as Mode, Epoch,
    GroupAuthKey, GroupId, LiveFrame, MlsMessageBytes, ProtoError, PushHandle, RelayId, RelayRef,
    SealedPush, SealedPushBlob, SealedSnapshot, SequenceNumber, Subscription, SubscriptionId,
    TimeToLive, Urgency,
};
use pingers_testkit::{ManualClock, seeded_rng, test_clock};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Delivery(#[from] DeliveryError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error("the commit unexpectedly conflicted")]
    Conflict,
    #[error("a committing thread panicked")]
    Thread,
}

type Live = u32;

struct World<S> {
    clock: ManualClock,
    delivery: Delivery<S, Live>,
    group: GroupId,
}

const SUBSCRIPTION_LIFETIME: Duration = Duration::from_hours(24);

fn subscription(tag: u8, lifetime: Duration) -> Result<Subscription, TestError> {
    Ok(Subscription {
        id: SubscriptionId::from_bytes([tag; 16]),
        push: SealedPush {
            relay: RelayRef::Listed(RelayId::new(1)),
            blob: SealedPushBlob::from_bytes([tag; 320]),
            urgency: Urgency::Quiet,
            handle: PushHandle::from_bytes([tag; 8]),
        },
        time_to_live: TimeToLive::new(lifetime)?,
    })
}

fn sub_id(tag: u8) -> SubscriptionId {
    SubscriptionId::from_bytes([tag; 16])
}

fn auth_key(tag: u8) -> GroupAuthKey {
    GroupAuthKey::from_bytes([tag; 32])
}

fn body(tag: u8) -> MlsMessageBytes {
    MlsMessageBytes::from_bytes(vec![tag; 3])
}

impl<S: DeliveryStore> World<S> {
    /// A group with the given subscriptions, created at the test start.
    fn new(store: S, subscriptions: &[u8]) -> Result<Self, TestError> {
        let clock = test_clock();
        let delivery = Delivery::new(store, Arc::new(clock.clone()));
        let group = GroupId::from_bytes([7; 32]);
        delivery.create_group(CreateGroup {
            group_id: group,
            auth_key: auth_key(0),
            subscriptions: subscriptions
                .iter()
                .map(|tag| subscription(*tag, SUBSCRIPTION_LIFETIME))
                .collect::<Result<_, _>>()?,
            state: Some(SealedSnapshot::from_bytes(vec![0xE0])),
        })?;
        Ok(Self {
            clock,
            delivery,
            group,
        })
    }

    fn commit(&self, epoch: u64, wake: Urgency) -> Result<CommitOutcome<Live>, TestError> {
        self.commit_with(commit_request(epoch, wake))
    }

    fn commit_with(&self, request: CommitRequest) -> Result<CommitOutcome<Live>, TestError> {
        Ok(self
            .delivery
            .commit(&self.group, request, &mut seeded_rng(1))?)
    }

    fn post(
        &self,
        epoch: u64,
        delivery: Mode,
        wake: Option<Urgency>,
    ) -> Result<pingers_delivery::Posted<Live>, TestError> {
        Ok(self.delivery.post(
            &self.group,
            AppMessage {
                epoch: Epoch::new(epoch),
                body: body(0xA0),
                delivery,
                wake,
            },
            &mut seeded_rng(2),
        )?)
    }

    fn log(&self) -> Result<Vec<(u64, u64, ContentType)>, TestError> {
        Ok(self
            .delivery
            .messages(&self.group, SequenceNumber::new(0))?
            .messages
            .iter()
            .map(|entry| (entry.sequence.get(), entry.epoch.get(), entry.content_type))
            .collect())
    }
}

fn commit_request(epoch: u64, wake: Urgency) -> CommitRequest {
    CommitRequest {
        epoch: Epoch::new(epoch),
        commit: body(0xC0),
        next_auth_key: auth_key(0xA0_u8.wrapping_add(u8::try_from(epoch % 64).unwrap_or(0))),
        state: None,
        subscriptions_added: Vec::new(),
        subscriptions_removed: Vec::new(),
        wake,
    }
}

fn woken(wakes: &[Wake]) -> BTreeSet<SubscriptionId> {
    wakes.iter().map(|wake| wake.subscription).collect()
}

fn accepted(outcome: CommitOutcome<Live>) -> Option<(SequenceNumber, Vec<Wake>, Vec<Live>)> {
    match outcome {
        CommitOutcome::Accepted {
            sequence,
            wakes,
            targets,
        } => Some((sequence, wakes, targets)),
        CommitOutcome::Conflict { .. } => None,
    }
}

// --- one commit per epoch ---

fn commit_in_head_epoch_is_accepted_and_moves_the_epoch(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let outcome = world.commit(0, Urgency::Quiet)?;
    assert_eq!(
        outcome.response(),
        CommitResponse::Accepted {
            sequence: SequenceNumber::new(1)
        }
    );
    let read = world
        .delivery
        .messages(&world.group, SequenceNumber::new(0))?;
    assert_eq!(read.head_epoch, Epoch::new(1));
    assert_eq!(world.delivery.auth_key(&world.group)?, auth_key(0xA0));
    Ok(())
}

fn second_commit_for_an_epoch_conflicts_and_changes_nothing(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let _first = world.commit(0, Urgency::Quiet)?;
    let mut loser = commit_request(0, Urgency::Quiet);
    loser.next_auth_key = auth_key(0xEE);
    loser.state = Some(SealedSnapshot::from_bytes(vec![0xEE]));
    let second = world.commit_with(loser)?;
    assert_eq!(
        second.response(),
        CommitResponse::Conflict {
            head_epoch: Epoch::new(1)
        }
    );
    assert_eq!(world.log()?, vec![(1, 0, ContentType::Commit)]);
    assert_eq!(world.delivery.auth_key(&world.group)?, auth_key(0xA0));
    let state = world.delivery.state(&world.group)?;
    assert_eq!(
        state.map(|state| state.state),
        Some(SealedSnapshot::from_bytes(vec![0xE0]))
    );
    Ok(())
}

fn commit_for_an_epoch_ahead_of_the_head_conflicts(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let outcome = world.commit(1, Urgency::Quiet)?;
    assert_eq!(
        outcome.response(),
        CommitResponse::Conflict {
            head_epoch: Epoch::new(0)
        }
    );
    assert_eq!(world.log()?, vec![]);
    Ok(())
}

fn concurrent_commits_for_one_epoch_have_exactly_one_winner<S: DeliveryStore + 'static>(
    store: S,
) -> Result<(), TestError> {
    const COMMITTERS: usize = 8;
    let world = Arc::new(World::new(store, &[])?);
    let barrier = Arc::new(Barrier::new(COMMITTERS));
    let handles: Vec<_> = (0..COMMITTERS)
        .map(|index| {
            let world = Arc::clone(&world);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut request = commit_request(0, Urgency::Quiet);
                request.next_auth_key = auth_key(u8::try_from(index).unwrap_or(0));
                barrier.wait();
                world.commit_with(request).map(|outcome| outcome.response())
            })
        })
        .collect();
    let mut responses = Vec::new();
    for handle in handles {
        responses.push(handle.join().map_err(|_| TestError::Thread)??);
    }
    let winners = responses
        .iter()
        .filter(|response| matches!(response, CommitResponse::Accepted { .. }))
        .count();
    assert_eq!(winners, 1);
    assert!(responses.iter().all(|response| match response {
        CommitResponse::Accepted { .. } => true,
        CommitResponse::Conflict { head_epoch } => *head_epoch == Epoch::new(1),
    }));
    assert_eq!(world.log()?, vec![(1, 0, ContentType::Commit)]);
    Ok(())
}

// --- the log ---

fn log_keeps_arrival_order_and_reads_after_a_position(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let first = world.post(0, Mode::Stored, None)?;
    let _commit = world.commit(0, Urgency::Quiet)?;
    let third = world.post(1, Mode::Stored, None)?;
    assert_eq!(first.sequence, Some(SequenceNumber::new(1)));
    assert_eq!(third.sequence, Some(SequenceNumber::new(3)));
    assert_eq!(
        world.log()?,
        vec![
            (1, 0, ContentType::Application),
            (2, 0, ContentType::Commit),
            (3, 1, ContentType::Application)
        ]
    );
    let after_first = world
        .delivery
        .messages(&world.group, SequenceNumber::new(1))?;
    let positions: Vec<u64> = after_first
        .messages
        .iter()
        .map(|entry| entry.sequence.get())
        .collect();
    assert_eq!(positions, vec![2, 3]);
    assert_eq!(
        after_first.messages.first().map(|entry| &entry.body),
        Some(&body(0xC0))
    );
    Ok(())
}

fn a_read_returns_at_most_one_page(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    for _ in 0..=PAGE_ENTRIES {
        let _posted = world.post(0, Mode::Stored, None)?;
    }
    let first = world
        .delivery
        .messages(&world.group, SequenceNumber::new(0))?;
    assert_eq!(first.messages.len(), PAGE_ENTRIES);
    let last = first.messages.last().map(|entry| entry.sequence);
    let rest = world
        .delivery
        .messages(&world.group, last.unwrap_or(SequenceNumber::new(0)))?;
    assert_eq!(rest.messages.len(), 1);
    Ok(())
}

fn message_from_an_epoch_ahead_of_the_head_is_rejected(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let result = world.post(1, Mode::Stored, None);
    assert!(matches!(
        result,
        Err(TestError::Delivery(DeliveryError::FutureEpoch { epoch, head_epoch }))
            if epoch == Epoch::new(1) && head_epoch == Epoch::new(0)
    ));
    assert_eq!(world.log()?, vec![]);
    Ok(())
}

fn message_from_an_earlier_epoch_is_stored(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let _commit = world.commit(0, Urgency::Quiet)?;
    let posted = world.post(0, Mode::Stored, None)?;
    assert_eq!(posted.sequence, Some(SequenceNumber::new(2)));
    Ok(())
}

fn ephemeral_message_is_forwarded_live_but_not_stored_or_pushed(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    let channel = world.delivery.open_live(&world.group, None, 40)?;
    let posted = world.post(0, Mode::Ephemeral, Some(Urgency::Alert))?;
    assert_eq!(posted.sequence, None);
    assert!(posted.wakes.is_empty());
    assert_eq!(posted.targets, vec![40]);
    assert_eq!(world.log()?, vec![]);
    world.delivery.close_live(&world.group, channel)?;
    Ok(())
}

// --- subscriptions and the wake rule ---

fn commit_wakes_every_subscription_with_the_requested_urgency(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1, 2])?;
    let (_, wakes, _) = accepted(world.commit(0, Urgency::Alert)?).ok_or(TestError::Conflict)?;
    assert_eq!(woken(&wakes), BTreeSet::from([sub_id(1), sub_id(2)]));
    for wake in &wakes {
        // Stored with Quiet; the commit asked for Alert, which wins and is never delayed.
        assert_eq!(wake.urgency, Urgency::Alert);
        assert_eq!(wake.delay, Duration::ZERO);
        assert_eq!(wake.bind, push_bind_for_subscription(&wake.subscription));
        assert_eq!(
            wake.push,
            subscription(wake.subscription.as_bytes()[0], SUBSCRIPTION_LIFETIME)?.push
        );
    }
    Ok(())
}

fn quiet_wakes_are_delayed_by_up_to_thirty_seconds(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let tags: Vec<u8> = (1..=40).collect();
    let world = World::new(store, &tags)?;
    let (_, wakes, _) = accepted(world.commit(0, Urgency::Quiet)?).ok_or(TestError::Conflict)?;
    assert_eq!(wakes.len(), 40);
    assert!(wakes.iter().all(|wake| wake.urgency == Urgency::Quiet));
    assert!(wakes.iter().all(|wake| wake.delay <= MAX_QUIET_DELAY));
    let distinct: BTreeSet<Duration> = wakes.iter().map(|wake| wake.delay).collect();
    assert!(
        distinct.len() > 10,
        "delays should spread over 0..=30 s: {distinct:?}"
    );
    assert!(
        distinct
            .iter()
            .any(|delay| *delay > Duration::from_secs(20))
    );
    Ok(())
}

fn subscriptions_on_a_live_channel_are_not_pushed(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1, 2])?;
    let _channel = world
        .delivery
        .open_live(&world.group, Some(sub_id(1)), 10)?;
    let (_, wakes, targets) =
        accepted(world.commit(0, Urgency::Alert)?).ok_or(TestError::Conflict)?;
    assert_eq!(woken(&wakes), BTreeSet::from([sub_id(2)]));
    assert_eq!(targets, vec![10]);
    Ok(())
}

fn stored_message_wakes_only_when_asked(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    let silent = world.post(0, Mode::Stored, None)?;
    assert!(silent.wakes.is_empty());
    let loud = world.post(0, Mode::Stored, Some(Urgency::Alert))?;
    assert_eq!(woken(&loud.wakes), BTreeSet::from([sub_id(1)]));
    Ok(())
}

fn commit_applies_subscription_changes_before_waking(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1, 2])?;
    let mut request = commit_request(0, Urgency::Alert);
    request.subscriptions_added = vec![subscription(3, SUBSCRIPTION_LIFETIME)?];
    request.subscriptions_removed = vec![sub_id(1)];
    let (_, wakes, _) = accepted(world.commit_with(request)?).ok_or(TestError::Conflict)?;
    assert_eq!(woken(&wakes), BTreeSet::from([sub_id(2), sub_id(3)]));
    Ok(())
}

fn losing_commit_applies_no_subscription_changes(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    let _winner = world.commit(0, Urgency::Quiet)?;
    let mut loser = commit_request(0, Urgency::Alert);
    loser.subscriptions_added = vec![subscription(3, SUBSCRIPTION_LIFETIME)?];
    loser.subscriptions_removed = vec![sub_id(1)];
    assert!(accepted(world.commit_with(loser)?).is_none());
    let posted = world.post(1, Mode::Stored, Some(Urgency::Alert))?;
    assert_eq!(woken(&posted.wakes), BTreeSet::from([sub_id(1)]));
    Ok(())
}

fn expired_subscription_is_not_woken_and_is_deleted(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    world
        .delivery
        .put_subscription(&world.group, subscription(2, Duration::from_secs(60))?)?;
    world.clock.advance(Duration::from_secs(59));
    let before = world.post(0, Mode::Stored, Some(Urgency::Alert))?;
    assert_eq!(woken(&before.wakes), BTreeSet::from([sub_id(1), sub_id(2)]));
    world.clock.advance(Duration::from_secs(1));
    let after = world.post(0, Mode::Stored, Some(Urgency::Alert))?;
    assert_eq!(woken(&after.wakes), BTreeSet::from([sub_id(1)]));
    assert_eq!(world.delivery.expire_idle()?, vec![]);
    let stored = world
        .delivery
        .store()
        .live_subscriptions(&world.group, pingers_testkit::TEST_START)?;
    assert_eq!(
        stored.len(),
        1,
        "the expired subscription is deleted, not just skipped"
    );
    Ok(())
}

fn subscription_over_thirty_days_is_refused(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let longest = world
        .delivery
        .put_subscription(&world.group, subscription(1, MAX_SUBSCRIPTION_LIFETIME)?);
    assert!(longest.is_ok());
    let too_long = MAX_SUBSCRIPTION_LIFETIME.saturating_add(Duration::from_secs(1));
    let refused = world
        .delivery
        .put_subscription(&world.group, subscription(2, too_long)?);
    assert!(matches!(refused, Err(DeliveryError::SubscriptionTooLong)));
    let mut in_commit = commit_request(0, Urgency::Quiet);
    in_commit.subscriptions_added = vec![subscription(3, too_long)?];
    assert!(matches!(
        world
            .delivery
            .commit(&world.group, in_commit, &mut seeded_rng(3)),
        Err(DeliveryError::SubscriptionTooLong)
    ));
    Ok(())
}

fn renewed_subscription_replaces_the_old_one(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    let mut renewed = subscription(1, SUBSCRIPTION_LIFETIME.saturating_mul(2))?;
    renewed.push.handle = PushHandle::from_bytes([0xBB; 8]);
    world.delivery.put_subscription(&world.group, renewed)?;
    world.clock.advance(SUBSCRIPTION_LIFETIME);
    let posted = world.post(0, Mode::Stored, Some(Urgency::Alert))?;
    assert_eq!(posted.wakes.len(), 1);
    assert_eq!(
        posted.wakes.first().map(|wake| wake.push.handle),
        Some(PushHandle::from_bytes([0xBB; 8]))
    );
    Ok(())
}

fn removed_subscription_is_not_woken_and_removal_is_idempotent(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1, 2])?;
    world
        .delivery
        .remove_subscription(&world.group, &sub_id(1))?;
    world
        .delivery
        .remove_subscription(&world.group, &sub_id(1))?;
    let posted = world.post(0, Mode::Stored, Some(Urgency::Alert))?;
    assert_eq!(woken(&posted.wakes), BTreeSet::from([sub_id(2)]));
    Ok(())
}

// --- live fan-out ---

fn live_frame_goes_to_the_other_channels(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let sender = world.delivery.open_live(&world.group, None, 1)?;
    let _second = world.delivery.open_live(&world.group, None, 2)?;
    let _third = world.delivery.open_live(&world.group, None, 3)?;
    let location = world.delivery.live_frame(
        &world.group,
        sender,
        LiveFrame {
            epoch: Epoch::new(0),
            body: body(1),
            delivery: Mode::Ephemeral,
        },
    )?;
    assert_eq!(location.sequence, None);
    assert_eq!(location.targets, vec![2, 3]);
    let chat = world.delivery.live_frame(
        &world.group,
        sender,
        LiveFrame {
            epoch: Epoch::new(0),
            body: body(2),
            delivery: Mode::Stored,
        },
    )?;
    assert_eq!(chat.sequence, Some(SequenceNumber::new(1)));
    assert_eq!(world.log()?, vec![(1, 0, ContentType::Application)]);
    Ok(())
}

fn closed_channel_gets_nothing_and_cannot_send(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let first = world.delivery.open_live(&world.group, None, 1)?;
    let second = world.delivery.open_live(&world.group, None, 2)?;
    world.delivery.close_live(&world.group, first)?;
    assert_eq!(world.delivery.live_channels(&world.group)?, 1);
    let posted = world.post(0, Mode::Ephemeral, None)?;
    assert_eq!(posted.targets, vec![2]);
    let frame = LiveFrame {
        epoch: Epoch::new(0),
        body: body(1),
        delivery: Mode::Ephemeral,
    };
    assert!(matches!(
        world.delivery.live_frame(&world.group, first, frame),
        Err(DeliveryError::UnknownChannel)
    ));
    assert!(matches!(
        world.delivery.close_live(&world.group, first),
        Err(DeliveryError::UnknownChannel)
    ));
    world.delivery.close_live(&world.group, second)?;
    assert_eq!(world.delivery.live_channels(&world.group)?, 0);
    Ok(())
}

fn live_channel_needs_an_existing_group(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let other = GroupId::from_bytes([9; 32]);
    assert!(matches!(
        world.delivery.open_live(&other, None, 1),
        Err(DeliveryError::UnknownGroup)
    ));
    Ok(())
}

// --- expiry and deletion ---

fn idle_group_expires_after_two_hours(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    let _channel = world.delivery.open_live(&world.group, Some(sub_id(1)), 5)?;
    world
        .clock
        .advance(IDLE_LIFETIME.saturating_sub(Duration::from_secs(1)));
    assert_eq!(world.delivery.expire_idle()?, vec![]);
    world.clock.advance(Duration::from_secs(1));
    assert_eq!(world.delivery.expire_idle()?, vec![world.group]);
    assert_eq!(world.delivery.live_channels(&world.group)?, 0);
    assert!(matches!(
        world
            .delivery
            .messages(&world.group, SequenceNumber::new(0)),
        Err(DeliveryError::UnknownGroup)
    ));
    Ok(())
}

fn writes_postpone_expiry(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    world.clock.advance(Duration::from_hours(1));
    let _posted = world.post(0, Mode::Ephemeral, None)?;
    world.clock.advance(Duration::from_hours(1));
    let _commit = world.commit(0, Urgency::Quiet)?;
    world.clock.advance(Duration::from_hours(1));
    world
        .delivery
        .put_subscription(&world.group, subscription(1, SUBSCRIPTION_LIFETIME)?)?;
    world.clock.advance(Duration::from_hours(1));
    let _stored = world.post(1, Mode::Stored, None)?;
    world
        .clock
        .advance(IDLE_LIFETIME.saturating_sub(Duration::from_secs(1)));
    assert_eq!(world.delivery.expire_idle()?, vec![]);
    world.clock.advance(Duration::from_secs(1));
    assert_eq!(world.delivery.expire_idle()?, vec![world.group]);
    Ok(())
}

fn reads_and_live_channels_postpone_expiry(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    world.clock.advance(Duration::from_hours(1));
    let _read = world
        .delivery
        .messages(&world.group, SequenceNumber::new(0))?;
    world.clock.advance(Duration::from_hours(1));
    let _state = world.delivery.state(&world.group)?;
    world.clock.advance(Duration::from_hours(1));
    let channel = world.delivery.open_live(&world.group, None, 1)?;
    world.clock.advance(Duration::from_hours(1));
    let _frame = world.delivery.live_frame(
        &world.group,
        channel,
        LiveFrame {
            epoch: Epoch::new(0),
            body: body(1),
            delivery: Mode::Ephemeral,
        },
    )?;
    world
        .clock
        .advance(IDLE_LIFETIME.saturating_sub(Duration::from_secs(1)));
    assert_eq!(world.delivery.expire_idle()?, vec![]);
    world.clock.advance(Duration::from_secs(1));
    assert_eq!(world.delivery.expire_idle()?, vec![world.group]);
    Ok(())
}

fn delete_removes_the_group_and_closes_its_channels(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    let _channel = world.delivery.open_live(&world.group, None, 1)?;
    world.delivery.delete_group(&world.group)?;
    assert_eq!(world.delivery.live_channels(&world.group)?, 0);
    assert!(matches!(
        world.delivery.state(&world.group),
        Err(DeliveryError::UnknownGroup)
    ));
    assert!(matches!(
        world.delivery.delete_group(&world.group),
        Err(DeliveryError::UnknownGroup)
    ));
    // The id is free again, with nothing left over from the old group.
    world.delivery.create_group(CreateGroup {
        group_id: world.group,
        auth_key: auth_key(1),
        subscriptions: Vec::new(),
        state: None,
    })?;
    assert_eq!(world.log()?, vec![]);
    assert_eq!(world.delivery.state(&world.group)?, None);
    let posted = world.post(0, Mode::Stored, Some(Urgency::Alert))?;
    assert!(posted.wakes.is_empty());
    Ok(())
}

fn expiry_leaves_other_groups_alone(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    world.clock.advance(Duration::from_hours(1));
    let other = GroupId::from_bytes([8; 32]);
    world.delivery.create_group(CreateGroup {
        group_id: other,
        auth_key: auth_key(1),
        subscriptions: vec![subscription(2, SUBSCRIPTION_LIFETIME)?],
        state: None,
    })?;
    world.clock.advance(Duration::from_hours(1));
    assert_eq!(world.delivery.expire_idle()?, vec![world.group]);
    assert_eq!(world.delivery.auth_key(&other)?, auth_key(1));
    let stored = world
        .delivery
        .store()
        .live_subscriptions(&other, world_now(&world))?;
    assert_eq!(stored.len(), 1);
    Ok(())
}

fn world_now<S>(world: &World<S>) -> pingers_time::UnixSeconds {
    pingers_time::Clock::now(&world.clock)
}

// --- creation, snapshots, auth keys ---

fn duplicate_group_is_refused(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let again = world.delivery.create_group(CreateGroup {
        group_id: world.group,
        auth_key: auth_key(9),
        subscriptions: Vec::new(),
        state: None,
    });
    assert!(matches!(again, Err(DeliveryError::GroupExists)));
    assert_eq!(world.delivery.auth_key(&world.group)?, auth_key(0));
    Ok(())
}

fn commit_snapshot_replaces_the_state_for_the_next_epoch(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let first = world.delivery.state(&world.group)?;
    assert_eq!(first.as_ref().map(|state| state.epoch), Some(Epoch::new(0)));
    let _without = world.commit(0, Urgency::Quiet)?;
    let kept = world.delivery.state(&world.group)?;
    assert_eq!(kept, first, "a commit without a snapshot keeps the old one");
    let mut request = commit_request(1, Urgency::Quiet);
    request.state = Some(SealedSnapshot::from_bytes(vec![0xE2]));
    let _with = world.commit_with(request)?;
    let replaced = world.delivery.state(&world.group)?;
    assert_eq!(
        replaced.as_ref().map(|state| state.epoch),
        Some(Epoch::new(2))
    );
    assert_eq!(
        replaced.map(|state| state.state),
        Some(SealedSnapshot::from_bytes(vec![0xE2]))
    );
    Ok(())
}

fn unknown_group_is_reported_everywhere(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let other = GroupId::from_bytes([9; 32]);
    let unknown =
        |result: Result<(), DeliveryError>| matches!(result, Err(DeliveryError::UnknownGroup));
    let delivery = &world.delivery;
    assert!(unknown(delivery.auth_key(&other).map(drop)));
    assert!(unknown(delivery.state(&other).map(drop)));
    assert!(unknown(
        delivery.messages(&other, SequenceNumber::new(0)).map(drop)
    ));
    assert!(unknown(
        delivery
            .commit(
                &other,
                commit_request(0, Urgency::Quiet),
                &mut seeded_rng(4)
            )
            .map(drop)
    ));
    assert!(unknown(
        delivery
            .post(
                &other,
                AppMessage {
                    epoch: Epoch::new(0),
                    body: body(1),
                    delivery: Mode::Ephemeral,
                    wake: None
                },
                &mut seeded_rng(5)
            )
            .map(drop)
    ));
    assert!(unknown(delivery.put_subscription(
        &other,
        subscription(1, SUBSCRIPTION_LIFETIME)?
    )));
    assert!(unknown(delivery.remove_subscription(&other, &sub_id(1))));
    Ok(())
}

macro_rules! for_each_store {
    ($($test:ident),* $(,)?) => {
        mod memory {
            use super::*;
            $(
                #[test]
                fn $test() -> Result<(), TestError> {
                    super::$test(MemoryDeliveryStore::new())
                }
            )*
        }
        #[cfg(feature = "sqlite")]
        mod sqlite {
            use super::*;
            $(
                #[test]
                fn $test() -> Result<(), TestError> {
                    super::$test(pingers_delivery::SqliteDeliveryStore::open_in_memory()?)
                }
            )*
        }
    };
}

for_each_store!(
    commit_in_head_epoch_is_accepted_and_moves_the_epoch,
    second_commit_for_an_epoch_conflicts_and_changes_nothing,
    commit_for_an_epoch_ahead_of_the_head_conflicts,
    concurrent_commits_for_one_epoch_have_exactly_one_winner,
    log_keeps_arrival_order_and_reads_after_a_position,
    a_read_returns_at_most_one_page,
    message_from_an_epoch_ahead_of_the_head_is_rejected,
    message_from_an_earlier_epoch_is_stored,
    ephemeral_message_is_forwarded_live_but_not_stored_or_pushed,
    commit_wakes_every_subscription_with_the_requested_urgency,
    quiet_wakes_are_delayed_by_up_to_thirty_seconds,
    subscriptions_on_a_live_channel_are_not_pushed,
    stored_message_wakes_only_when_asked,
    commit_applies_subscription_changes_before_waking,
    losing_commit_applies_no_subscription_changes,
    expired_subscription_is_not_woken_and_is_deleted,
    subscription_over_thirty_days_is_refused,
    renewed_subscription_replaces_the_old_one,
    removed_subscription_is_not_woken_and_removal_is_idempotent,
    live_frame_goes_to_the_other_channels,
    closed_channel_gets_nothing_and_cannot_send,
    live_channel_needs_an_existing_group,
    idle_group_expires_after_two_hours,
    writes_postpone_expiry,
    reads_and_live_channels_postpone_expiry,
    delete_removes_the_group_and_closes_its_channels,
    expiry_leaves_other_groups_alone,
    duplicate_group_is_refused,
    commit_snapshot_replaces_the_state_for_the_next_epoch,
    unknown_group_is_reported_everywhere,
);

/// A file-backed store keeps everything across a restart.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_store_survives_reopening() -> Result<(), TestError> {
    use pingers_delivery::SqliteDeliveryStore;

    let path = std::env::temp_dir().join(format!(
        "pingers-delivery-reopen-{}.sqlite",
        std::process::id()
    ));
    let _stale = std::fs::remove_file(&path);
    {
        let world = World::new(SqliteDeliveryStore::open(&path)?, &[1])?;
        let _commit = world.commit(0, Urgency::Quiet)?;
    }
    let clock: ManualClock = test_clock();
    let reopened: Delivery<_, Live> =
        Delivery::new(SqliteDeliveryStore::open(&path)?, Arc::new(clock));
    let group = GroupId::from_bytes([7; 32]);
    let read = reopened.messages(&group, SequenceNumber::new(0))?;
    assert_eq!(read.head_epoch, Epoch::new(1));
    assert_eq!(read.messages.len(), 1);
    let posted = reopened.post(
        &group,
        AppMessage {
            epoch: Epoch::new(1),
            body: body(1),
            delivery: Mode::Stored,
            wake: Some(Urgency::Alert),
        },
        &mut seeded_rng(6),
    )?;
    assert_eq!(posted.sequence, Some(SequenceNumber::new(2)));
    assert_eq!(woken(&posted.wakes), BTreeSet::from([sub_id(1)]));
    drop(reopened);
    let _cleanup = std::fs::remove_file(&path);
    Ok(())
}
