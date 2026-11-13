// SPDX-License-Identifier: AGPL-3.0-only
//! The roster as knock verification sees it (`pingers_presence::RosterLookup`), and as the
//! roster screen sees it.

use pingers_crypto::AccountPublicKey;
use pingers_mls::Roster;
use pingers_presence::RosterLookup;
use pingers_proto::{Epoch, LeafIndex};
use pingers_store::{MemberName, Verification};

/// A Party's rosters at the epochs this device keeps (the current one and the one before),
/// loaded from the store in one go so knock verification does no I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterView {
    current: Epoch,
    rosters: Vec<EpochRoster>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EpochRoster {
    epoch: Epoch,
    roster: Roster,
}

impl RosterView {
    pub(crate) fn new(current: Epoch, rosters: impl IntoIterator<Item = (Epoch, Roster)>) -> Self {
        Self {
            current,
            rosters: rosters
                .into_iter()
                .map(|(epoch, roster)| EpochRoster { epoch, roster })
                .collect(),
        }
    }

    /// The roster at `epoch`, if this device still keeps it.
    #[must_use]
    pub fn at(&self, epoch: Epoch) -> Option<&Roster> {
        self.rosters
            .iter()
            .find(|kept| kept.epoch == epoch)
            .map(|kept| &kept.roster)
    }
}

impl RosterLookup for RosterView {
    fn signature_key(&self, leaf: LeafIndex, epoch: Epoch) -> Option<AccountPublicKey> {
        self.at(epoch)?.signature_key(leaf).copied()
    }

    fn current_epoch(&self) -> Epoch {
        self.current
    }
}

/// One member as the roster screen shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterMember {
    /// Their leaf in the Party group.
    pub leaf: LeafIndex,
    /// Their account key.
    pub signature_key: AccountPublicKey,
    /// Their display name, once their roster profile has arrived.
    pub name: Option<MemberName>,
    /// Whether this device checked their safety code.
    pub verification: Verification,
    /// Whether this is this device.
    pub is_self: Myself,
}

/// Whether a roster member is this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Myself {
    /// This device's own leaf.
    Yes,
    /// Another member.
    No,
}

#[cfg(test)]
mod tests {
    use pingers_mls::RosterEntry;

    use super::*;

    #[test]
    fn lookups_use_the_roster_of_the_asked_epoch() {
        let entry = |leaf, fill| RosterEntry {
            leaf: LeafIndex::new(leaf),
            signature_key: AccountPublicKey::from_bytes([fill; 32]),
        };
        let view = RosterView::new(
            Epoch::new(5),
            [
                (Epoch::new(5), Roster::new(vec![entry(0, 1)])),
                (Epoch::new(4), Roster::new(vec![entry(0, 1), entry(1, 2)])),
            ],
        );
        assert_eq!(view.current_epoch(), Epoch::new(5));
        assert_eq!(
            view.signature_key(LeafIndex::new(1), Epoch::new(4)),
            Some(AccountPublicKey::from_bytes([2; 32]))
        );
        assert_eq!(view.signature_key(LeafIndex::new(1), Epoch::new(5)), None);
        assert_eq!(view.signature_key(LeafIndex::new(0), Epoch::new(3)), None);
    }
}
