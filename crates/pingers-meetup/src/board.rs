// SPDX-License-Identifier: AGPL-3.0-only
//! Cross-checking the board segment hashes members saw (design B.4.3): a server that shows
//! different members different boards is caught when they meet.

use std::collections::BTreeMap;

use pingers_proto::{LeafIndex, SeenSegment};

/// This device's segment hashes and those members reported.
#[derive(Clone, Debug, Default)]
pub(crate) struct BoardCheck {
    own: Vec<SeenSegment>,
    peers: BTreeMap<LeafIndex, Vec<SeenSegment>>,
}

/// Whether two reports disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Agreement {
    /// No segment with the same start has a different hash.
    Consistent,
    /// Some segment does: the server equivocated.
    Equivocation,
}

impl BoardCheck {
    /// Records this device's own hashes and checks them against every report so far.
    pub(crate) fn set_own(&mut self, segments: Vec<SeenSegment>) -> Agreement {
        self.own = segments;
        if self.peers.values().any(|peer| differ(&self.own, peer)) {
            Agreement::Equivocation
        } else {
            Agreement::Consistent
        }
    }

    /// Records a member's report and checks it against this device's hashes.
    pub(crate) fn peer_seen(&mut self, leaf: LeafIndex, segments: Vec<SeenSegment>) -> Agreement {
        let agreement = if differ(&self.own, &segments) {
            Agreement::Equivocation
        } else {
            Agreement::Consistent
        };
        self.peers.insert(leaf, segments);
        agreement
    }

    pub(crate) fn forget(&mut self, leaf: LeafIndex) {
        self.peers.remove(&leaf);
    }
}

fn differ(ours: &[SeenSegment], theirs: &[SeenSegment]) -> bool {
    ours.iter().any(|own| {
        theirs
            .iter()
            .any(|peer| peer.start == own.start && peer.hash != own.hash)
    })
}

#[cfg(test)]
mod tests {
    use pingers_proto::SegmentHash;
    use pingers_time::UnixSeconds;

    use super::*;

    fn segment(start: u64, hash: u8) -> SeenSegment {
        SeenSegment {
            start: UnixSeconds::new(start).bucket(),
            hash: SegmentHash::from_bytes([hash; 32]),
        }
    }

    #[test]
    fn the_same_segment_with_another_hash_is_equivocation() {
        let mut check = BoardCheck::default();
        assert_eq!(check.set_own(vec![segment(0, 1)]), Agreement::Consistent);
        assert_eq!(
            check.peer_seen(LeafIndex::new(1), vec![segment(0, 2)]),
            Agreement::Equivocation
        );
    }

    #[test]
    fn different_segments_are_not_compared() {
        let mut check = BoardCheck::default();
        assert_eq!(check.set_own(vec![segment(0, 1)]), Agreement::Consistent);
        assert_eq!(
            check.peer_seen(LeafIndex::new(1), vec![segment(3_600, 2), segment(0, 1)]),
            Agreement::Consistent
        );
    }

    #[test]
    fn a_report_before_our_own_is_checked_when_ours_arrives() {
        let mut check = BoardCheck::default();
        assert_eq!(
            check.peer_seen(LeafIndex::new(1), vec![segment(0, 2)]),
            Agreement::Consistent
        );
        assert_eq!(check.set_own(vec![segment(0, 1)]), Agreement::Equivocation);
    }

    #[test]
    fn a_member_who_left_is_no_longer_compared() {
        let mut check = BoardCheck::default();
        let _first = check.peer_seen(LeafIndex::new(1), vec![segment(0, 2)]);
        check.forget(LeafIndex::new(1));
        assert_eq!(check.set_own(vec![segment(0, 1)]), Agreement::Consistent);
    }
}
