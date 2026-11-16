// SPDX-License-Identifier: AGPL-3.0-only
//! The delivery service's rules, run against every store implementation.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};

use pingers_crypto::push_bind_for_subscription;
use pingers_delivery::{
    CommitOutcome, Delivery, DeliveryError, DeliveryStore, GroupLifetime, GroupStart, LiveAttach,
    MAX_LIVE_CHANNELS_PER_GROUP, MAX_QUIET_DELAY, MAX_SUBSCRIPTION_LIFETIME,
    MAX_SUBSCRIPTIONS_PER_GROUP, MEETUP_IDLE_LIFETIME, MemoryDeliveryStore, PAGE_ENTRIES,
    PARTY_IDLE_LIFETIME, PastAuthKey, Reader, STORED_LIVE_FRAMES_PER_BUCKET, Wake,
};
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, ContentType, CreateGroup, Delivery as Mode, Epoch,
    GroupAuthKey, GroupId, LiveFrame, MlsMessageBytes, ProtoError, PushHandle, PushRelayKeyId,
    RelayId, RelayRef, SealedPush, SealedPushBlob, SealedSnapshot, SequenceNumber, Subscription,
    SubscriptionId, SubscriptionOwner, TimeToLive, Urgency,
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
    #[error("the commit made no wake for that subscription")]
    NoWake,
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
            key_id: PushRelayKeyId::new(1),
            blob: SealedPushBlob::from_bytes([tag; 320]),
            urgency: Urgency::Quiet,
            handle: PushHandle::from_bytes([tag; 8]),
        },
        time_to_live: TimeToLive::new(lifetime)?,
        owner: SubscriptionOwner::from_bytes([tag; 32]),
    })
}

/// A group whose log starts at epoch 0.
fn at_zero(lifetime: GroupLifetime) -> GroupStart {
    GroupStart {
        lifetime,
        epoch: Epoch::new(0),
    }
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
        delivery.create_group(
            CreateGroup {
                group_id: group,
                auth_key: auth_key(0),
                subscriptions: subscriptions
                    .iter()
                    .map(|tag| subscription(*tag, SUBSCRIPTION_LIFETIME))
                    .collect::<Result<_, _>>()?,
                state: Some(SealedSnapshot::from_bytes(vec![0xE0])),
            },
            at_zero(GroupLifetime::Meetup),
        )?;
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
            .messages(&self.group, SequenceNumber::new(0), Reader::Current)?
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
        .messages(&world.group, SequenceNumber::new(0), Reader::Current)?;
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
    let state = world.delivery.state(&world.group, Reader::Current)?;
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
    let after_first =
        world
            .delivery
            .messages(&world.group, SequenceNumber::new(1), Reader::Current)?;
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
        .messages(&world.group, SequenceNumber::new(0), Reader::Current)?;
    assert_eq!(first.messages.len(), PAGE_ENTRIES);
    let last = first.messages.last().map(|entry| entry.sequence);
    let rest = world.delivery.messages(
        &world.group,
        last.unwrap_or(SequenceNumber::new(0)),
        Reader::Current,
    )?;
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
    let channel = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        40,
    )?;
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
    let _channel = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: Some(sub_id(1)),
        },
        10,
    )?;
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

/// A wake made earlier is sent only if, when its turn comes, the very subscription it was
/// made for still stands: not one since removed, replaced with a new sealed push, expired, or
/// in a deleted group.
fn a_wake_is_due_only_while_its_subscription_stands(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1, 2, 3])?;
    world
        .delivery
        .put_subscription(&world.group, subscription(4, Duration::from_secs(60))?)?;
    let (_, wakes, _) = accepted(world.commit(0, Urgency::Quiet)?).ok_or(TestError::Conflict)?;
    let due = |tag: u8| -> Result<bool, TestError> {
        let wake = wakes
            .iter()
            .find(|wake| wake.subscription == sub_id(tag))
            .ok_or(TestError::NoWake)?;
        Ok(world.delivery.still_subscribed(&world.group, wake)?)
    };
    assert!(due(1)? && due(2)? && due(3)? && due(4)?);
    world
        .delivery
        .remove_subscription(&world.group, &sub_id(1))?;
    let mut renewed = subscription(2, SUBSCRIPTION_LIFETIME)?;
    renewed.push.handle = PushHandle::from_bytes([0xBB; 8]);
    world.delivery.put_subscription(&world.group, renewed)?;
    world.clock.advance(Duration::from_secs(60));
    assert!(!due(1)?, "removed");
    assert!(!due(2)?, "replaced by a new sealed push");
    assert!(!due(4)?, "expired");
    assert!(due(3)?, "untouched");
    world.delivery.delete_group(&world.group)?;
    assert!(!due(3)?, "its group was deleted");
    Ok(())
}

