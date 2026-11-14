// SPDX-License-Identifier: AGPL-3.0-only
//! The RLN membership tree and its hourly roots (design B.4.8, "Tree updates and root
//! staleness").
//!
//! Registrations and removals take effect at the next root epoch (one hour). The public leaf
//! list only shows what the current root contains, so a client that builds its tree from the
//! list always reaches an accepted root. The server accepts the current and the previous root.

use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard};

use pingers_anon::{
    AcceptedRoots, AnonError, IdCommitment, LeafUpdate, MembershipRoot, MembershipTree,
};
use pingers_proto::{
    AccountId, HourlyRoot, MembershipIndex, MembershipLeafResponse, MembershipLeaves, RootEpoch,
};
use pingers_time::{Clock, UnixSeconds};

use crate::QuotaError;
use crate::store::{MembershipStore, StoredLeaf, Zeroing};

/// How long one root epoch lasts.
pub const ROOT_EPOCH_LENGTH: Duration = Duration::from_hours(1);

/// The most leaves one `GET /v1/rln/leaves` answer carries (320 KB of leaves at 10,000).
pub const LEAVES_PER_PAGE: usize = 10_000;

/// The tree's capacity: depth 20.
const TREE_CAPACITY: usize = 1 << 20;

/// How many leaves one account may register in 24 root epochs (a day). A device needs one;
/// a reinstall or a crash between registering and saving its secret needs another. Without a
/// limit, one account could fill the tree (every registration takes a new position).
pub const DAILY_LEAF_LIMIT: usize = 4;

/// The root epochs [`DAILY_LEAF_LIMIT`] counts over.
const LEAF_LIMIT_EPOCHS: u32 = 24;

/// The root epoch containing `instant`.
///
/// # Errors
///
/// [`QuotaError::TimeOutOfRange`] past the year 491,000 or so, when the hour count leaves
/// `u32`.
pub fn root_epoch(instant: UnixSeconds) -> Result<RootEpoch, QuotaError> {
    let hours = instant
        .get()
        .checked_div(ROOT_EPOCH_LENGTH.as_secs())
        .ok_or(QuotaError::TimeOutOfRange)?;
    u32::try_from(hours)
        .map(RootEpoch::new)
        .map_err(|_| QuotaError::TimeOutOfRange)
}

fn next_epoch(epoch: RootEpoch) -> Result<RootEpoch, QuotaError> {
    epoch
        .get()
        .checked_add(1)
        .map(RootEpoch::new)
        .ok_or(QuotaError::TimeOutOfRange)
}

fn index_of(position: usize) -> Result<MembershipIndex, QuotaError> {
    u32::try_from(position)
        .map(MembershipIndex::new)
        .map_err(|_| QuotaError::Anon(AnonError::TreeFull))
}

fn position_of(index: MembershipIndex) -> Result<usize, QuotaError> {
    usize::try_from(index.get()).map_err(|_| QuotaError::CorruptField { field: "rln_leaf" })
}

/// What the service keeps in memory.
struct State {
    /// The tree as of the latest root.
    tree: MembershipTree,
    /// Every leaf, applied or pending, in index order.
    leaves: Vec<StoredLeaf>,
    /// How many of `leaves` the tree contains.
    applied: usize,
    /// The latest two roots, oldest first; empty before the first roll.
    roots: Vec<HourlyRoot>,
}

impl State {
    fn current_epoch(&self) -> Option<RootEpoch> {
        self.roots.last().map(|root| root.epoch)
    }

    /// Whether `leaf` is zeroed in the root of `epoch`.
    fn zeroed_at(leaf: &StoredLeaf, epoch: Option<RootEpoch>) -> bool {
        matches!((leaf.zeroed_from, epoch), (Some(from), Some(epoch)) if from <= epoch)
    }

