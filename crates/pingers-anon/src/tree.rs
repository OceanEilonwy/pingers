// SPDX-License-Identifier: AGPL-3.0-only
//! The membership tree: one leaf per registered device, depth 20 (design B.4.8).
//!
//! The client keeps the whole tree and computes its own path, so it never asks the server for
//! "my path", which would name it.
//!
//! An in-memory Poseidon Merkle tree matching the RLN-v2 circuit: empty leaves are zero, a
//! parent is `Poseidon(left, right)`. Only the populated prefix of each level is stored; the
//! rest is the precomputed root of an empty subtree.

use rln::circuit::Fr;

use crate::{AnonError, LeafValue, MembershipRoot, field};

/// Tree depth: room for 2^20 devices. Fixed by the circuit.
pub(crate) const DEPTH: usize = 20;

/// The number of leaf positions.
const CAPACITY: usize = 1 << DEPTH;

/// A leaf's position in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeafPosition(u32);

impl LeafPosition {
    /// Wraps a position from the wire (`idx`).
    #[must_use]
    pub const fn new(position: u32) -> Self {
        Self(position)
    }

    /// The position.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One change from `GET /v1/rln/leaves`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafUpdate {
    /// A leaf appended at the next position.
    Appended(LeafValue),
    /// A leaf zeroed (account deleted, slashed or banned).
    Zeroed(LeafPosition),
}

/// A Merkle authentication path for one leaf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MerklePath {
    /// Sibling hashes from the leaf level up; always [`DEPTH`] of them.
    siblings: Vec<Fr>,
    /// The leaf's position; bit `i` says whether the node at level `i` is a right child.
    position: LeafPosition,
}

impl MerklePath {
    /// The leaf this path is for.
    #[must_use]
    pub const fn position(&self) -> LeafPosition {
        self.position
    }

    /// The sibling hashes, leaf level first.
    #[cfg(any(feature = "prover", test))]
    pub(crate) fn siblings(&self) -> &[Fr] {
        &self.siblings
    }

    /// The circuit's `identityPathIndex`: 1 where the path node is a right child.
    #[cfg(any(feature = "prover", test))]
    pub(crate) fn index_bits(&self) -> Vec<u8> {
        (0..DEPTH)
            .map(|level| u8::from((self.position.0 >> level) & 1 == 1))
            .collect()
    }

    /// The root this path leads to from `leaf`.
    #[cfg(any(feature = "prover", test))]
    pub(crate) fn root_from(&self, leaf: Fr) -> Fr {
        self.siblings
            .iter()
            .zip(self.index_bits())
            .fold(leaf, |node, (sibling, bit)| {
                if bit == 1 {
                    field::poseidon(&[*sibling, node])
                } else {
                    field::poseidon(&[node, *sibling])
                }
            })
    }
}

/// The membership tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipTree {
    /// `levels[0]` holds the leaves; `levels[h]` the populated prefix of level `h`, so
    /// `levels[DEPTH]` holds the root once any leaf exists. Always `DEPTH + 1` levels.
    levels: Vec<Vec<Fr>>,
    /// `empty[h]`: the root of an empty subtree of height `h`. Always `DEPTH + 1` entries.
    empty: Vec<Fr>,
}

impl MembershipTree {
    /// An empty tree.
    ///
    /// # Errors
    ///
    /// [`AnonError::Parameters`] if the hash parameters fail to load.
    pub fn empty() -> Result<Self, AnonError> {
        let mut empty = Vec::with_capacity(DEPTH.saturating_add(1));
        let mut node = Fr::from(0_u64);
        empty.push(node);
        for _ in 0..DEPTH {
            node = field::poseidon(&[node, node]);
            empty.push(node);
        }
        Ok(Self {
            levels: vec![Vec::new(); DEPTH.saturating_add(1)],
            empty,
        })
    }

    /// Applies updates in order. Either all of them apply or, on an error, none do.
    ///
    /// # Errors
    ///
    /// [`AnonError::TreeFull`] past 2^20 leaves; [`AnonError::NoSuchLeaf`] when zeroing a
    /// position not yet appended; [`AnonError::NotAFieldElement`] for a non-canonical leaf.
    pub fn apply(&mut self, updates: &[LeafUpdate]) -> Result<(), AnonError> {
        let mut leaves = self.levels.first().cloned().unwrap_or_default();
        let mut dirty: Vec<usize> = Vec::with_capacity(updates.len());
        for update in updates {
            match update {
                LeafUpdate::Appended(value) => {
                    if leaves.len() >= CAPACITY {
                        return Err(AnonError::TreeFull);
                    }
                    dirty.push(leaves.len());
                    leaves.push(field::decode(value.as_bytes())?);
                }
                LeafUpdate::Zeroed(position) => {
                    let index = usize::try_from(position.0).map_err(|_| AnonError::NoSuchLeaf)?;
                    let leaf = leaves.get_mut(index).ok_or(AnonError::NoSuchLeaf)?;
                    *leaf = Fr::from(0_u64);
                    dirty.push(index);
                }
            }
        }
        let mut levels = self.levels.clone();
        if let Some(level) = levels.first_mut() {
            *level = leaves;
        }
        self.levels = self.rehash(levels, dirty)?;
        Ok(())
    }