// --- live fan-out ---

fn live_frame_goes_to_the_other_channels(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let sender = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        1,
    )?;
    let _second = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        2,
    )?;
    let _third = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        3,
    )?;
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
    let first = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        1,
    )?;
    let second = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        2,
    )?;
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

fn a_removal_closes_the_removed_members_live_channel(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1, 2])?;
    let _won = world.commit(0, Urgency::Quiet)?;
    let removed = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(1),
            subscription: Some(sub_id(1)),
        },
        1,
    )?;
    let _staying = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(1),
            subscription: Some(sub_id(2)),
        },
        2,
    )?;
    let _anonymous = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(1),
            subscription: None,
        },
        3,
    )?;
    // A losing commit closes nothing.
    let lost = world.commit_with(CommitRequest {
        subscriptions_removed: vec![sub_id(1)],
        ..commit_request(0, Urgency::Quiet)
    })?;
    assert!(accepted(lost).is_none());
    assert_eq!(world.delivery.live_channels(&world.group)?, 3);
    let outcome = world.commit_with(CommitRequest {
        subscriptions_removed: vec![sub_id(1)],
        ..commit_request(1, Urgency::Quiet)
    })?;
    let (_, wakes, targets) = accepted(outcome).ok_or(TestError::Conflict)?;
    // The commit goes to the others only, as the last frame of their epoch's channels; the
    // ones that got it are not pushed as well, and the removed member can send nothing more.
    assert_eq!(targets, vec![2, 3]);
    assert!(wakes.is_empty());
    assert_eq!(world.delivery.live_channels(&world.group)?, 0);
    let frame = LiveFrame {
        epoch: Epoch::new(1),
        body: body(1),
        delivery: Mode::Stored,
    };
    assert!(matches!(
        world.delivery.live_frame(&world.group, removed, frame),
        Err(DeliveryError::UnknownChannel)
    ));
    Ok(())
}

/// A member removed by a commit that does not name its subscription (it attached without
/// `?sub=`, or under a made-up one) loses its channel with the epoch: the commit is its last
/// frame, it cannot inject frames afterwards, and it cannot keep the group alive.
fn a_removed_members_channel_ends_with_its_epoch(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[2])?;
    let removed = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        1,
    )?;
    let staying = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        2,
    )?;
    let (_, _, targets) = accepted(world.commit(0, Urgency::Quiet)?).ok_or(TestError::Conflict)?;
    assert_eq!(targets, vec![1, 2]);
    assert_eq!(world.delivery.live_channels(&world.group)?, 0);
    let frame = |delivery| LiveFrame {
        epoch: Epoch::new(1),
        body: body(1),
        delivery,
    };
    for mode in [Mode::Stored, Mode::Ephemeral] {
        assert!(matches!(
            world
                .delivery
                .live_frame(&world.group, removed, frame(mode)),
            Err(DeliveryError::UnknownChannel)
        ));
    }
    assert!(matches!(
        world
            .delivery
            .live_frame(&world.group, staying, frame(Mode::Ephemeral)),
        Err(DeliveryError::UnknownChannel)
    ));
    assert_eq!(world.log()?, vec![(1, 0, ContentType::Commit)]);
    // Attaching again needs the next epoch's key: the old one's epoch has ended.
    assert!(matches!(
        world.delivery.open_live(
            &world.group,
            LiveAttach {
                epoch: Epoch::new(0),
                subscription: None
            },
            1
        ),
        Err(DeliveryError::EpochEnded)
    ));
    // Its frames kept nothing alive: the group expires an idle lifetime after the commit.
    world.clock.advance(MEETUP_IDLE_LIFETIME);
    assert_eq!(world.delivery.expire_idle()?, vec![world.group]);
    Ok(())
}

