// SPDX-License-Identifier: AGPL-3.0-only
//! The meet point: last writer wins by `(lamport, sender leaf)` (design B.6, B.6.1).

use pingers_geo::Coordinate;
use pingers_proto::{Lamport, LeafIndex, MeetPoint, MeetPointLabel};

/// What the user asks for (design E.1, `MeetPointDraft`).
#[derive(Clone, Debug, PartialEq)]
pub enum MeetPointDraft {
    /// A fixed place.
    Fixed {
        /// Where.
        position: Coordinate,
        /// What to call it.
        label: Option<MeetPointLabel>,
    },
    /// "Meet at me": this member's live position. Implicitly grants everyone this member's
    /// location while it stays the meet point; the UI confirms first.
    AtMe,
    /// Remove the meet point.
    Clear,
}

/// Whether an incoming meet point changed the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum MeetPointUpdate {
    /// It won and is now current.
    Applied,
    /// It lost to the current one, or was an `AtMember` not sent by that member.
    Ignored,
}

/// The current meet point and who set it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeetPointRegister {
    current: Option<Written>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Written {
    point: MeetPoint,
    sender: LeafIndex,
}

impl MeetPointRegister {
    /// An empty register.
    #[must_use]
    pub const fn new() -> Self {
        Self { current: None }
    }

    /// The current meet point. `Cleared` is reported as `None`.
    #[must_use]
    pub fn current(&self) -> Option<&MeetPoint> {
        match &self.current {
            Some(Written {
                point: MeetPoint::Cleared { .. },
                ..
            })
            | None => None,
            Some(Written { point, .. }) => Some(point),
        }
    }

    /// The highest Lamport value seen, for stamping this member's next write.
    #[must_use]
    pub fn lamport(&self) -> Lamport {
        self.current
            .as_ref()
            .map_or(Lamport::new(0), |written| lamport_of(&written.point))
    }

    /// Applies a meet point sent by `sender`. `AtMember { leaf }` counts only when
    /// `sender == leaf`. Ties on Lamport go to the higher sender leaf.
    pub fn apply(&mut self, sender: LeafIndex, point: MeetPoint) -> MeetPointUpdate {
        if let MeetPoint::AtMember { leaf, .. } = &point
            && *leaf != sender
        {
            return MeetPointUpdate::Ignored;
        }
        let incoming = (lamport_of(&point), sender);
        let wins = self
            .current
            .as_ref()
            .is_none_or(|written| incoming > (lamport_of(&written.point), written.sender));
        if wins {
            self.current = Some(Written { point, sender });
            MeetPointUpdate::Applied
        } else {
            MeetPointUpdate::Ignored
        }
    }
}

const fn lamport_of(point: &MeetPoint) -> Lamport {
    match point {
        MeetPoint::Fixed { lamport, .. }
        | MeetPoint::AtMember { lamport, .. }
        | MeetPoint::Cleared { lamport } => *lamport,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_member(leaf: u32, lamport: u64) -> MeetPoint {
        MeetPoint::AtMember {
            leaf: LeafIndex::new(leaf),
            lamport: Lamport::new(lamport),
        }
    }

    #[test]
    fn meet_at_me_from_someone_else_is_ignored() {
        let mut register = MeetPointRegister::new();
        assert_eq!(
            register.apply(LeafIndex::new(2), at_member(1, 1)),
            MeetPointUpdate::Ignored
        );
        assert_eq!(register.current(), None);
    }

    #[test]
    fn later_lamport_wins() {
        let mut register = MeetPointRegister::new();
        assert_eq!(
            register.apply(LeafIndex::new(1), at_member(1, 5)),
            MeetPointUpdate::Applied
        );
        assert_eq!(
            register.apply(LeafIndex::new(2), at_member(2, 4)),
            MeetPointUpdate::Ignored
        );
        assert_eq!(
            register.apply(LeafIndex::new(2), at_member(2, 6)),
            MeetPointUpdate::Applied
        );
        assert_eq!(register.lamport(), Lamport::new(6));
    }

    #[test]
    fn equal_lamport_goes_to_the_higher_leaf() {
        let mut register = MeetPointRegister::new();
        assert_eq!(
            register.apply(LeafIndex::new(3), at_member(3, 5)),
            MeetPointUpdate::Applied
        );
        assert_eq!(
            register.apply(LeafIndex::new(2), at_member(2, 5)),
            MeetPointUpdate::Ignored
        );
    }

    #[test]
    fn the_same_write_twice_is_applied_once() {
        let mut register = MeetPointRegister::new();
        assert_eq!(
            register.apply(LeafIndex::new(1), at_member(1, 3)),
            MeetPointUpdate::Applied
        );
        assert_eq!(
            register.apply(LeafIndex::new(1), at_member(1, 3)),
            MeetPointUpdate::Ignored
        );
    }

    #[test]
    fn cleared_meet_point_reads_as_none() {
        let mut register = MeetPointRegister::new();
        assert_eq!(
            register.apply(LeafIndex::new(1), at_member(1, 1)),
            MeetPointUpdate::Applied
        );
        let cleared = MeetPoint::Cleared {
            lamport: Lamport::new(2),
        };
        assert_eq!(
            register.apply(LeafIndex::new(1), cleared),
            MeetPointUpdate::Applied
        );
        assert_eq!(register.current(), None);
    }
}
