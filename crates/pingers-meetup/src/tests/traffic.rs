// SPDX-License-Identifier: AGPL-3.0-only
//! Uniform traffic (design B.6.1): frame sizes and timing do not depend on whether, or with
//! how many, a member shares; two fixed cadences (design F.3).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_proto::{ChatBody, Delivery, Urgency};

use super::{
    ALICE, BEN, CHLOE, DAN, GrantSuite, Net, Sent, Setup, Shape, TestResult, asking, count,
    location_frames, positions_from, shapes,
};
use crate::{
    CadenceMode, ExactLocation, GrantScope, KEEP_ALIVE_INTERVAL, LIVE_FRAME_INTERVAL,
    LOCATION_FRAME_INTERVAL, LocationVisibility, MeetPointDraft, MeetupAction, MeetupInput,
};

/// How many members Ben shares with.
#[derive(Clone, Copy, Debug)]
enum Grantees {
    None,
    One,
    All,
}

/// Ben's frames over ten minutes with the compass opened for 30 s in the middle, after
/// sharing with `grantees`.
fn bens_frames(grantees: Grantees) -> Result<Vec<Shape>, super::TestError> {
    let mut setups = asking(3);
    if let (Grantees::All, Some(ben)) = (grantees, setups.get_mut(1)) {
        ben.visibility = LocationVisibility::Everyone;
    }
    let mut net = Net::new(&setups, 40)?;
    net.fix(BEN, 51.5, -0.12)?;
    if let Grantees::One = grantees {
        net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    }
    net.take_sent(BEN)?;
    net.advance(300)?;
    net.run(BEN, MeetupInput::Cadence(CadenceMode::Live))?;
    net.advance(30)?;
    net.run(BEN, MeetupInput::Cadence(CadenceMode::Background))?;
    net.advance(270)?;
    shapes(&net.take_sent(BEN)?)
}

#[test]
fn frame_sizes_and_timing_are_the_same_with_zero_one_or_all_grantees() -> TestResult {
    let none = bens_frames(Grantees::None)?;
    let one = bens_frames(Grantees::One)?;
    let all = bens_frames(Grantees::All)?;
    assert_eq!(none, one);
    assert_eq!(none, all);
    // Only location frames: 5 background, 15 live (330 to 358 s, every 2 s), then 4 more.
    assert!(none.iter().all(|shape| shape.kind == "location"));
    assert_eq!(none.len(), 5 + 15 + 4);
    assert!(none.iter().all(|shape| shape.size == 256));
    assert!(
        none.iter()
            .all(|shape| shape.delivery == Delivery::Ephemeral && shape.wake.is_none())
    );
    Ok(())
}

fn gaps(frames: &[Shape]) -> Vec<u64> {
    frames
        .windows(2)
        .filter_map(|pair| match pair {
            [first, second] => Some(second.at.get().saturating_sub(first.at.get())),
            _ => None,
        })
        .collect()
}

#[test]
fn background_frames_every_minute_live_frames_every_two_seconds() -> TestResult {
    let frames = bens_frames(Grantees::None)?;
    let gaps = gaps(&frames);
    let background = LOCATION_FRAME_INTERVAL.as_secs();
    let live = LIVE_FRAME_INTERVAL.as_secs();
    // Four background gaps, live from 330 s, and back to 60 s after the last live frame.
    assert_eq!(gaps.get(..4), Some([background; 4].as_slice()));
    assert!(
        gaps.get(4..18)
            .is_some_and(|live_gaps| live_gaps.iter().all(|gap| *gap == live))
    );
    assert!(
        gaps.get(19..)
            .is_some_and(|rest| rest.iter().all(|gap| *gap == background))
    );
    // The location cadence alone keeps every member well inside the keep-alive interval.
    assert!(gaps.iter().all(|gap| *gap < KEEP_ALIVE_INTERVAL.as_secs()));
    Ok(())
}

