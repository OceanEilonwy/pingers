// SPDX-License-Identifier: AGPL-3.0-only
//! Liveness, ending, chat and board cross-checks (design B.4.3, B.5, B.6).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_crypto::GrantKeyPair;
use pingers_proto::{
    ChatBody, ChatMessageId, Epoch, GroupId, MeetupMessage, SeenSegment, SegmentHash,
};
use pingers_testkit::seeded_rng;
use pingers_time::UnixSeconds;

use super::{ALICE, BEN, CHLOE, DAN, Net, TestResult, asking, at, signature};
use crate::{
    ALONE_LIMIT, AlwaysAllow, EndReason, LocationVisibility, MeetupAction, MeetupConfig,
    MeetupError, MeetupEvent, MeetupInput, MeetupSession, Member, SILENCE_LIMIT,
};

fn chat(text: &str) -> Result<ChatBody, super::TestError> {
    Ok(ChatBody::new(text.to_owned())?)
}

/// Stops a device without removing it: it neither ticks nor receives.
fn silence(net: &mut Net, leaf: pingers_proto::LeafIndex) -> TestResult {
    net.device_mut(leaf)?.ended = Some(EndReason::Removed);
    Ok(())
}

#[test]
fn a_member_silent_for_ten_minutes_is_removed_and_asked_again_later() -> TestResult {
    let mut net = Net::new(&asking(3), 80)?;
    silence(&mut net, CHLOE)?;
    let limit = SILENCE_LIMIT.as_secs();
    net.advance(limit - 1)?;
    assert!(net.device(ALICE)?.removals.is_empty());
    net.advance(1)?;
    assert_eq!(net.device(ALICE)?.removals, vec![CHLOE]);
    assert_eq!(net.device(BEN)?.removals, vec![CHLOE]);
    // The removal did not happen: asked again one limit later, not on every tick.
    net.advance(limit - 1)?;
    assert_eq!(net.device(ALICE)?.removals.len(), 1);
    net.advance(1)?;
    assert_eq!(net.device(ALICE)?.removals.len(), 2);
    Ok(())
}

#[test]
fn any_frame_counts_as_a_sign_of_life() -> TestResult {
    let mut net = Net::new(&asking(2), 81)?;
    silence(&mut net, BEN)?;
    net.advance(SILENCE_LIMIT.as_secs() - 1)?;
    net.deliver(BEN, ALICE, MeetupMessage::KeepAlive)?;
    net.advance(SILENCE_LIMIT.as_secs() - 1)?;
    assert!(net.device(ALICE)?.removals.is_empty());
    net.advance(1)?;
    assert_eq!(net.device(ALICE)?.removals, vec![BEN]);
    Ok(())
}

#[test]
fn members_sending_on_the_cadence_are_never_removed() -> TestResult {
    let mut net = Net::new(&asking(3), 82)?;
    net.advance(3 * SILENCE_LIMIT.as_secs())?;
    assert!(
        net.devices
            .values()
            .all(|device| device.removals.is_empty())
    );
    Ok(())
}

#[test]
fn a_lone_member_ends_the_meetup_after_thirty_minutes() -> TestResult {
    let mut net = Net::new(&asking(2), 83)?;
    net.advance(100)?;
    net.remove(BEN)?;
    net.advance(ALONE_LIMIT.as_secs() - 1)?;
    assert_eq!(net.device(ALICE)?.ended, None);
    net.advance(1)?;
    assert_eq!(net.device(ALICE)?.ended, Some(EndReason::AloneTooLong));
    let session = &mut net
        .devices
        .get_mut(&ALICE)
        .ok_or(super::TestError::Setup("alice"))?
        .session;
    assert_eq!(session.next_deadline(), None);
    assert!(matches!(
        session.handle(MeetupInput::Tick, at(1), &mut seeded_rng(1)),
        Err(MeetupError::Ended)
    ));
    Ok(())
}

#[test]
fn company_arriving_resets_the_alone_timer() -> TestResult {
    let mut net = Net::new(&asking(2), 84)?;
    net.remove(BEN)?;
    net.advance(ALONE_LIMIT.as_secs() - 10)?;
    net.add(CHLOE, &super::Setup::with(LocationVisibility::AskEachTime))?;
    net.advance(20)?;
    net.remove(CHLOE)?;
    net.advance(ALONE_LIMIT.as_secs() - 1)?;
    assert_eq!(net.device(ALICE)?.ended, None);
    net.advance(1)?;
    assert_eq!(net.device(ALICE)?.ended, Some(EndReason::AloneTooLong));
    Ok(())
}

#[test]
fn a_session_started_alone_ends_thirty_minutes_after_joining() -> TestResult {
    let mut net = Net::new(&asking(1), 85)?;
    net.advance(ALONE_LIMIT.as_secs())?;
    assert_eq!(net.device(ALICE)?.ended, Some(EndReason::AloneTooLong));
    Ok(())
}

#[test]
fn being_removed_from_the_group_ends_the_session() -> TestResult {
    let mut net = Net::new(&asking(3), 86)?;
    let members: Vec<Member> = net
        .members()
        .into_iter()
        .filter(|member| member.leaf != CHLOE)
        .collect();
    let outputs = net.run(
        CHLOE,
        MeetupInput::MembersChanged {
            epoch: Epoch::new(2),
            members,
        },
    )?;
    assert_eq!(outputs, vec![crate::MeetupOutput::End(EndReason::Removed)]);
    Ok(())
}

