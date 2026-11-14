// SPDX-License-Identifier: AGPL-3.0-only
//! The local copy of the membership tree (design B.4.8, "Tree updates and root staleness").
//!
//! The main app keeps every leaf the server's current hourly root contains and computes its own
//! Merkle path, so it never asks for "my path", which would name it. A refresh fetches the
//! public leaf list (`GET /v1/rln/leaves?after=n`, anonymous, no credential) from where the copy
//! ends, zeroes the positions the server lists as removed, and checks that the copy reproduces
//! the server's latest root before recording it. A copy that does not is fetched again from the
//! start once; a second mismatch is an error and the device keeps using Privacy Pass.
//!
//! What a proof needs, the root and this device's path to it, is kept in the store
//! (`rln_tree_state`), so a proof in a wake reads one row. The tree itself is kept in memory
//! between refreshes in the same process, so a refresh costs about 20 hashes per changed leaf;
//! a new process rebuilds it once from the stored leaves.
//!
//! Roots are hourly and the server accepts the current and the previous one. A refresh is due
//! once the copy's root is no longer the current one; a proof needs one no older than the
//! previous one, so a device whose copy is more than an hour behind refreshes before proving.

use std::collections::BTreeSet;

use pingers_anon::{LeafUpdate, LeafValue, MembershipRoot, MembershipTree};
use pingers_proto::{HourlyRoot, MembershipIndex, MembershipLeaves, RootEpoch};
use pingers_store::{
    PATH_LENGTH, RlnIdentity, Session, StoredPath, TreeLeaf, TreePosition, TreeState,
};
use pingers_time::UnixSeconds;

use crate::credential::root_epoch;
use crate::engine::blocking;
use crate::{ApiRequest, SyncEngine, SyncError, WakeBudget};

/// What a membership refresh did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct MembershipRefresh {
    /// Leaves appended to the local copy.
    pub added: usize,
    /// Leaves newly zeroed (removed devices).
    pub zeroed: usize,
    /// The hourly root the copy now reproduces, or `None` before the server's first root.
    pub root_epoch: Option<RootEpoch>,
}

/// The tree kept in memory between refreshes, with what it was built from.
pub(crate) struct TreeCache {
    tree: MembershipTree,
    /// Leaves in `tree`.
    leaf_count: u32,
    /// Positions zeroed in `tree`.
    zeroed: BTreeSet<u32>,
}

/// One pass over the leaf list.
enum Pass {
    /// The copy reproduces the server's root (or the server has none yet).
    Matches(MembershipRefresh),
    /// It does not: fetch again from the start.
    Mismatch,
}

/// What the pages of one pass brought.
struct Fetched {
    /// Leaves the copy held before.
    start: u32,
    /// New leaves, in order from `start`.
    appended: Vec<LeafValue>,
    /// Every zeroed position, as the last page listed them.
    zeroed: Vec<MembershipIndex>,
    /// The server's latest root, as the last page listed it.
    root: Option<HourlyRoot>,
}

/// Whether a refresh is due at `now`: no copy yet, or its root is no longer the current one.
pub(crate) fn refresh_due(state: Option<&TreeState>, now: UnixSeconds) -> bool {
    state.is_none_or(|state| u64::from(state.root_epoch.get()) < root_epoch(now))
}

/// Whether a proof against the copy's root is accepted at `now`: the server takes the current
/// and the previous hourly root.
pub(crate) fn root_accepted(state: &TreeState, now: UnixSeconds) -> bool {
    u64::from(state.root_epoch.get()).saturating_add(1) >= root_epoch(now)
}

impl SyncEngine {
    /// Refreshes the membership tree if its root is no longer the current hour's
    /// ([`SyncEngine::refresh_membership`]); otherwise costs one store read.
    ///
    /// # Errors
    ///
    /// As [`SyncEngine::refresh_membership`].
    pub async fn ensure_membership_fresh(
        &self,
        budget: WakeBudget,
    ) -> Result<Option<MembershipRefresh>, SyncError> {
        let now = self.now();
        let state = blocking(&self.database, |session| {
            Ok(session.rln_tree_state().load()?)
        })
        .await?;
        if !refresh_due(state.as_ref(), now) {
            return Ok(None);
        }
        self.refresh_membership(budget).await.map(Some)
    }