fn grant_frame_shapes(sent: &[Sent]) -> Result<Vec<Shape>, super::TestError> {
    Ok(shapes(sent)?
        .into_iter()
        .filter(|shape| shape.kind == "grant")
        .collect())
}

#[test]
fn starting_and_joining_send_the_same_grant_frames_under_every_setting() -> TestResult {
    let mut at_start = Vec::new();
    let mut on_join = Vec::new();
    for visibility in [
        LocationVisibility::AskEachTime,
        LocationVisibility::Everyone,
        LocationVisibility::Nobody,
    ] {
        let mut setups = asking(3);
        if let Some(ben) = setups.get_mut(1) {
            ben.visibility = visibility;
        }
        let mut net = Net::new(&setups, 41)?;
        at_start.push(shapes(&net.take_sent(BEN)?)?);
        net.add(DAN, &Setup::with(LocationVisibility::AskEachTime))?;
        on_join.push(grant_frame_shapes(&net.take_sent(BEN)?)?);
    }
    // One grant-sized frame per other member at start, one per newcomer on joining.
    assert!(at_start.iter().all(|sent| count_kind(sent, "grant") == 2));
    assert!(on_join.iter().all(|sent| sent.len() == 1));
    assert!(at_start.windows(2).all(|pair| pair.first() == pair.last()));
    assert!(on_join.windows(2).all(|pair| pair.first() == pair.last()));
    Ok(())
}

fn count_kind(shapes: &[Shape], kind: &str) -> usize {
    shapes.iter().filter(|shape| shape.kind == kind).count()
}

#[test]
fn a_rotation_sends_one_grant_sized_frame_per_other_member() -> TestResult {
    let mut net = Net::new(&asking(4), 42)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    net.take_sent(BEN)?;
    // Revoking a member who never held the key...
    net.act(BEN, MeetupAction::RevokeLocation(ALICE))?;
    let not_a_holder = grant_frame_shapes(&net.take_sent(BEN)?)?;
    // ...looks the same as revoking one who did.
    net.act(BEN, MeetupAction::RevokeLocation(CHLOE))?;
    let holder = grant_frame_shapes(&net.take_sent(BEN)?)?;
    assert_eq!(not_a_holder.len(), 3);
    assert_eq!(not_a_holder, holder);
    Ok(())
}

#[test]
fn changing_the_setting_sends_one_grant_sized_frame_per_other_member() -> TestResult {
    for visibility in [LocationVisibility::Everyone, LocationVisibility::Nobody] {
        let mut net = Net::new(&asking(4), 43)?;
        net.take_sent(BEN)?;
        net.act(BEN, MeetupAction::SetVisibility(visibility))?;
        assert_eq!(count(&net.take_sent(BEN)?, "grant"), 3);
    }
    Ok(())
}

#[test]
fn a_fix_never_sends_a_frame_by_itself() -> TestResult {
    let mut net = Net::new(&asking(2), 44)?;
    let outputs = net.run(
        BEN,
        MeetupInput::OwnFix(pingers_geo::Fix {
            coordinate: pingers_geo::Coordinate::new(1.0, 1.0)?,
            accuracy: pingers_geo::Meters::from_whole(3),
            taken_at: net.now,
        }),
    )?;
    assert!(outputs.is_empty());
    Ok(())
}

#[test]
fn a_holder_gets_dummies_until_the_sender_has_a_fix() -> TestResult {
    let mut net = Net::new(&asking(3), 45)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    net.advance(120)?;
    assert_eq!(positions_from(&net.take_events(CHLOE)?, BEN), 0);
    assert!(matches!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { .. }
    ));
    net.fix(BEN, 10.0, 10.0)?;
    net.advance(60)?;
    assert_eq!(positions_from(&net.take_events(CHLOE)?, BEN), 1);
    Ok(())
}

#[test]
fn location_frames_keep_the_generation_until_a_rotation() -> TestResult {
    let mut net = Net::new(&asking(3), 46)?;
    net.advance(180)?;
    let sent = net.take_sent(BEN)?;
    let frames = location_frames(&sent);
    assert!(frames.len() >= 3);
    let generation = net.session(BEN)?.generation();
    assert!(frames.iter().all(|frame| frame.generation == generation));
    Ok(())
}