// --- limits ---

/// A subscription whose id is `index` big-endian, for groups with more than 256.
fn numbered(index: u16) -> Result<Subscription, TestError> {
    let mut id = [0_u8; 16];
    for (target, source) in id.iter_mut().zip(index.to_be_bytes()) {
        *target = source;
    }
    Ok(Subscription {
        id: SubscriptionId::from_bytes(id),
        ..subscription(1, SUBSCRIPTION_LIFETIME)?
    })
}

fn numbered_range(range: core::ops::Range<u16>) -> Result<Vec<Subscription>, TestError> {
    range.map(numbered).collect()
}

fn a_group_holds_a_bounded_number_of_subscriptions(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let max = u16::try_from(MAX_SUBSCRIPTIONS_PER_GROUP).map_err(|_| TestError::Thread)?;
    let clock = test_clock();
    let delivery: Delivery<_, Live> = Delivery::new(store, Arc::new(clock));
    let create = |id: u8, subscriptions| {
        delivery.create_group(
            CreateGroup {
                group_id: GroupId::from_bytes([id; 32]),
                auth_key: auth_key(0),
                subscriptions,
                state: None,
            },
            at_zero(GroupLifetime::Party),
        )
    };
    assert!(matches!(
        create(1, numbered_range(0..max.saturating_add(1))?),
        Err(DeliveryError::TooManySubscriptions)
    ));
    create(2, numbered_range(0..max)?)?;
    let group = GroupId::from_bytes([2; 32]);
    assert!(matches!(
        delivery.put_subscription(&group, numbered(max)?),
        Err(DeliveryError::TooManySubscriptions)
    ));
    // Renewing one already there is not a new one.
    delivery.put_subscription(&group, numbered(0)?)?;
    // A commit that adds one more is refused whole; one that also removes one is not.
    let adding = CommitRequest {
        subscriptions_added: vec![numbered(max)?],
        ..commit_request(0, Urgency::Quiet)
    };
    assert!(matches!(
        delivery.commit(&group, adding.clone(), &mut seeded_rng(1)),
        Err(DeliveryError::TooManySubscriptions)
    ));
    assert_eq!(delivery.head_epoch(&group, Reader::Current)?, Epoch::new(0));
    let swapping = CommitRequest {
        subscriptions_removed: vec![numbered(0)?.id],
        ..adding
    };
    let outcome = delivery.commit(&group, swapping, &mut seeded_rng(1))?;
    assert!(matches!(outcome, CommitOutcome::Accepted { .. }));
    Ok(())
}

fn a_group_has_a_bounded_number_of_live_channels(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let mut channels = Vec::new();
    for handle in 0..MAX_LIVE_CHANNELS_PER_GROUP {
        let handle = u32::try_from(handle).map_err(|_| TestError::Thread)?;
        channels.push(world.delivery.open_live(
            &world.group,
            LiveAttach {
                epoch: Epoch::new(0),
                subscription: None,
            },
            handle,
        )?);
    }
    assert!(matches!(
        world.delivery.open_live(
            &world.group,
            LiveAttach {
                epoch: Epoch::new(0),
                subscription: None
            },
            9_999
        ),
        Err(DeliveryError::TooManyChannels)
    ));
    let closed = channels.pop().ok_or(TestError::Thread)?;
    world.delivery.close_live(&world.group, closed)?;
    world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        9_999,
    )?;
    Ok(())
}