    /// Brings the local membership tree up to the server's latest root: appends new leaves
    /// page by page, zeroes the listed positions, checks the root, and records the root and this
    /// device's path. A copy that does not reproduce the root is dropped and fetched again once.
    ///
    /// # Errors
    ///
    /// [`SyncError::TreeMismatch`] if a fresh copy still does not reproduce the server's root;
    /// [`SyncError::Status`]; [`SyncError::Store`]; as [`SyncEngine::send`].
    pub async fn refresh_membership(
        &self,
        budget: WakeBudget,
    ) -> Result<MembershipRefresh, SyncError> {
        if let Pass::Matches(refresh) = self.refresh_pass(budget).await? {
            return Ok(refresh);
        }
        self.forget_tree().await?;
        match self.refresh_pass(budget).await? {
            Pass::Matches(refresh) => Ok(refresh),
            Pass::Mismatch => Err(SyncError::TreeMismatch),
        }
    }

    /// Drops the local copy, in the store and in memory.
    async fn forget_tree(&self) -> Result<(), SyncError> {
        drop(self.take_tree_cache());
        blocking(&self.database, |session| {
            session.transaction(|session| {
                session.rln_tree().clear()?;
                session.rln_tree_state().clear()?;
                Ok::<_, SyncError>(())
            })
        })
        .await
    }

    async fn refresh_pass(&self, budget: WakeBudget) -> Result<Pass, SyncError> {
        let fetched = self.fetch_leaves(budget).await?;
        let cache = self.take_tree_cache();
        let (pass, cache) = blocking(&self.database, move |session| {
            apply_fetched(session, &fetched, cache)
        })
        .await?;
        self.put_tree_cache(cache);
        Ok(pass)
    }

    /// Fetches pages from the end of the copy until one comes back empty, storing each page's
    /// leaves as it arrives.
    async fn fetch_leaves(&self, budget: WakeBudget) -> Result<Fetched, SyncError> {
        let start = blocking(&self.database, |session| {
            Ok(session.rln_tree().leaf_count()?)
        })
        .await?;
        let mut fetched = Fetched {
            start,
            appended: Vec::new(),
            zeroed: Vec::new(),
            root: None,
        };
        let mut after = start;
        loop {
            let page: MembershipLeaves = self
                .exchange(ApiRequest::rln_leaves(MembershipIndex::new(after)), budget)
                .await?
                .success()?
                .decode()?;
            fetched.root = page.roots.last().copied();
            fetched.zeroed = page.zeroed;
            if page.leaves.is_empty() {
                return Ok(fetched);
            }
            let count =
                u32::try_from(page.leaves.len()).map_err(|_| SyncError::OutOfRange("tree"))?;
            let next = after
                .checked_add(count)
                .ok_or(SyncError::OutOfRange("tree"))?;
            let leaves: Vec<TreeLeaf> = page
                .leaves
                .iter()
                .zip(after..)
                .map(|(value, position)| TreeLeaf {
                    position: TreePosition::new(position),
                    value: LeafValue::from_bytes(*value),
                })
                .collect();
            fetched
                .appended
                .extend(leaves.iter().map(|leaf| leaf.value));
            blocking(&self.database, move |session| {
                Ok(session.rln_tree().set_leaves(&leaves)?)
            })
            .await?;
            after = next;
        }
    }
}

