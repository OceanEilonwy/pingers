// SPDX-License-Identifier: AGPL-3.0-only
//! The membership tree: one leaf per registered device, depth 20 (design B.4.8).
//!
//! The client keeps the whole tree and computes its own path, so it never asks the server for
//! "my path", which would name it.

use crate::stub::{Unimplemented, not_implemented};
use crate::{AnonError, LeafValue, MembershipRoot};

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
    unimplemented: Unimplemented,
}

/// The membership tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipTree {
    unimplemented: Unimplemented,
}

impl MembershipTree {
    /// An empty tree.
    ///
    /// # Errors
    ///
    /// [`AnonError::Parameters`] if the hash parameters fail to load.
    pub fn empty() -> Result<Self, AnonError> {
        not_implemented(())
    }

    /// Applies updates in order.
    ///
    /// # Errors
    ///
    /// [`AnonError::TreeOutOfOrder`], [`AnonError::TreeFull`], [`AnonError::NoSuchLeaf`].
    pub fn apply(&mut self, updates: &[LeafUpdate]) -> Result<(), AnonError> {
        let _ = updates;
        match self.unimplemented {}
    }

    /// The current root.
    #[must_use]
    pub fn root(&self) -> MembershipRoot {
        match self.unimplemented {}
    }

    /// How many leaves have been appended (live or zeroed).
    #[must_use]
    pub fn leaf_count(&self) -> u32 {
        match self.unimplemented {}
    }

    /// The authentication path for `position`.
    ///
    /// # Errors
    ///
    /// [`AnonError::NoSuchLeaf`] if the position has no leaf.
    pub fn path(&self, position: LeafPosition) -> Result<MerklePath, AnonError> {
        let _ = position;
        match self.unimplemented {}
    }
}