fn a_live_channel_stores_a_bounded_number_of_frames_per_bucket(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let channel = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        1,
    )?;
    let frame = |delivery| LiveFrame {
        epoch: Epoch::new(0),
        body: body(1),
        delivery,
    };
    // Start at a bucket boundary, so the whole allowance falls in one bucket.
    let next = world_now(&world).bucket().next().ok_or(TestError::Thread)?;
    world
        .clock
        .advance(next.start().saturating_duration_since(world_now(&world)));
    for _ in 0..STORED_LIVE_FRAMES_PER_BUCKET {
        let _ = world
            .delivery
            .live_frame(&world.group, channel, frame(Mode::Stored))?;
    }
    let before = world.log()?.len();
    assert!(matches!(
        world
            .delivery
            .live_frame(&world.group, channel, frame(Mode::Stored)),
        Err(DeliveryError::LiveRateLimited)
    ));
    assert_eq!(world.log()?.len(), before);
    // Ephemeral frames are not counted; the next bucket starts afresh.
    let _ = world
        .delivery
        .live_frame(&world.group, channel, frame(Mode::Ephemeral))?;
    world.clock.advance(pingers_time::Bucket::LENGTH);
    let _ = world
        .delivery
        .live_frame(&world.group, channel, frame(Mode::Stored))?;
    Ok(())
}

fn live_channel_needs_an_existing_group(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let other = GroupId::from_bytes([9; 32]);
    assert!(matches!(
        world.delivery.open_live(
            &other,
            LiveAttach {
                epoch: Epoch::new(0),
                subscription: None
            },
            1
        ),
        Err(DeliveryError::UnknownGroup)
    ));
    Ok(())
}

// --- expiry and deletion ---

fn idle_group_expires_after_two_hours(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    let _channel = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: Some(sub_id(1)),
        },
        5,
    )?;
    world
        .clock
        .advance(MEETUP_IDLE_LIFETIME.saturating_sub(Duration::from_secs(1)));
    assert_eq!(world.delivery.expire_idle()?, vec![]);
    world.clock.advance(Duration::from_secs(1));
    assert_eq!(world.delivery.expire_idle()?, vec![world.group]);
    assert_eq!(world.delivery.live_channels(&world.group)?, 0);
    assert!(matches!(
        world
            .delivery
            .messages(&world.group, SequenceNumber::new(0), Reader::Current),
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
        .advance(MEETUP_IDLE_LIFETIME.saturating_sub(Duration::from_secs(1)));
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
        .messages(&world.group, SequenceNumber::new(0), Reader::Current)?;
    world.clock.advance(Duration::from_hours(1));
    let _state = world.delivery.state(&world.group, Reader::Current)?;
    world.clock.advance(Duration::from_hours(1));
    let channel = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        1,
    )?;
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
        .advance(MEETUP_IDLE_LIFETIME.saturating_sub(Duration::from_secs(1)));
    assert_eq!(world.delivery.expire_idle()?, vec![]);
    world.clock.advance(Duration::from_secs(1));
    assert_eq!(world.delivery.expire_idle()?, vec![world.group]);
    Ok(())
}

fn party_groups_stay_idle_for_half_a_year(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    world.clock.advance(MEETUP_IDLE_LIFETIME);
    assert_eq!(world.delivery.expire_idle()?, vec![world.group]);
    let party = GroupId::from_bytes([8; 32]);
    world.delivery.create_group(
        CreateGroup {
            group_id: party,
            auth_key: auth_key(0),
            subscriptions: Vec::new(),
            state: None,
        },
        at_zero(GroupLifetime::Party),
    )?;
    let almost = PARTY_IDLE_LIFETIME.saturating_sub(Duration::from_secs(1));
    // Every kind of activity renews a Party group by its own lifetime, not a Meetup's.
    let activities: [&dyn Fn() -> Result<(), TestError>; 6] = [
        &|| {
            Ok(world
                .delivery
                .messages(&party, SequenceNumber::new(0), Reader::Current)
                .map(drop)?)
        },
        &|| Ok(world.delivery.state(&party, Reader::Current).map(drop)?),
        &|| {
            Ok(world
                .delivery
                .put_subscription(&party, subscription(1, SUBSCRIPTION_LIFETIME)?)?)
        },
        &|| {
            let outcome = world.delivery.commit(
                &party,
                commit_request(0, Urgency::Quiet),
                &mut seeded_rng(3),
            )?;
            assert!(matches!(outcome, CommitOutcome::Accepted { .. }));
            Ok(())
        },
        &|| {
            let message = AppMessage {
                epoch: Epoch::new(1),
                body: body(1),
                delivery: Mode::Stored,
                wake: None,
            };
            Ok(world
                .delivery
                .post(&party, message, &mut seeded_rng(4))
                .map(drop)?)
        },
        &|| {
            Ok(world
                .delivery
                .open_live(
                    &party,
                    LiveAttach {
                        epoch: Epoch::new(1),
                        subscription: None,
                    },
                    1,
                )
                .map(drop)?)
        },
    ];
    for activity in activities {
        world.clock.advance(almost);
        assert_eq!(world.delivery.expire_idle()?, vec![]);
        activity()?;
    }
    world.clock.advance(almost);
    assert_eq!(world.delivery.expire_idle()?, vec![]);
    world.clock.advance(Duration::from_secs(1));
    assert_eq!(world.delivery.expire_idle()?, vec![party]);
    Ok(())
}

