// SPDX-License-Identifier: AGPL-3.0-only
//! Which registration a maintenance step picks.

use pingers_crypto::{MailboxReadKey, PushSealFingerprint};
use pingers_proto::PushHandle;
use pingers_store::MailboxState;
use zeroize::Zeroizing;

use super::*;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

fn at(seconds: u64) -> UnixSeconds {
    UnixSeconds::new(seconds)
}

fn stamp(fill: u8, not_after: u64) -> SealStamp {
    SealStamp {
        fingerprint: PushSealFingerprint::from_bytes([fill; 16]),
        not_after: at(not_after),
    }
}

/// The current seal: fingerprint 1, good for 30 days.
fn current() -> SealStamp {
    stamp(1, NOW + 30 * DAY)
}

fn mailbox(fill: u8, push: bool, seal: Option<SealStamp>, expires_at: u64) -> Mailbox {
    Mailbox {
        read_key: MailboxReadKey::from_bytes(Zeroizing::new([fill; 32])),
        purpose: MailboxPurpose::Knock,
        state: MailboxState::Pooled,
        push: push.then(|| PushHandle::from_bytes([fill; 8])),
        seal,
        expires_at: at(expires_at),
    }
}

fn group(fill: u8) -> GroupId {
    GroupId::from_bytes([fill; 32])
}

fn subscription(fill: u8, seal: Option<SealStamp>, expires_at: u64) -> StoredSubscription {
    StoredSubscription {
        group: group(fill),
        id: SubscriptionId::from_bytes([fill; 16]),
        handle: PushHandle::from_bytes([fill; 8]),
        expires_at: at(expires_at),
        seal,
    }
}

fn task(
    mailboxes: &[Mailbox],
    subscriptions: &[StoredSubscription],
    groups: &[GroupId],
    desired: Option<SealStamp>,
) -> Option<Task> {
    let groups: HashSet<GroupId> = groups.iter().copied().collect();
    next_task(
        &Registrations {
            mailboxes,
            subscriptions,
            groups: &groups,
        },
        desired,
        at(NOW),
        TimeToLive::new(Duration::from_hours(30 * 24)).ok()?,
    )
}

#[test]
fn with_pushes_off_subscriptions_go_first_then_mailbox_pushes() {
    let pushed = mailbox(1, true, Some(current()), NOW + DAY);
    let subscribed = subscription(3, Some(current()), NOW + 30 * DAY);
    let pushed_id = pushed.id();
    let both = [mailbox(2, false, None, NOW + DAY), pushed];
    assert_eq!(
        task(&both, &[subscribed], &[group(3)], None),
        Some(Task::RemoveSubscription(group(3)))
    );
    assert_eq!(
        task(&both, &[], &[group(3)], None),
        Some(Task::RemoveMailboxPush(pushed_id))
    );
    let bare = [mailbox(2, false, None, NOW + DAY)];
    assert_eq!(task(&bare, &[], &[group(3)], None), None);
}

#[test]
fn a_group_without_a_subscription_comes_before_any_stale_one() {
    let stale = subscription(3, None, NOW + DAY);
    assert_eq!(
        task(&[], &[stale], &[group(3), group(4)], Some(current())),
        Some(Task::AddSubscription(group(4)))
    );
}

#[test]
fn stale_seals_come_before_expiring_ones_unrecorded_first_then_the_earliest() {
    let expiring = subscription(2, Some(current()), NOW + DAY);
    let old_key_late = subscription(3, Some(stamp(9, NOW + 20 * DAY)), NOW + 30 * DAY);
    let old_key_early = mailbox(4, true, Some(stamp(9, NOW + 10 * DAY)), NOW + 30 * DAY);
    let unrecorded = mailbox(5, true, None, NOW + 30 * DAY);
    let groups = [group(2), group(3)];
    let subscriptions = [expiring, old_key_late];
    let (early_id, unrecorded_id) = (old_key_early.id(), unrecorded.id());
    let both = [old_key_early, unrecorded];
    assert_eq!(
        task(&both, &subscriptions, &groups, Some(current())),
        Some(Task::ResealMailbox(unrecorded_id))
    );
    let early = [mailbox(
        4,
        true,
        Some(stamp(9, NOW + 10 * DAY)),
        NOW + 30 * DAY,
    )];
    assert_eq!(
        task(&early, &subscriptions, &groups, Some(current())),
        Some(Task::ResealMailbox(early_id))
    );
    assert_eq!(
        task(&[], &subscriptions, &groups, Some(current())),
        Some(Task::ResealSubscription(group(3)))
    );
    assert_eq!(
        task(&[], &[expiring], &[group(2)], Some(current())),
        Some(Task::ResealSubscription(group(2)))
    );
}