#[test]
fn chat_and_meet_point_are_stored_and_wake() -> TestResult {
    let mut net = Net::new(&asking(2), 47)?;
    net.take_sent(BEN)?;
    net.act(
        BEN,
        MeetupAction::SendChat {
            body: ChatBody::new("here".to_owned())?,
            parent: None,
        },
    )?;
    net.act(BEN, MeetupAction::SetMeetPoint(MeetPointDraft::Clear))?;
    let sent = shapes(&net.take_sent(BEN)?)?;
    let chat = sent.iter().find(|shape| shape.kind == "chat");
    let meet_point = sent.iter().find(|shape| shape.kind == "meet point");
    assert_eq!(
        chat.map(|shape| (shape.delivery, shape.wake)),
        Some((Delivery::Stored, Some(Urgency::Alert)))
    );
    assert_eq!(
        meet_point.map(|shape| (shape.delivery, shape.wake)),
        Some((Delivery::Stored, Some(Urgency::Quiet)))
    );
    Ok(())
}

#[test]
fn asks_and_grants_are_stored_without_a_wake_at_location_frame_size() -> TestResult {
    let mut net = Net::new(&asking(2), 48)?;
    net.take_sent(ALICE)?;
    net.take_sent(BEN)?;
    net.grant(ALICE, BEN, GrantScope::ThisMeetup)?;
    let ask = shapes(&net.take_sent(ALICE)?)?;
    let grant = shapes(&net.take_sent(BEN)?)?;
    assert_eq!(
        ask.iter().map(|shape| shape.kind).collect::<Vec<_>>(),
        ["ask"]
    );
    assert_eq!(
        grant.iter().map(|shape| shape.kind).collect::<Vec<_>>(),
        ["grant"]
    );
    assert!(ask.iter().chain(&grant).all(|shape| shape.size == 256
        && shape.delivery == Delivery::Stored
        && shape.wake.is_none()));
    Ok(())
}

/// Ben's stored frames (what the group log keeps) as a four-member meetup goes through a
/// revoke, a member leaving, a member joining and a setting change, after sharing with
/// `grantees`.
fn bens_stored_frames(grantees: Grantees) -> Result<Vec<Shape>, super::TestError> {
    let mut setups = asking(4);
    if let (Grantees::All, Some(ben)) = (grantees, setups.get_mut(1)) {
        ben.visibility = LocationVisibility::Everyone;
    }
    let mut net = Net::new(&setups, 49)?;
    let mut stored = shapes(&net.take_sent(BEN)?)?;
    if let Grantees::One = grantees {
        net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    }
    net.take_sent(BEN)?;
    net.advance(10)?;
    net.act(BEN, MeetupAction::RevokeLocation(ALICE))?;
    net.advance(10)?;
    let _left = net.remove(DAN)?;
    net.advance(10)?;
    net.add(DAN, &Setup::with(LocationVisibility::AskEachTime))?;
    net.advance(10)?;
    net.act(BEN, MeetupAction::SetVisibility(LocationVisibility::Nobody))?;
    stored.extend(shapes(&net.take_sent(BEN)?)?);
    stored.retain(|shape| shape.delivery == Delivery::Stored);
    Ok(stored)
}

#[test]
fn stored_grant_traffic_is_the_same_with_zero_one_or_all_grantees() -> TestResult {
    let none = bens_stored_frames(Grantees::None)?;
    let one = bens_stored_frames(Grantees::One)?;
    let all = bens_stored_frames(Grantees::All)?;
    assert_eq!(none, one);
    assert_eq!(none, all);
    // Start 3, revoke 3, leave 2, join 1, setting change 3: one per other member each time
    // (one per newcomer on joining), all grant-sized, none waking anyone.
    assert_eq!(none.len(), 3 + 3 + 2 + 1 + 3);
    assert!(
        none.iter()
            .all(|shape| shape.kind == "grant" && shape.size == 256 && shape.wake.is_none())
    );
    Ok(())
}