    /// Recomputes the ancestors of the `dirty` leaf indices one level at a time, so a batch
    /// of n updates costs about 2n hashes rather than 20n.
    fn rehash(
        &self,
        mut levels: Vec<Vec<Fr>>,
        mut dirty: Vec<usize>,
    ) -> Result<Vec<Vec<Fr>>, AnonError> {
        for height in 0..DEPTH {
            dirty = dirty.into_iter().map(|index| index >> 1).collect();
            dirty.sort_unstable();
            dirty.dedup();
            let empty = *self.empty.get(height).ok_or(AnonError::Internal)?;
            let children = levels.get(height).ok_or(AnonError::Internal)?;
            let mut parents = levels
                .get(height.saturating_add(1))
                .ok_or(AnonError::Internal)?
                .clone();
            parents.resize(children.len().div_ceil(2), empty);
            for &parent in &dirty {
                let left_index = parent.checked_mul(2).ok_or(AnonError::Internal)?;
                let left = *children.get(left_index).ok_or(AnonError::Internal)?;
                let right = children
                    .get(left_index.saturating_add(1))
                    .copied()
                    .unwrap_or(empty);
                *parents.get_mut(parent).ok_or(AnonError::Internal)? =
                    field::poseidon(&[left, right]);
            }
            *levels
                .get_mut(height.saturating_add(1))
                .ok_or(AnonError::Internal)? = parents;
        }
        Ok(levels)
    }

    /// The current root.
    #[must_use]
    pub fn root(&self) -> MembershipRoot {
        let root = self
            .levels
            .get(DEPTH)
            .and_then(|level| level.first())
            .or_else(|| self.empty.get(DEPTH))
            .copied()
            .unwrap_or_default();
        MembershipRoot::from_bytes(field::encode(root))
    }

    /// How many leaves have been appended (live or zeroed).
    #[must_use]
    pub fn leaf_count(&self) -> u32 {
        // `apply` stores at most 2^20 leaves, so the conversion cannot fail.
        self.levels
            .first()
            .map_or(0, |leaves| u32::try_from(leaves.len()).unwrap_or(u32::MAX))
    }