fn a_group_uploaded_after_its_first_commit_starts_at_that_epoch(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let meetup = GroupId::from_bytes([8; 32]);
    world.delivery.create_group(
        CreateGroup {
            group_id: meetup,
            auth_key: auth_key(1),
            subscriptions: Vec::new(),
            state: Some(SealedSnapshot::from_bytes(vec![0xE1])),
        },
        GroupStart {
            lifetime: GroupLifetime::Meetup,
            epoch: Epoch::new(1),
        },
    )?;
    assert_eq!(
        world
            .delivery
            .state(&meetup, Reader::Current)?
            .map(|state| state.epoch),
        Some(Epoch::new(1))
    );
    let stale = world.delivery.commit(
        &meetup,
        commit_request(0, Urgency::Quiet),
        &mut seeded_rng(5),
    )?;
    assert!(matches!(
        stale,
        CommitOutcome::Conflict { head_epoch } if head_epoch == Epoch::new(1)
    ));
    let next = world.delivery.commit(
        &meetup,
        commit_request(1, Urgency::Quiet),
        &mut seeded_rng(5),
    )?;
    assert!(matches!(next, CommitOutcome::Accepted { .. }));
    assert_eq!(
        world
            .delivery
            .messages(&meetup, SequenceNumber::new(0), Reader::Current)?
            .head_epoch,
        Epoch::new(2)
    );
    Ok(())
}

// --- who reads the log ---

/// Positions and epochs of what `reader` reads after `after`, and the head epoch it is told.
fn read_as(
    world: &World<impl DeliveryStore>,
    after: u64,
    reader: Reader,
) -> Result<(Vec<(u64, u64)>, u64), TestError> {
    let page = world
        .delivery
        .messages(&world.group, SequenceNumber::new(after), reader)?;
    Ok((
        page.messages
            .iter()
            .map(|entry| (entry.sequence.get(), entry.epoch.get()))
            .collect(),
        page.head_epoch.get(),
    ))
}

/// The log: 1 post (epoch 0), 2 commit (0 to 1), 3 a late post still in epoch 0, 4 post
/// (epoch 1), 5 commit (1 to 2), 6 post (epoch 2), 7 a late post in epoch 0 again.
fn three_epochs(store: impl DeliveryStore) -> Result<World<impl DeliveryStore>, TestError> {
    let world = World::new(store, &[])?;
    let _ = world.post(0, Mode::Stored, None)?;
    let _ = world.commit(0, Urgency::Quiet)?;
    let _ = world.post(0, Mode::Stored, None)?;
    let _ = world.post(1, Mode::Stored, None)?;
    let _ = world.commit(1, Urgency::Quiet)?;
    let _ = world.post(2, Mode::Stored, None)?;
    let _ = world.post(0, Mode::Stored, None)?;
    Ok(world)
}

fn a_lagging_member_catches_up_one_commit_at_a_time(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = three_epochs(store)?;
    // With epoch 0's key: up to the commit that ended epoch 0.
    assert_eq!(
        read_as(&world, 0, Reader::Behind(Epoch::new(0)))?,
        (vec![(1, 0), (2, 0)], 1)
    );
    // Having applied that commit, with epoch 1's key from where it stopped: the late epoch-0
    // post is not lost.
    assert_eq!(
        read_as(&world, 2, Reader::Behind(Epoch::new(1)))?,
        (vec![(3, 0), (4, 1), (5, 1)], 2)
    );
    // And with the current key, the rest.
    assert_eq!(
        read_as(&world, 5, Reader::Current)?,
        (vec![(6, 2), (7, 0)], 2)
    );
    Ok(())
}