/// Ben's grant frames on starting and on a newcomer joining, with `visibility`, in a
/// quantum-resistant Meetup (X-Wing grant keys).
fn quantum_grant_frames(
    visibility: LocationVisibility,
) -> Result<(Vec<Shape>, Vec<Shape>), super::TestError> {
    let mut setups = asking(3);
    if let Some(ben) = setups.get_mut(1) {
        ben.visibility = visibility;
    }
    let mut net = Net::with_suite(&setups, 44, GrantSuite::XWing)?;
    let at_start = grant_frame_shapes(&net.take_sent(BEN)?)?;
    net.add_with_suite(
        DAN,
        &Setup::with(LocationVisibility::AskEachTime),
        GrantSuite::XWing,
    )?;
    Ok((at_start, grant_frame_shapes(&net.take_sent(BEN)?)?))
}

/// In a quantum-resistant Meetup a real grant is sealed to an X-Wing key, so a dummy must be
/// too: sealed to an X25519 throwaway key it would fall in a smaller size class, and show who
/// was not granted.
#[test]
fn a_quantum_resistant_meetup_sends_the_same_grant_frames_sharing_or_not() -> TestResult {
    let (shared_start, shared_join) = quantum_grant_frames(LocationVisibility::Everyone)?;
    for visibility in [LocationVisibility::AskEachTime, LocationVisibility::Nobody] {
        let (start, join) = quantum_grant_frames(visibility)?;
        assert_eq!(start, shared_start);
        assert_eq!(join, shared_join);
    }
    assert_eq!(shared_start.len(), 2);
    assert_eq!(shared_join.len(), 1);
    // X-Wing grants do not fit 1 KiB: every grant, real or dummy, is a 4 KiB frame.
    assert!(
        shared_start
            .iter()
            .chain(&shared_join)
            .all(|shape| shape.size == 4096)
    );

    // A rotation: revoking a member who held the key looks like revoking one who did not.
    let mut net = Net::with_suite(&asking(4), 45, GrantSuite::XWing)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    net.take_sent(BEN)?;
    net.act(BEN, MeetupAction::RevokeLocation(ALICE))?;
    let not_a_holder = grant_frame_shapes(&net.take_sent(BEN)?)?;
    net.act(BEN, MeetupAction::RevokeLocation(CHLOE))?;
    let holder = grant_frame_shapes(&net.take_sent(BEN)?)?;
    assert_eq!(not_a_holder.len(), 3);
    assert!(
        not_a_holder
            .iter()
            .chain(&holder)
            .all(|shape| shape.size == 4096)
    );
    assert_eq!(
        not_a_holder
            .iter()
            .map(|shape| shape.size)
            .collect::<Vec<_>>(),
        holder.iter().map(|shape| shape.size).collect::<Vec<_>>()
    );
    Ok(())
}

/// Asks in a quantum-resistant Meetup: an ask to any member has the size of a grant, so an ask
/// and its "Allow" cannot be told from each other or from a dummy.
#[test]
fn asks_in_a_quantum_resistant_meetup_are_indistinguishable() -> TestResult {
    let mut net = Net::with_suite(&asking(3), 46, GrantSuite::XWing)?;
    net.take_sent(CHLOE)?;
    net.act(CHLOE, MeetupAction::AskLocation(BEN))?;
    let to_ben = shapes(&net.take_sent(CHLOE)?)?;
    net.act(CHLOE, MeetupAction::AskLocation(ALICE))?;
    let to_alice = shapes(&net.take_sent(CHLOE)?)?;
    let asks: Vec<&Shape> = to_ben
        .iter()
        .chain(&to_alice)
        .filter(|shape| shape.kind == "ask")
        .collect();
    assert_eq!(asks.len(), 2);
    assert!(asks.iter().all(|shape| shape.size == 4096
        && shape.delivery == Delivery::Stored
        && shape.wake.is_none()));
    Ok(())
}
