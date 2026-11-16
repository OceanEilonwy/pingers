// SPDX-License-Identifier: AGPL-3.0-only
//! The meet point across devices: last writer wins by `(lamport, leaf)`, `AtMember` only
//! from that member, and late joiners (design B.5, B.6, B.6.1).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_geo::Coordinate;
use pingers_proto::{E7Coordinate, Lamport, MeetPoint, MeetPointLabel, MeetupMessage};

use super::{ALICE, BEN, CHLOE, DAN, Net, Setup, TestResult, asking};
use crate::{LocationVisibility, MeetPointDraft, MeetupAction, MeetupEvent};

fn fixed(latitude: f64, longitude: f64) -> Result<MeetPointDraft, super::TestError> {
    Ok(MeetPointDraft::Fixed {
        position: Coordinate::new(latitude, longitude)?,
        label: Some(MeetPointLabel::new("Fountain".to_owned())?),
    })
}

fn changed(events: &[MeetupEvent]) -> usize {
    events
        .iter()
        .filter(|event| **event == MeetupEvent::MeetPointChanged)
        .count()
}

#[test]
fn a_meet_point_reaches_everyone_with_the_next_lamport() -> TestResult {
    let mut net = Net::new(&asking(3), 60)?;
    net.act(BEN, MeetupAction::SetMeetPoint(fixed(1.0, 2.0)?))?;
    for leaf in [ALICE, BEN, CHLOE] {
        let point = net.session(leaf)?.meet_point().cloned();
        assert_eq!(
            point,
            Some(MeetPoint::Fixed {
                position: E7Coordinate::new(10_000_000, 20_000_000)?,
                label: Some(MeetPointLabel::new("Fountain".to_owned())?),
                lamport: Lamport::new(1),
            })
        );
        // Every device knows who set it, for "set by Ben".
        assert_eq!(net.session(leaf)?.meet_point_set_by(), Some(BEN));
        assert_eq!(changed(&net.take_events(leaf)?), 1);
    }
    // The next write, from anyone, has a higher Lamport value.
    net.act(ALICE, MeetupAction::SetMeetPoint(MeetPointDraft::Clear))?;
    assert_eq!(net.session(CHLOE)?.meet_point(), None);
    Ok(())
}

#[test]
fn concurrent_writes_converge_on_the_higher_leaf() -> TestResult {
    let mut net = Net::new(&asking(3), 61)?;
    // Alice and Chloé both write Lamport 1 before seeing each other's write.
    let alice = MeetPoint::Fixed {
        position: E7Coordinate::new(1, 1)?,
        label: None,
        lamport: Lamport::new(1),
    };
    let chloe = MeetPoint::AtMember {
        leaf: CHLOE,
        lamport: Lamport::new(1),
    };
    net.deliver(ALICE, BEN, MeetupMessage::MeetPoint(alice.clone()))?;
    net.deliver(CHLOE, BEN, MeetupMessage::MeetPoint(chloe.clone()))?;
    net.deliver(CHLOE, ALICE, MeetupMessage::MeetPoint(chloe.clone()))?;
    net.deliver(ALICE, CHLOE, MeetupMessage::MeetPoint(alice))?;
    for leaf in [ALICE, BEN] {
        assert_eq!(net.session(leaf)?.meet_point(), Some(&chloe));
    }
    Ok(())
}

#[test]
fn an_older_write_arriving_late_changes_nothing() -> TestResult {
    let mut net = Net::new(&asking(2), 62)?;
    net.act(ALICE, MeetupAction::SetMeetPoint(MeetPointDraft::Clear))?;
    net.act(ALICE, MeetupAction::SetMeetPoint(fixed(3.0, 4.0)?))?;
    net.take_events(BEN)?;
    net.deliver(
        ALICE,
        BEN,
        MeetupMessage::MeetPoint(MeetPoint::Cleared {
            lamport: Lamport::new(1),
        }),
    )?;
    assert!(net.session(BEN)?.meet_point().is_some());
    assert_eq!(changed(&net.take_events(BEN)?), 0);
    Ok(())
}