    /// The authentication path for `position`.
    ///
    /// # Errors
    ///
    /// [`AnonError::NoSuchLeaf`] if the position has no leaf.
    pub fn path(&self, position: LeafPosition) -> Result<MerklePath, AnonError> {
        if position.0 >= self.leaf_count() {
            return Err(AnonError::NoSuchLeaf);
        }
        let mut index = usize::try_from(position.0).map_err(|_| AnonError::NoSuchLeaf)?;
        let mut siblings = Vec::with_capacity(DEPTH);
        for height in 0..DEPTH {
            let empty = *self.empty.get(height).ok_or(AnonError::Internal)?;
            let sibling = self
                .levels
                .get(height)
                .and_then(|level| level.get(index ^ 1))
                .copied()
                .unwrap_or(empty);
            siblings.push(sibling);
            index >>= 1;
        }
        Ok(MerklePath { siblings, position })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn leaf(value: u64) -> LeafUpdate {
        LeafUpdate::Appended(LeafValue::from_bytes(field::encode(Fr::from(value))))
    }

    fn leaf_at(tree: &MembershipTree, position: u32) -> Result<Fr, AnonError> {
        let index = usize::try_from(position).map_err(|_| AnonError::Internal)?;
        tree.levels
            .first()
            .and_then(|leaves| leaves.get(index))
            .copied()
            .ok_or(AnonError::Internal)
    }

    /// Folds every leaf's path and compares with the stored root.
    fn assert_paths_lead_to_root(tree: &MembershipTree) -> Result<(), AnonError> {
        let root = field::decode(tree.root().as_bytes())?;
        for position in 0..tree.leaf_count() {
            let path = tree.path(LeafPosition::new(position))?;
            assert_eq!(path.position(), LeafPosition::new(position));
            assert_eq!(path.siblings().len(), DEPTH);
            assert_eq!(
                path.root_from(leaf_at(tree, position)?),
                root,
                "leaf {position}"
            );
        }
        Ok(())
    }

    #[test]
    fn positions_keep_their_value() {
        assert_eq!(LeafPosition::new(77).get(), 77);
    }

    #[test]
    fn empty_root_is_the_zero_subtree_root() -> Result<(), AnonError> {
        let tree = MembershipTree::empty()?;
        let mut node = Fr::from(0_u64);
        for _ in 0..DEPTH {
            node = field::poseidon(&[node, node]);
        }
        assert_eq!(tree.root(), MembershipRoot::from_bytes(field::encode(node)));
        assert_eq!(tree.leaf_count(), 0);
        Ok(())
    }

    #[test]
    fn single_leaf_root_hashes_up_the_left_edge() -> Result<(), AnonError> {
        let mut tree = MembershipTree::empty()?;
        tree.apply(&[leaf(7)])?;
        let mut node = Fr::from(7_u64);
        let mut empty = Fr::from(0_u64);
        for _ in 0..DEPTH {
            node = field::poseidon(&[node, empty]);
            empty = field::poseidon(&[empty, empty]);
        }
        assert_eq!(tree.root(), MembershipRoot::from_bytes(field::encode(node)));
        Ok(())
    }

    #[test]
    fn every_path_leads_to_the_root() -> Result<(), AnonError> {
        let mut tree = MembershipTree::empty()?;
        tree.apply(&(1..=11).map(leaf).collect::<Vec<_>>())?;
        assert_eq!(tree.leaf_count(), 11);
        assert_paths_lead_to_root(&tree)?;
        tree.apply(&[LeafUpdate::Zeroed(LeafPosition::new(4)), leaf(12)])?;
        assert_eq!(tree.leaf_count(), 12);
        assert_paths_lead_to_root(&tree)
    }

    #[test]
    fn batched_and_one_by_one_updates_agree() -> Result<(), AnonError> {
        let updates: Vec<LeafUpdate> = (1..=9)
            .map(leaf)
            .chain([LeafUpdate::Zeroed(LeafPosition::new(2))])
            .collect();
        let mut batched = MembershipTree::empty()?;
        batched.apply(&updates)?;
        let mut stepwise = MembershipTree::empty()?;
        for update in &updates {
            stepwise.apply(core::slice::from_ref(update))?;
        }
        assert_eq!(batched, stepwise);
        Ok(())
    }

    #[test]
    fn zeroing_a_leaf_matches_appending_zero() -> Result<(), AnonError> {
        let mut zeroed = MembershipTree::empty()?;
        zeroed.apply(&[leaf(1), leaf(2), LeafUpdate::Zeroed(LeafPosition::new(1))])?;
        let mut direct = MembershipTree::empty()?;
        direct.apply(&[leaf(1), leaf(0)])?;
        assert_eq!(zeroed.root(), direct.root());
        assert_eq!(zeroed.leaf_count(), 2);
        Ok(())
    }

    #[test]
    fn zeroing_an_unknown_position_fails_and_changes_nothing() -> Result<(), AnonError> {
        let mut tree = MembershipTree::empty()?;
        tree.apply(&[leaf(1)])?;
        let before = tree.clone();
        let result = tree.apply(&[leaf(2), LeafUpdate::Zeroed(LeafPosition::new(2))]);
        assert!(matches!(result, Err(AnonError::NoSuchLeaf)));
        assert_eq!(tree, before);
        Ok(())
    }

    #[test]
    fn a_non_canonical_leaf_is_rejected() -> Result<(), AnonError> {
        let mut tree = MembershipTree::empty()?;
        let result = tree.apply(&[LeafUpdate::Appended(LeafValue::from_bytes([0xff; 32]))]);
        assert!(matches!(result, Err(AnonError::NotAFieldElement)));
        assert_eq!(tree.leaf_count(), 0);
        Ok(())
    }

    #[test]
    fn paths_exist_only_for_appended_leaves() -> Result<(), AnonError> {
        let mut tree = MembershipTree::empty()?;
        assert!(matches!(
            tree.path(LeafPosition::new(0)),
            Err(AnonError::NoSuchLeaf)
        ));
        tree.apply(&[leaf(1), leaf(2)])?;
        assert!(tree.path(LeafPosition::new(1)).is_ok());
        assert!(matches!(
            tree.path(LeafPosition::new(2)),
            Err(AnonError::NoSuchLeaf)
        ));
        Ok(())
    }

    #[test]
    fn path_index_bits_are_the_position_little_endian() -> Result<(), AnonError> {
        let mut tree = MembershipTree::empty()?;
        tree.apply(&(0..6).map(leaf).collect::<Vec<_>>())?;
        let bits = tree.path(LeafPosition::new(5))?.index_bits();
        assert_eq!(bits.get(..4), Some([1, 0, 1, 0].as_slice()));
        assert_eq!(bits.len(), DEPTH);
        Ok(())
    }

    #[test]
    fn the_tree_fills_at_two_to_the_twenty() -> Result<(), AnonError> {
        let mut tree = MembershipTree::empty()?;
        // Fill without hashing: the capacity check only reads the leaf count.
        if let Some(leaves) = tree.levels.first_mut() {
            leaves.resize(CAPACITY.saturating_sub(1), Fr::from(0_u64));
        }
        tree.apply(&[leaf(1)])?;
        assert_eq!(tree.leaf_count(), 1 << DEPTH);
        assert!(matches!(tree.apply(&[leaf(2)]), Err(AnonError::TreeFull)));
        Ok(())
    }
}