fn a_removed_member_reads_nothing_written_after_its_removal(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    // Removed by the commit that ended epoch 0, it keeps only epoch 0's key.
    let world = three_epochs(store)?;
    let removed = Reader::Behind(Epoch::new(0));
    // It reads the commit that removed it, and nothing after: not the late epoch-0 posts (3
    // and 7) it could open, whatever position it asks from.
    assert_eq!(read_as(&world, 1, removed)?, (vec![(2, 0)], 1));
    for after in 2..=7 {
        assert_eq!(read_as(&world, after, removed)?, (vec![], 1));
    }
    assert_eq!(
        world.delivery.head_epoch(&world.group, removed)?,
        Epoch::new(1)
    );
    // The head is not overstated for a reader one epoch behind.
    assert_eq!(
        world
            .delivery
            .head_epoch(&world.group, Reader::Behind(Epoch::new(1)))?,
        Epoch::new(2)
    );
    // Nor is a later snapshot shown to it.
    let _ = world.commit_with(CommitRequest {
        state: Some(SealedSnapshot::from_bytes(vec![0xE3])),
        ..commit_request(2, Urgency::Quiet)
    })?;
    assert!(matches!(
        world.delivery.state(&world.group, removed),
        Err(DeliveryError::LaterEpoch)
    ));
    assert!(matches!(
        world
            .delivery
            .state(&world.group, Reader::Behind(Epoch::new(3))),
        Ok(Some(_))
    ));
    assert!(
        world
            .delivery
            .state(&world.group, Reader::Current)?
            .is_some()
    );
    Ok(())
}

fn reads_from_behind_do_not_keep_a_group_alive(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let _ = world.commit(0, Urgency::Quiet)?;
    world
        .clock
        .advance(MEETUP_IDLE_LIFETIME.saturating_sub(Duration::from_secs(1)));
    let _ = read_as(&world, 0, Reader::Behind(Epoch::new(0)))?;
    let _ = world
        .delivery
        .state(&world.group, Reader::Behind(Epoch::new(1)))?;
    world.clock.advance(Duration::from_secs(1));
    assert_eq!(world.delivery.expire_idle()?, vec![world.group]);
    Ok(())
}

fn earlier_epochs_keys_are_kept_newest_first(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    assert_eq!(world.delivery.past_auth_keys(&world.group)?, vec![]);
    let _first = world.commit(0, Urgency::Quiet)?;
    let _second = world.commit(1, Urgency::Quiet)?;
    // A losing commit changes nothing.
    let _lost = world.commit(0, Urgency::Quiet)?;
    assert_eq!(
        world.delivery.past_auth_keys(&world.group)?,
        vec![
            PastAuthKey {
                epoch: Epoch::new(1),
                key: commit_request(0, Urgency::Quiet).next_auth_key,
            },
            PastAuthKey {
                epoch: Epoch::new(0),
                key: auth_key(0),
            },
        ]
    );
    assert_eq!(
        world.delivery.head_epoch(&world.group, Reader::Current)?,
        Epoch::new(2)
    );
    world.delivery.delete_group(&world.group)?;
    assert!(matches!(
        world.delivery.past_auth_keys(&world.group),
        Err(DeliveryError::UnknownGroup)
    ));
    Ok(())
}

/// A Party group outlives a delete: any member holds its log-auth key, so a delete would let
/// one member erase the Party for everyone.
fn a_party_group_is_not_deleted_on_request(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let party = GroupId::from_bytes([8; 32]);
    world.delivery.create_group(
        CreateGroup {
            group_id: party,
            auth_key: auth_key(0),
            subscriptions: vec![subscription(1, SUBSCRIPTION_LIFETIME)?],
            state: None,
        },
        at_zero(GroupLifetime::Party),
    )?;
    assert!(matches!(
        world.delivery.delete_group(&party),
        Err(DeliveryError::PartyGroupKept)
    ));
    assert!(world.delivery.subscription(&party, &sub_id(1))?.is_some());
    // A Meetup group still goes when its last member deletes it.
    world.delivery.delete_group(&world.group)?;
    Ok(())
}