#[test]
fn a_subscription_is_renewed_within_three_days_of_running_out() {
    let groups = [group(2)];
    let inside = subscription(2, Some(current()), NOW + 3 * DAY);
    let outside = subscription(2, Some(current()), NOW + 3 * DAY + 1);
    assert_eq!(
        task(&[], &[inside], &groups, Some(current())),
        Some(Task::ResealSubscription(group(2)))
    );
    assert_eq!(task(&[], &[outside], &groups, Some(current())), None);
    // A seal running out before the subscription counts too (capped before a rotation).
    let capped = subscription(2, Some(stamp(1, NOW + 2 * DAY)), NOW + 30 * DAY);
    assert_eq!(
        task(&[], &[capped], &groups, Some(current())),
        Some(Task::ResealSubscription(group(2)))
    );
}

#[test]
fn a_mailbox_seal_is_renewed_only_if_it_runs_out_first() {
    let outlives = [mailbox(1, true, Some(stamp(1, NOW + DAY)), NOW + DAY)];
    assert_eq!(task(&outlives, &[], &[], Some(current())), None);
    let runs_out = mailbox(2, true, Some(stamp(1, NOW + 2 * DAY)), NOW + 20 * DAY);
    let runs_out_id = runs_out.id();
    assert_eq!(
        task(&[runs_out], &[], &[], Some(current())),
        Some(Task::ResealMailbox(runs_out_id))
    );
    let later = [mailbox(
        3,
        true,
        Some(stamp(1, NOW + 5 * DAY)),
        NOW + 20 * DAY,
    )];
    assert_eq!(task(&later, &[], &[], Some(current())), None);
}

#[test]
fn a_mailbox_registered_without_a_push_gets_one_when_pushes_are_on() {
    let bare = mailbox(1, false, None, NOW + DAY);
    let bare_id = bare.id();
    assert_eq!(
        task(&[bare], &[], &[], Some(current())),
        Some(Task::ResealMailbox(bare_id))
    );
}

#[test]
fn a_subscription_of_a_group_left_is_never_chosen() {
    let left = subscription(7, None, NOW + DAY);
    assert_eq!(task(&[], &[left], &[], Some(current())), None);
    assert_eq!(task(&[], &[left], &[], None), None);
}

#[test]
fn up_to_date_registrations_need_nothing() {
    let fresh_mailbox = [mailbox(1, true, Some(current()), NOW + DAY)];
    let fresh_subscription = [subscription(2, Some(current()), NOW + 30 * DAY)];
    assert_eq!(
        task(
            &fresh_mailbox,
            &fresh_subscription,
            &[group(2)],
            Some(current())
        ),
        None
    );
}

#[test]
fn a_renewal_that_gains_no_time_is_not_made() {
    // A relay whose longest token is a day: a fresh seal is already inside the window.
    let short = stamp(1, NOW + DAY);
    let fresh = [mailbox(1, true, Some(short), NOW + 20 * DAY)];
    assert_eq!(task(&fresh, &[], &[], Some(short)), None);
    let subscribed = [subscription(2, Some(short), NOW + 30 * DAY)];
    assert_eq!(task(&[], &subscribed, &[group(2)], Some(short)), None);
    // A day later the same relay gives a later not_after, so renewing gains time.
    let later = stamp(1, NOW + 2 * DAY);
    assert_eq!(
        task(&[], &subscribed, &[group(2)], Some(later)),
        Some(Task::ResealSubscription(group(2)))
    );
}