#[test]
fn starting_without_being_a_member_fails() -> TestResult {
    let mut rng = seeded_rng(87);
    let pair = GrantKeyPair::generate(&mut rng)?;
    let other = GrantKeyPair::generate(&mut rng)?;
    let result = MeetupSession::start(
        MeetupConfig {
            group: GroupId::from_bytes([1; 32]),
            own_leaf: BEN,
            epoch: Epoch::new(1),
            own_grant_key: pair.secret,
            members: vec![Member {
                leaf: ALICE,
                signature_key: signature(ALICE)?,
                grant_key: other.public,
            }],
            visibility: LocationVisibility::AskEachTime,
            always_allow: AlwaysAllow::default(),
            joined_at: UnixSeconds::new(0),
        },
        &mut rng,
    );
    assert!(matches!(result, Err(MeetupError::NotAMember)));
    Ok(())
}

#[test]
fn frames_from_non_members_are_ignored() -> TestResult {
    let mut net = Net::new(&asking(2), 88)?;
    let outputs = net.deliver(
        DAN,
        ALICE,
        MeetupMessage::Chat {
            id: ChatMessageId::from_bytes([1; 16]),
            parent: None,
            body: chat("hello")?,
            written_at: net.now,
        },
    )?;
    assert!(outputs.is_empty());
    Ok(())
}

#[test]
fn chat_reaches_everyone_and_replies_stay_one_level_deep() -> TestResult {
    let mut net = Net::new(&asking(3), 89)?;
    net.act(
        ALICE,
        MeetupAction::SendChat {
            body: chat("at the gate")?,
            parent: None,
        },
    )?;
    let root = net
        .take_events(BEN)?
        .into_iter()
        .find_map(|event| match event {
            MeetupEvent::Chat {
                id,
                sender,
                parent: None,
                ..
            } if sender == ALICE => Some(id),
            _ => None,
        })
        .ok_or(super::TestError::Setup("no chat"))?;
    // The sender hears its own message too, for one code path to the store.
    assert!(
        net.take_events(ALICE)?
            .iter()
            .any(|event| matches!(event, MeetupEvent::Chat { id, .. } if *id == root))
    );
    net.act(
        BEN,
        MeetupAction::SendChat {
            body: chat("coming")?,
            parent: Some(root),
        },
    )?;
    let reply = net
        .take_events(CHLOE)?
        .into_iter()
        .find_map(|event| match event {
            MeetupEvent::Chat {
                id, sender, parent, ..
            } if sender == BEN => Some((id, parent)),
            _ => None,
        })
        .ok_or(super::TestError::Setup("no reply"))?;
    assert_eq!(reply.1, Some(root));
    // A reply to the reply is filed under the root.
    net.act(
        CHLOE,
        MeetupAction::SendChat {
            body: chat("me too")?,
            parent: Some(reply.0),
        },
    )?;
    assert!(net.take_events(ALICE)?.iter().any(|event| matches!(
        event,
        MeetupEvent::Chat { sender, parent, .. } if *sender == CHLOE && *parent == Some(root)
    )));
    Ok(())
}

#[test]
fn a_duplicate_chat_message_is_shown_once() -> TestResult {
    let mut net = Net::new(&asking(2), 90)?;
    let message = MeetupMessage::Chat {
        id: ChatMessageId::from_bytes([4; 16]),
        parent: None,
        body: chat("hi")?,
        written_at: net.now,
    };
    net.deliver(BEN, ALICE, message.clone())?;
    net.deliver(BEN, ALICE, message)?;
    assert_eq!(net.take_events(ALICE)?.len(), 1);
    Ok(())
}

fn seen(hash: u8) -> Vec<SeenSegment> {
    vec![SeenSegment {
        start: UnixSeconds::new(3_600).bucket(),
        hash: SegmentHash::from_bytes([hash; 32]),
    }]
}

#[test]
fn different_board_hashes_raise_equivocation_on_both_sides() -> TestResult {
    let mut net = Net::new(&asking(2), 91)?;
    net.run(ALICE, MeetupInput::OwnBoardSeen(seen(1)))?;
    assert!(net.take_events(ALICE)?.is_empty());
    net.run(BEN, MeetupInput::OwnBoardSeen(seen(2)))?;
    assert_eq!(
        net.take_events(ALICE)?,
        vec![MeetupEvent::BoardEquivocation]
    );
    assert_eq!(net.take_events(BEN)?, vec![MeetupEvent::BoardEquivocation]);
    Ok(())
}

#[test]
fn matching_board_hashes_raise_nothing() -> TestResult {
    let mut net = Net::new(&asking(2), 92)?;
    net.run(ALICE, MeetupInput::OwnBoardSeen(seen(1)))?;
    net.run(BEN, MeetupInput::OwnBoardSeen(seen(1)))?;
    assert!(net.take_events(ALICE)?.is_empty());
    assert!(net.take_events(BEN)?.is_empty());
    Ok(())
}

#[test]
fn the_deadline_is_the_earliest_timer() -> TestResult {
    let mut net = Net::new(&asking(2), 93)?;
    // Right after the first frame: the next frame, 60 s later.
    assert_eq!(net.session(ALICE)?.next_deadline(), Some(at(60)));
    // A 15-minute grant does not move it earlier than the next frame...
    net.grant(BEN, ALICE, crate::GrantScope::FifteenMinutes)?;
    assert_eq!(net.session(ALICE)?.next_deadline(), Some(at(60)));
    // ...but the silence limit of a member who stops sending does, once it is the earliest.
    silence(&mut net, BEN)?;
    net.advance(SILENCE_LIMIT.as_secs() - 30)?;
    assert_eq!(
        net.session(ALICE)?.next_deadline(),
        Some(at(SILENCE_LIMIT.as_secs()))
    );
    Ok(())
}