/// Applies a pass to the copy: zeroes in the store, the tree (from the cache, or rebuilt from
/// the stored leaves), the root check, and the recorded state.
fn apply_fetched(
    session: &Session,
    fetched: &Fetched,
    cache: Option<TreeCache>,
) -> Result<(Pass, Option<TreeCache>), SyncError> {
    let count = fetched
        .start
        .checked_add(
            u32::try_from(fetched.appended.len()).map_err(|_| SyncError::OutOfRange("tree"))?,
        )
        .ok_or(SyncError::OutOfRange("tree"))?;
    let listed: BTreeSet<u32> = fetched
        .zeroed
        .iter()
        .map(|position| position.get())
        .filter(|position| *position < count)
        .collect();
    let stored_state = session.rln_tree_state().load()?;
    let mut cache = cache.filter(|cache| {
        cache.leaf_count == fetched.start
            && stored_state
                .as_ref()
                .is_some_and(|state| state.root == cache.tree.root())
    });
    let newly_zeroed: Vec<u32> = listed
        .iter()
        .copied()
        .filter(|position| {
            cache
                .as_ref()
                .is_none_or(|cache| !cache.zeroed.contains(position))
        })
        .collect();
    session.rln_tree().set_leaves(
        &newly_zeroed
            .iter()
            .map(|position| TreeLeaf {
                position: TreePosition::new(*position),
                value: LeafValue::from_bytes([0; 32]),
            })
            .collect::<Vec<_>>(),
    )?;
    let tree = match cache.take() {
        Some(mut cache) => {
            let updates: Vec<LeafUpdate> = fetched
                .appended
                .iter()
                .map(|value| LeafUpdate::Appended(*value))
                .chain(
                    newly_zeroed
                        .iter()
                        .map(|position| LeafUpdate::Zeroed(MembershipIndex::new(*position))),
                )
                .collect();
            match cache.tree.apply(&updates) {
                Ok(()) => cache.tree,
                // A leaf that is not a field element: the copy is wrong.
                Err(_) => return Ok((Pass::Mismatch, None)),
            }
        }
        None => match crate::credential::tree_from(session.rln_tree().leaves()?) {
            Ok(tree) => tree,
            Err(SyncError::Anon(_)) => return Ok((Pass::Mismatch, None)),
            Err(other) => return Err(other),
        },
    };
    let refresh = MembershipRefresh {
        added: fetched.appended.len(),
        zeroed: newly_zeroed.len(),
        root_epoch: fetched.root.map(|root| root.epoch),
    };
    let Some(root) = fetched.root else {
        // The server has no root yet: nothing can be proved, so nothing is recorded.
        return Ok((Pass::Matches(refresh), None));
    };
    if tree.root() != MembershipRoot::from_bytes(root.root) || tree.leaf_count() != count {
        return Ok((Pass::Mismatch, None));
    }
    let identity = session.rln_identity().load()?;
    let path = own_path(session, &tree, identity.as_ref())?;
    session.rln_tree_state().save(&TreeState {
        root_epoch: root.epoch,
        root: tree.root(),
        leaf_count: count,
        path,
    })?;
    Ok((
        Pass::Matches(refresh),
        Some(TreeCache {
            tree,
            leaf_count: count,
            zeroed: listed,
        }),
    ))
}

/// This device's path in `tree`, if its leaf is there and not zeroed.
fn own_path(
    session: &Session,
    tree: &MembershipTree,
    identity: Option<&RlnIdentity>,
) -> Result<Option<StoredPath>, SyncError> {
    let Some(identity) = identity else {
        return Ok(None);
    };
    let mine = identity.secret.id_commitment()?.leaf()?;
    if session.rln_tree().leaf(identity.leaf)? != Some(mine) {
        return Ok(None);
    }
    let path = tree.path(MembershipIndex::new(identity.leaf.get()))?;
    let siblings: [[u8; 32]; PATH_LENGTH] = path.sibling_bytes();
    Ok(Some(StoredPath {
        position: identity.leaf,
        siblings,
    }))
}

impl SyncEngine {
    fn take_tree_cache(&self) -> Option<TreeCache> {
        crate::engine::lock(&self.tree_cache).take()
    }

    fn put_tree_cache(&self, cache: Option<TreeCache>) {
        *crate::engine::lock(&self.tree_cache) = cache;
    }
}

#[cfg(test)]
mod tests {
    use pingers_anon::MembershipRoot;

    use super::*;

    fn state(epoch: u32) -> TreeState {
        TreeState {
            root_epoch: RootEpoch::new(epoch),
            root: MembershipRoot::from_bytes([1; 32]),
            leaf_count: 1,
            path: None,
        }
    }

    fn hour(epoch: u64, into: u64) -> UnixSeconds {
        UnixSeconds::new(epoch.saturating_mul(3_600).saturating_add(into))
    }

    #[test]
    fn a_refresh_is_due_once_the_root_is_not_the_current_hour() {
        assert!(refresh_due(None, hour(10, 0)));
        assert!(!refresh_due(Some(&state(10)), hour(10, 3_599)));
        assert!(refresh_due(Some(&state(10)), hour(11, 0)));
        assert!(
            !refresh_due(Some(&state(11)), hour(10, 0)),
            "a root from the future"
        );
    }

    #[test]
    fn the_current_and_previous_root_are_accepted() {
        assert!(root_accepted(&state(10), hour(10, 0)));
        assert!(root_accepted(&state(10), hour(11, 3_599)));
        assert!(!root_accepted(&state(10), hour(12, 0)));
    }
}