/// A subscription keeps the owner key it was first stored with: neither a PUT nor a commit can
/// hand its id to another key, until it expires.
fn a_subscription_keeps_its_owner_key(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    let hijack = Subscription {
        owner: SubscriptionOwner::from_bytes([0xee; 32]),
        ..subscription(1, SUBSCRIPTION_LIFETIME)?
    };
    assert!(matches!(
        world
            .delivery
            .put_subscription(&world.group, hijack.clone()),
        Err(DeliveryError::SubscriptionOwned)
    ));
    assert!(matches!(
        world.commit_with(CommitRequest {
            subscriptions_added: vec![hijack.clone()],
            ..commit_request(0, Urgency::Quiet)
        }),
        Err(TestError::Delivery(DeliveryError::SubscriptionOwned))
    ));
    // The owner renews it; a new id can have any owner.
    world
        .delivery
        .put_subscription(&world.group, subscription(1, SUBSCRIPTION_LIFETIME)?)?;
    world
        .delivery
        .put_subscription(&world.group, subscription(2, SUBSCRIPTION_LIFETIME)?)?;
    assert_eq!(
        world
            .delivery
            .subscription(&world.group, &sub_id(1))?
            .map(|found| found.owner),
        Some(SubscriptionOwner::from_bytes([1; 32]))
    );
    // Once it has expired, the id is free.
    world.clock.advance(SUBSCRIPTION_LIFETIME);
    world.delivery.put_subscription(&world.group, hijack)?;
    Ok(())
}

fn delete_removes_the_group_and_closes_its_channels(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    let _channel = world.delivery.open_live(
        &world.group,
        LiveAttach {
            epoch: Epoch::new(0),
            subscription: None,
        },
        1,
    )?;
    world.delivery.delete_group(&world.group)?;
    assert_eq!(world.delivery.live_channels(&world.group)?, 0);
    assert!(matches!(
        world.delivery.state(&world.group, Reader::Current),
        Err(DeliveryError::UnknownGroup)
    ));
    assert!(matches!(
        world.delivery.delete_group(&world.group),
        Err(DeliveryError::UnknownGroup)
    ));
    // The id is free again, with nothing left over from the old group.
    world.delivery.create_group(
        CreateGroup {
            group_id: world.group,
            auth_key: auth_key(1),
            subscriptions: Vec::new(),
            state: None,
        },
        at_zero(GroupLifetime::Meetup),
    )?;
    assert_eq!(world.log()?, vec![]);
    assert_eq!(world.delivery.state(&world.group, Reader::Current)?, None);
    let posted = world.post(0, Mode::Stored, Some(Urgency::Alert))?;
    assert!(posted.wakes.is_empty());
    Ok(())
}

fn expiry_leaves_other_groups_alone(store: impl DeliveryStore) -> Result<(), TestError> {
    let world = World::new(store, &[1])?;
    world.clock.advance(Duration::from_hours(1));
    let other = GroupId::from_bytes([8; 32]);
    world.delivery.create_group(
        CreateGroup {
            group_id: other,
            auth_key: auth_key(1),
            subscriptions: vec![subscription(2, SUBSCRIPTION_LIFETIME)?],
            state: None,
        },
        at_zero(GroupLifetime::Meetup),
    )?;
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
    let again = world.delivery.create_group(
        CreateGroup {
            group_id: world.group,
            auth_key: auth_key(9),
            subscriptions: Vec::new(),
            state: None,
        },
        at_zero(GroupLifetime::Meetup),
    );
    assert!(matches!(again, Err(DeliveryError::GroupExists)));
    assert_eq!(world.delivery.auth_key(&world.group)?, auth_key(0));
    Ok(())
}

