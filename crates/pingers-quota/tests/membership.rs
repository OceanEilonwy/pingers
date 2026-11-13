// SPDX-License-Identifier: AGPL-3.0-only
//! The membership tree: hourly roots, what the public leaf list shows, and rebuilding from the
//! store. Run against every store implementation.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;
use std::sync::Arc;

use pingers_anon::{AnonError, IdCommitment, LeafUpdate, LeafValue, MembershipTree, RlnSecret};
use pingers_proto::{AccountId, HourlyRoot, MembershipIndex, RootEpoch};
use pingers_quota::{
    LEAVES_PER_PAGE, Membership, MembershipStore, MemoryQuotaStore, QuotaError, ROOT_EPOCH_LENGTH,
    root_epoch,
};
use pingers_testkit::{ManualClock, TEST_START, seeded_rng, test_clock};
use pingers_time::{Clock as _, UnixSeconds};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Quota(#[from] QuotaError),
    #[error(transparent)]
    Anon(#[from] AnonError),
}

fn account(tag: u8) -> AccountId {
    AccountId::from_bytes([tag; 16])
}

fn identity(seed: u64) -> Result<IdCommitment, TestError> {
    Ok(RlnSecret::generate(&mut seeded_rng(seed))?.id_commitment()?)
}

/// Moves the clock to the start of the next root epoch.
fn next_hour(clock: &ManualClock) -> Result<(), TestError> {
    let epoch = root_epoch(clock.now())?;
    let start = u64::from(epoch.get())
        .saturating_add(1)
        .saturating_mul(3600);
    clock.advance(UnixSeconds::new(start).saturating_duration_since(clock.now()));
    Ok(())
}

/// The tree a client builds from the public list.
fn client_tree(membership: &Membership<impl MembershipStore>) -> Result<MembershipTree, TestError> {
    let list = membership.leaves(MembershipIndex::new(0))?;
    let mut updates: Vec<LeafUpdate> = list
        .leaves
        .iter()
        .map(|leaf| LeafUpdate::Appended(LeafValue::from_bytes(*leaf)))
        .collect();
    updates.extend(list.zeroed.iter().map(|index| LeafUpdate::Zeroed(*index)));
    let mut tree = MembershipTree::empty()?;
    tree.apply(&updates)?;
    Ok(tree)
}

fn service<S: MembershipStore>(store: S) -> Result<(ManualClock, Membership<S>), TestError> {
    let clock = test_clock();
    let membership = Membership::new(store, Arc::new(clock.clone()))?;
    Ok((clock, membership))
}

fn registration_is_listed_from_the_next_root_epoch(
    store: impl MembershipStore,
) -> Result<(), TestError> {
    let (clock, membership) = service(store)?;
    let commitment = identity(1)?;
    let response = membership.register(account(1), &commitment)?;
    let now = root_epoch(TEST_START)?;
    assert_eq!(response.index, MembershipIndex::new(0));
    assert_eq!(
        response.live_from,
        RootEpoch::new(now.get().saturating_add(1))
    );
    let before = membership.leaves(MembershipIndex::new(0))?;
    assert!(before.leaves.is_empty(), "pending leaves are not listed");
    assert_eq!(before.next, MembershipIndex::new(0));
    let empty_root = *MembershipTree::empty()?.root().as_bytes();
    assert_eq!(
        before.roots,
        vec![HourlyRoot {
            epoch: now,
            root: empty_root
        }]
    );

    clock.advance(ROOT_EPOCH_LENGTH.saturating_sub(Duration::from_secs(901)));
    assert!(
        membership
            .leaves(MembershipIndex::new(0))?
            .leaves
            .is_empty(),
        "still the same hour"
    );
    next_hour(&clock)?;
    let after = membership.leaves(MembershipIndex::new(0))?;
    assert_eq!(after.leaves, vec![*commitment.leaf()?.as_bytes()]);
    assert_eq!(after.next, MembershipIndex::new(1));
    let tree = client_tree(&membership)?;
    let roots = membership.accepted_roots()?;
    assert_eq!(roots.current, tree.root());
    assert_eq!(
        roots.previous.map(|root| *root.as_bytes()),
        Some(empty_root)
    );
    assert_eq!(after.roots.len(), 2);
    assert_eq!(
        after.roots.last().map(|root| root.epoch),
        Some(response.live_from)
    );
    Ok(())
}

fn only_the_current_and_previous_roots_are_accepted(
    store: impl MembershipStore,
) -> Result<(), TestError> {
    let (clock, membership) = service(store)?;
    let first = membership.accepted_roots()?;
    assert_eq!(first.previous, None);
    membership.register(account(1), &identity(1)?)?;
    next_hour(&clock)?;
    let second = membership.accepted_roots()?;
    membership.register(account(2), &identity(2)?)?;
    next_hour(&clock)?;
    let third = membership.accepted_roots()?;
    assert!(second.accepts(&first.current));
    assert!(third.accepts(&second.current));
    assert!(!third.accepts(&first.current));
    Ok(())
}

fn an_hour_without_changes_still_rolls_the_root_window(
    store: impl MembershipStore,
) -> Result<(), TestError> {
    let (clock, membership) = service(store)?;
    membership.register(account(1), &identity(1)?)?;
    let empty = membership.accepted_roots()?.current;
    next_hour(&clock)?;
    membership.roll()?;
    next_hour(&clock)?;
    let roots = membership.accepted_roots()?;
    assert!(
        !roots.accepts(&empty),
        "the empty root aged out after two hours"
    );
    assert_eq!(roots.previous, Some(roots.current));
    Ok(())
}

fn registering_again_zeroes_the_old_leaf_at_the_next_root(
    store: impl MembershipStore,
) -> Result<(), TestError> {
    let (clock, membership) = service(store)?;
    membership.register(account(1), &identity(1)?)?;
    next_hour(&clock)?;
    let again = membership.register(account(1), &identity(2)?)?;
    assert_eq!(again.index, MembershipIndex::new(1));
    assert!(
        membership
            .leaves(MembershipIndex::new(0))?
            .zeroed
            .is_empty()
    );
    next_hour(&clock)?;
    let list = membership.leaves(MembershipIndex::new(0))?;
    assert_eq!(list.leaves.len(), 2);
    assert_eq!(list.zeroed, vec![MembershipIndex::new(0)]);
    assert_eq!(
        membership.accepted_roots()?.current,
        client_tree(&membership)?.root()
    );
    Ok(())
}

fn removal_zeroes_the_leaf_at_the_next_root(store: impl MembershipStore) -> Result<(), TestError> {
    let (clock, membership) = service(store)?;
    membership.register(account(1), &identity(1)?)?;
    membership.register(account(2), &identity(2)?)?;
    next_hour(&clock)?;
    membership.remove(&account(1))?;
    membership.remove(&account(9))?;
    assert!(
        membership
            .leaves(MembershipIndex::new(0))?
            .zeroed
            .is_empty()
    );
    next_hour(&clock)?;
    let list = membership.leaves(MembershipIndex::new(0))?;
    assert_eq!(list.zeroed, vec![MembershipIndex::new(0)]);
    assert_eq!(
        membership.accepted_roots()?.current,
        client_tree(&membership)?.root()
    );
    // Removing again changes nothing.
    membership.remove(&account(1))?;
    next_hour(&clock)?;
    assert_eq!(
        membership.leaves(MembershipIndex::new(0))?.zeroed,
        list.zeroed
    );
    Ok(())
}

fn a_leaf_removed_before_it_went_live_never_counts(
    store: impl MembershipStore,
) -> Result<(), TestError> {
    let (clock, membership) = service(store)?;
    membership.register(account(1), &identity(1)?)?;
    membership.remove(&account(1))?;
    next_hour(&clock)?;
    let list = membership.leaves(MembershipIndex::new(0))?;
    assert_eq!(list.leaves.len(), 1);
    assert_eq!(list.zeroed, vec![MembershipIndex::new(0)]);
    assert_eq!(
        membership.accepted_roots()?.current,
        client_tree(&membership)?.root()
    );
    Ok(())
}

fn the_list_reads_from_a_position(store: impl MembershipStore) -> Result<(), TestError> {
    let (clock, membership) = service(store)?;
    let commitments: Vec<IdCommitment> = (1..=3).map(identity).collect::<Result<_, _>>()?;
    for (tag, commitment) in (1_u8..).zip(&commitments) {
        membership.register(account(tag), commitment)?;
    }
    next_hour(&clock)?;
    let tail = membership.leaves(MembershipIndex::new(1))?;
    let expected: Vec<[u8; 32]> = commitments
        .iter()
        .skip(1)
        .map(|commitment| Ok(*commitment.leaf()?.as_bytes()))
        .collect::<Result<_, TestError>>()?;
    assert_eq!(tail.leaves, expected);
    assert_eq!(tail.next, MembershipIndex::new(3));
    let beyond = membership.leaves(MembershipIndex::new(7))?;
    assert!(beyond.leaves.is_empty());
    assert_eq!(beyond.next, MembershipIndex::new(3));
    Ok(())
}

fn a_non_canonical_commitment_is_refused(store: impl MembershipStore) -> Result<(), TestError> {
    let (_clock, membership) = service(store)?;
    let refused = membership.register(account(1), &IdCommitment::from_bytes([0xFF; 32]));
    assert!(matches!(
        refused,
        Err(QuotaError::Anon(AnonError::NotAFieldElement))
    ));
    Ok(())
}

fn the_tree_is_rebuilt_from_the_store<S: MembershipStore>(store: S) -> Result<(), TestError> {
    let store = Arc::new(store);
    let clock = test_clock();
    let first = Membership::new(Arc::clone(&store), Arc::new(clock.clone()))?;
    first.register(account(1), &identity(1)?)?;
    first.register(account(2), &identity(2)?)?;
    next_hour(&clock)?;
    first.remove(&account(1))?;
    let pending = first.register(account(3), &identity(3)?)?;
    let roots = first.accepted_roots()?;
    let list = first.leaves(MembershipIndex::new(0))?;
    drop(first);

    let second = Membership::new(Arc::clone(&store), Arc::new(clock.clone()))?;
    assert_eq!(second.accepted_roots()?, roots);
    assert_eq!(second.leaves(MembershipIndex::new(0))?, list);
    next_hour(&clock)?;
    let later = second.leaves(MembershipIndex::new(0))?;
    assert_eq!(later.leaves.len(), 3, "the pending registration survived");
    assert_eq!(
        later.zeroed,
        vec![MembershipIndex::new(0)],
        "so did the pending removal"
    );
    assert_eq!(
        second.register(account(4), &identity(4)?)?.index,
        MembershipIndex::new(3)
    );
    assert_eq!(pending.index, MembershipIndex::new(2));
    Ok(())
}

fn a_stored_root_that_does_not_match_the_leaves_is_detected<S: MembershipStore>(
    store: S,
) -> Result<(), TestError> {
    store.insert_root(HourlyRoot {
        epoch: RootEpoch::new(5),
        root: [1; 32],
    })?;
    let rebuilt = Membership::new(store, Arc::new(test_clock()));
    assert!(matches!(rebuilt, Err(QuotaError::RootMismatch)));
    Ok(())
}

macro_rules! for_each_store {
    ($($test:ident),* $(,)?) => {
        mod memory {
            use super::*;
            $(
                #[test]
                fn $test() -> Result<(), TestError> {
                    super::$test(MemoryQuotaStore::new())
                }
            )*
        }
        #[cfg(feature = "sqlite")]
        mod sqlite {
            use super::*;
            $(
                #[test]
                fn $test() -> Result<(), TestError> {
                    super::$test(pingers_quota::SqliteQuotaStore::open_in_memory()?)
                }
            )*
        }
    };
}

for_each_store!(
    registration_is_listed_from_the_next_root_epoch,
    only_the_current_and_previous_roots_are_accepted,
    an_hour_without_changes_still_rolls_the_root_window,
    registering_again_zeroes_the_old_leaf_at_the_next_root,
    removal_zeroes_the_leaf_at_the_next_root,
    a_leaf_removed_before_it_went_live_never_counts,
    the_list_reads_from_a_position,
    a_non_canonical_commitment_is_refused,
    the_tree_is_rebuilt_from_the_store,
    a_stored_root_that_does_not_match_the_leaves_is_detected,
);

/// One page of the list is capped, and the next page starts where it ended.
#[test]
fn the_list_is_paged() -> Result<(), TestError> {
    let (clock, membership) = service(MemoryQuotaStore::new())?;
    let commitment = identity(1)?;
    let total = LEAVES_PER_PAGE.saturating_add(1);
    for tag in 0..total {
        let bytes = u128::try_from(tag).unwrap_or(0).to_be_bytes();
        membership.register(AccountId::from_bytes(bytes), &commitment)?;
    }
    next_hour(&clock)?;
    let first = membership.leaves(MembershipIndex::new(0))?;
    assert_eq!(first.leaves.len(), LEAVES_PER_PAGE);
    let rest = membership.leaves(first.next)?;
    assert_eq!(rest.leaves.len(), 1);
    assert_eq!(usize::try_from(rest.next.get()).unwrap_or(0), total);
    Ok(())
}