#[test]
fn meet_at_someone_else_is_ignored() -> TestResult {
    let mut net = Net::new(&asking(3), 63)?;
    net.deliver(
        ALICE,
        CHLOE,
        MeetupMessage::MeetPoint(MeetPoint::AtMember {
            leaf: BEN,
            lamport: Lamport::new(5),
        }),
    )?;
    assert_eq!(net.session(CHLOE)?.meet_point(), None);
    assert_eq!(changed(&net.take_events(CHLOE)?), 0);
    Ok(())
}

#[test]
fn a_late_joiner_gets_the_meet_point_from_the_lowest_member() -> TestResult {
    let mut net = Net::new(&asking(3), 64)?;
    net.act(CHLOE, MeetupAction::SetMeetPoint(fixed(5.0, 6.0)?))?;
    for leaf in [ALICE, BEN, CHLOE] {
        net.take_sent(leaf)?;
    }
    net.add(DAN, &Setup::with(LocationVisibility::AskEachTime))?;
    let snapshots = |sent: &[super::Sent]| super::count(sent, "snapshot");
    assert_eq!(snapshots(&net.take_sent(ALICE)?), 1);
    assert_eq!(snapshots(&net.take_sent(BEN)?), 0);
    assert_eq!(snapshots(&net.take_sent(CHLOE)?), 0);
    assert_eq!(
        net.session(DAN)?.meet_point(),
        net.session(ALICE)?.meet_point()
    );
    assert!(net.session(DAN)?.meet_point().is_some());
    Ok(())
}

#[test]
fn a_late_joiner_learns_the_clock_of_a_cleared_meet_point() -> TestResult {
    let mut net = Net::new(&asking(2), 65)?;
    for _ in 0..3 {
        net.act(BEN, MeetupAction::SetMeetPoint(MeetPointDraft::Clear))?;
    }
    net.add(CHLOE, &Setup::with(LocationVisibility::AskEachTime))?;
    assert_eq!(net.session(CHLOE)?.meet_point(), None);
    // Chloé's first write must beat the cleared state everywhere.
    net.act(CHLOE, MeetupAction::SetMeetPoint(fixed(1.0, 1.0)?))?;
    for leaf in [ALICE, BEN, CHLOE] {
        assert!(matches!(
            net.session(leaf)?.meet_point(),
            Some(MeetPoint::Fixed { lamport, .. }) if *lamport == Lamport::new(4)
        ));
    }
    Ok(())
}

#[test]
fn meet_at_me_reaches_a_late_joiner_from_its_owner_only() -> TestResult {
    let mut net = Net::new(&asking(3), 66)?;
    net.act(CHLOE, MeetupAction::SetMeetPoint(MeetPointDraft::AtMe))?;
    for leaf in [ALICE, BEN, CHLOE] {
        net.take_sent(leaf)?;
    }
    net.add(DAN, &Setup::with(LocationVisibility::AskEachTime))?;
    // Chloé re-sends her own `AtMember`; Alice's snapshot cannot vouch for it.
    assert_eq!(super::count(&net.take_sent(CHLOE)?, "meet point"), 1);
    assert!(matches!(
        net.session(DAN)?.meet_point(),
        Some(MeetPoint::AtMember { leaf, .. }) if *leaf == CHLOE
    ));
    Ok(())
}

#[test]
fn a_snapshot_claiming_someone_elses_position_is_ignored() -> TestResult {
    let mut net = Net::new(&asking(3), 67)?;
    net.deliver(
        ALICE,
        CHLOE,
        MeetupMessage::Snapshot {
            meet_point: Some(MeetPoint::AtMember {
                leaf: BEN,
                lamport: Lamport::new(2),
            }),
            lamport: Lamport::new(2),
        },
    )?;
    assert_eq!(net.session(CHLOE)?.meet_point(), None);
    Ok(())
}