fn commit_snapshot_replaces_the_state_for_the_next_epoch(
    store: impl DeliveryStore,
) -> Result<(), TestError> {
    let world = World::new(store, &[])?;
    let first = world.delivery.state(&world.group, Reader::Current)?;
    assert_eq!(first.as_ref().map(|state| state.epoch), Some(Epoch::new(0)));
    let _without = world.commit(0, Urgency::Quiet)?;
    let kept = world.delivery.state(&world.group, Reader::Current)?;
    assert_eq!(kept, first, "a commit without a snapshot keeps the old one");
    let mut request = commit_request(1, Urgency::Quiet);
    request.state = Some(SealedSnapshot::from_bytes(vec![0xE2]));
    let _with = world.commit_with(request)?;
    let replaced = world.delivery.state(&world.group, Reader::Current)?;
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
    assert!(unknown(delivery.state(&other, Reader::Current).map(drop)));
    assert!(unknown(
        delivery
            .messages(&other, SequenceNumber::new(0), Reader::Current)
            .map(drop)
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
    a_wake_is_due_only_while_its_subscription_stands,
    live_frame_goes_to_the_other_channels,
    closed_channel_gets_nothing_and_cannot_send,
    a_removal_closes_the_removed_members_live_channel,
    a_removed_members_channel_ends_with_its_epoch,
    a_group_holds_a_bounded_number_of_subscriptions,
    a_group_has_a_bounded_number_of_live_channels,
    a_live_channel_stores_a_bounded_number_of_frames_per_bucket,
    live_channel_needs_an_existing_group,
    idle_group_expires_after_two_hours,
    writes_postpone_expiry,
    reads_and_live_channels_postpone_expiry,
    party_groups_stay_idle_for_half_a_year,
    a_lagging_member_catches_up_one_commit_at_a_time,
    a_removed_member_reads_nothing_written_after_its_removal,
    reads_from_behind_do_not_keep_a_group_alive,
    earlier_epochs_keys_are_kept_newest_first,
    a_group_uploaded_after_its_first_commit_starts_at_that_epoch,
    delete_removes_the_group_and_closes_its_channels,
    a_party_group_is_not_deleted_on_request,
    a_subscription_keeps_its_owner_key,
    expiry_leaves_other_groups_alone,
    duplicate_group_is_refused,
    commit_snapshot_replaces_the_state_for_the_next_epoch,
    unknown_group_is_reported_everywhere,
);

/// The limits are the design's absolute numbers, not just consistent with each other.
#[test]
fn limits_are_thirty_days_two_hours_half_a_year_and_thirty_seconds() {
    assert_eq!(MAX_SUBSCRIPTION_LIFETIME, Duration::from_hours(720));
    assert_eq!(MEETUP_IDLE_LIFETIME, Duration::from_hours(2));
    assert_eq!(PARTY_IDLE_LIFETIME, Duration::from_hours(180 * 24));
    assert_eq!(MAX_QUIET_DELAY, Duration::from_secs(30));
}

/// A deleted group leaves no trace of its id in the database or its write-ahead log.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_store_overwrites_deleted_groups() -> Result<(), TestError> {
    use pingers_delivery::SqliteDeliveryStore;

    let path = std::env::temp_dir().join(format!(
        "pingers-delivery-secure-delete-{}.sqlite",
        std::process::id()
    ));
    let wal = path.with_extension("sqlite-wal");
    let _stale = std::fs::remove_file(&path);
    let world = World::new(SqliteDeliveryStore::open(&path)?, &[1])?;
    let _posted = world.post(0, Mode::Stored, None)?;
    world.delivery.delete_group(&world.group)?;
    let id = [7_u8; 32];
    for file in [&path, &wal] {
        let bytes = std::fs::read(file).unwrap_or_default();
        assert!(
            !bytes.windows(id.len()).any(|window| window == id),
            "{} still holds the group id",
            file.display()
        );
    }
    drop(world);
    for file in [&path, &wal, &path.with_extension("sqlite-shm")] {
        let _cleanup = std::fs::remove_file(file);
    }
    Ok(())
}

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
    let read = reopened.messages(&group, SequenceNumber::new(0), Reader::Current)?;
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
    assert_eq!(
        reopened.store().head(&group)?.lifetime,
        GroupLifetime::Meetup
    );
    drop(reopened);
    let _cleanup = std::fs::remove_file(&path);
    Ok(())
}
