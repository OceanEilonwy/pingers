// SPDX-License-Identifier: AGPL-3.0-only
//! Uniform traffic (design B.6.1): frame sizes and timing do not depend on whether, or with
//! how many, a member shares; two fixed cadences (design F.3).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_proto::{ChatBody, Delivery, Urgency};

use super::{
    ALICE, BEN, CHLOE, DAN, Net, Sent, Setup, Shape, TestResult, asking, count, location_frames,
    positions_from, shapes,
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
fn asks_are_ephemeral_and_the_same_size_as_location_frames() -> TestResult {
    let mut net = Net::new(&asking(2), 48)?;
    net.take_sent(ALICE)?;
    net.act(ALICE, MeetupAction::AskLocation(BEN))?;
    net.advance(60)?;
    let sent = shapes(&net.take_sent(ALICE)?)?;
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|shape| shape.size == 256
        && shape.delivery == Delivery::Ephemeral
        && shape.wake.is_none()));
    Ok(())
}
