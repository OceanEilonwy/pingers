// SPDX-License-Identifier: AGPL-3.0-only
//! A Party's roster, as the Meetup roster check needs it.

use pingers_crypto::AccountPublicKey;
use pingers_proto::LeafIndex;

/// One member of a group: its leaf and its signature key (its account key, design B.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RosterEntry {
    /// The member's leaf.
    pub leaf: LeafIndex,
    /// The member's leaf signature key.
    pub signature_key: AccountPublicKey,
}

/// The members of a group at one epoch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roster {
    entries: Vec<RosterEntry>,
}

impl Roster {
    /// A roster from its entries.
    #[must_use]
    pub const fn new(entries: Vec<RosterEntry>) -> Self {
        Self { entries }
    }

    /// The entries, in leaf order.
    #[must_use]
    pub fn entries(&self) -> &[RosterEntry] {
        &self.entries
    }

    /// The signature key of the member at `leaf`, if there is one.
    #[must_use]
    pub fn signature_key(&self, leaf: LeafIndex) -> Option<&AccountPublicKey> {
        self.entries
            .iter()
            .find(|entry| entry.leaf == leaf)
            .map(|entry| &entry.signature_key)
    }

    /// Whether some member signs with `key`: the Meetup roster check (design B.5).
    #[must_use]
    pub fn contains_key(&self, key: &AccountPublicKey) -> bool {
        self.entries.iter().any(|entry| entry.signature_key == *key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_check_finds_members_by_key() {
        let member = AccountPublicKey::from_bytes([1; 32]);
        let roster = Roster::new(vec![RosterEntry {
            leaf: LeafIndex::new(0),
            signature_key: member,
        }]);
        assert!(roster.contains_key(&member));
        assert!(!roster.contains_key(&AccountPublicKey::from_bytes([2; 32])));
        assert_eq!(roster.signature_key(LeafIndex::new(0)), Some(&member));
    }
}