    /// The updates that bring the tree from `self.applied` to the root of `epoch`, and the
    /// new applied count.
    fn updates_until(&self, epoch: RootEpoch) -> (Vec<LeafUpdate>, usize) {
        let current = self.current_epoch();
        let appended: Vec<&StoredLeaf> = self
            .leaves
            .iter()
            .skip(self.applied)
            .take_while(|leaf| leaf.live_from <= epoch)
            .collect();
        let applied = self.applied.saturating_add(appended.len());
        let mut updates: Vec<LeafUpdate> = appended
            .iter()
            .map(|leaf| LeafUpdate::Appended(leaf.value))
            .collect();
        updates.extend(
            self.leaves
                .iter()
                .take(applied)
                .filter(|leaf| {
                    Self::zeroed_at(leaf, Some(epoch)) && !Self::zeroed_at(leaf, current)
                })
                .map(|leaf| LeafUpdate::Zeroed(leaf.index)),
        );
        (updates, applied)
    }
}

/// The membership tree service: registration, removal, roots and the public leaf list.
pub struct Membership<S> {
    store: S,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl<S: MembershipStore> Membership<S> {
    /// Loads the tree from `store` and checks it against the latest stored root.
    ///
    /// # Errors
    ///
    /// [`QuotaError::RootMismatch`] if the rebuilt tree does not reach the stored root;
    /// [`QuotaError::Anon`] if a stored leaf is not a field element; storage errors.
    pub fn new(store: S, clock: Arc<dyn Clock>) -> Result<Self, QuotaError> {
        let leaves = store.leaves()?;
        for (position, leaf) in leaves.iter().enumerate() {
            if position_of(leaf.index)? != position {
                return Err(QuotaError::CorruptField { field: "rln_leaf" });
            }
        }
        let roots = store.recent_roots()?;
        let mut state = State {
            tree: MembershipTree::empty()?,
            leaves,
            applied: 0,
            roots: Vec::new(),
        };
        if let Some(latest) = roots.last() {
            let (updates, applied) = state.updates_until(latest.epoch);
            state.tree.apply(&updates)?;
            if state.tree.root().as_bytes() != &latest.root {
                return Err(QuotaError::RootMismatch);
            }
            state.applied = applied;
        }
        state.roots = roots;
        Ok(Self {
            store,
            clock,
            state: Mutex::new(state),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, QuotaError> {
        self.state.lock().map_err(|_| QuotaError::Poisoned)
    }

    /// Locks the state after rolling it to the current root epoch.
    fn rolled(&self) -> Result<MutexGuard<'_, State>, QuotaError> {
        let epoch = root_epoch(self.clock.now())?;
        let mut state = self.lock()?;
        if state
            .current_epoch()
            .is_some_and(|current| current >= epoch)
        {
            return Ok(state);
        }
        let (updates, applied) = state.updates_until(epoch);
        let mut tree = state.tree.clone();
        tree.apply(&updates)?;
        let root = HourlyRoot {
            epoch,
            root: *tree.root().as_bytes(),
        };
        self.store.insert_root(root)?;
        state.tree = tree;
        state.applied = applied;
        state.roots.push(root);
        let skip = state.roots.len().saturating_sub(2);
        state.roots.drain(..skip);
        Ok(state)
    }

    /// Starts the next root epoch if its hour has come. Every other method does this first;
    /// call it hourly so roots appear on time even when nobody asks.
    ///
    /// # Errors
    ///
    /// Storage errors; [`QuotaError::TimeOutOfRange`].
    pub fn roll(&self) -> Result<(), QuotaError> {
        self.rolled().map(drop)
    }

    /// `POST /v1/rln/leaf`: appends the account's leaf, live from the next root epoch. An
    /// account has one live leaf: registering again zeroes the old one at the same time.
    ///
    /// # Errors
    ///
    /// [`QuotaError::Anon`] with [`AnonError::NotAFieldElement`] for a non-canonical
    /// commitment or [`AnonError::TreeFull`]; [`QuotaError::LeafLimit`] if the account
    /// registered [`DAILY_LEAF_LIMIT`] leaves in the last day; storage errors.
    pub fn register(
        &self,
        account: AccountId,
        id_commitment: &IdCommitment,
    ) -> Result<MembershipLeafResponse, QuotaError> {
        let value = id_commitment.leaf()?;
        let mut state = self.rolled()?;
        let current = state.current_epoch().ok_or(QuotaError::TimeOutOfRange)?;
        let live_from = next_epoch(current)?;
        if state.leaves.len() >= TREE_CAPACITY {
            return Err(QuotaError::Anon(AnonError::TreeFull));
        }
        let recent = state
            .leaves
            .iter()
            .filter(|leaf| {
                leaf.account == account
                    && leaf.live_from.get().saturating_add(LEAF_LIMIT_EPOCHS) > live_from.get()
            })
            .count();
        if recent >= DAILY_LEAF_LIMIT {
            return Err(QuotaError::LeafLimit);
        }
        let index = index_of(state.leaves.len())?;
        let replaces = live_leaf(&state, &account).map(|old| Zeroing {
            index: old,
            from: live_from,
        });
        let leaf = StoredLeaf {
            index,
            value,
            account,
            live_from,
            zeroed_from: None,
        };
        self.store.register(&leaf, replaces)?;
        if let Some(zeroing) = replaces {
            mark_zeroed(&mut state, zeroing)?;
        }
        state.leaves.push(leaf);
        Ok(MembershipLeafResponse { index, live_from })
    }

    /// Removes an account's leaf (account deleted or banned): zeroed at the next root, so its
    /// proofs stop verifying once the previous root ages out, at most two hours later.
    /// Removing an account with no live leaf does nothing.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn remove(&self, account: &AccountId) -> Result<(), QuotaError> {
        let mut state = self.rolled()?;
        let Some(index) = live_leaf(&state, account) else {
            return Ok(());
        };
        let current = state.current_epoch().ok_or(QuotaError::TimeOutOfRange)?;
        let zeroing = Zeroing {
            index,
            from: next_epoch(current)?,
        };
        self.store.zero(zeroing)?;
        mark_zeroed(&mut state, zeroing)
    }

    /// The roots proofs may use now: the current and the previous hourly root.
    ///
    /// # Errors
    ///
    /// Storage errors; [`QuotaError::TimeOutOfRange`].
    pub fn accepted_roots(&self) -> Result<AcceptedRoots, QuotaError> {
        let state = self.rolled()?;
        let value = |root: &HourlyRoot| MembershipRoot::from_bytes(root.root);
        let mut roots = state.roots.iter().rev();
        let current = roots.next().map(value).ok_or(QuotaError::TimeOutOfRange)?;
        let accepted = AcceptedRoots::first(current);
        Ok(match roots.next().map(value) {
            Some(previous) => AcceptedRoots::first(previous).advance(current),
            None => accepted,
        })
    }

    /// `GET /v1/rln/leaves?after=n`: the leaves from position `after` on that the current root
    /// contains (at most [`LEAVES_PER_PAGE`]), every zeroed position, and the recent roots.
    ///
    /// # Errors
    ///
    /// Storage errors; [`QuotaError::TimeOutOfRange`].
    pub fn leaves(&self, after: MembershipIndex) -> Result<MembershipLeaves, QuotaError> {
        let state = self.rolled()?;
        let start = position_of(after)?.min(state.applied);
        let leaves: Vec<[u8; 32]> = state
            .leaves
            .iter()
            .take(state.applied)
            .skip(start)
            .take(LEAVES_PER_PAGE)
            .map(|leaf| *leaf.value.as_bytes())
            .collect();
        let current = state.current_epoch();
        let zeroed = state
            .leaves
            .iter()
            .take(state.applied)
            .filter(|leaf| State::zeroed_at(leaf, current))
            .map(|leaf| leaf.index)
            .collect();
        Ok(MembershipLeaves {
            next: index_of(start.saturating_add(leaves.len()))?,
            leaves,
            zeroed,
            roots: state.roots.clone(),
        })
    }
}

/// The account's leaf that is not zeroed or being zeroed.
fn live_leaf(state: &State, account: &AccountId) -> Option<MembershipIndex> {
    state
        .leaves
        .iter()
        .rev()
        .find(|leaf| leaf.account == *account && leaf.zeroed_from.is_none())
        .map(|leaf| leaf.index)
}

fn mark_zeroed(state: &mut State, zeroing: Zeroing) -> Result<(), QuotaError> {
    let position = position_of(zeroing.index)?;
    let leaf = state
        .leaves
        .get_mut(position)
        .ok_or(QuotaError::CorruptField { field: "rln_leaf" })?;
    leaf.zeroed_from = Some(zeroing.from);
    Ok(())
}
