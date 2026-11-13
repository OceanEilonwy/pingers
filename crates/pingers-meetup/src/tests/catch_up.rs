// SPDX-License-Identifier: AGPL-3.0-only
//! Grants and asks are stored in the group log (design B.6.1): a member whose live channel
//! was down gets them when it reads the log, a copy that arrives twice changes nothing, and
//! a late one is judged by its own generation or timestamp.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_proto::{LeafIndex, MeetupMessage};

use super::{ALICE, BEN, CHLOE, Net, Sent, TestError, TestResult, allow, asking, count};
use crate::{ASK_EXPIRY, ExactLocation, GrantScope, MeetupAction, MeetupError, MeetupEvent};

/// The last frame of `kind` among `sent`.
fn last_of(sent: &[Sent], wanted: &str) -> Result<MeetupMessage, TestError> {
    sent.iter()
        .rev()
        .find(|sent| super::kind(&sent.frame.message) == wanted)
        .map(|sent| sent.frame.message.clone())
        .ok_or(TestError::Setup("no such frame"))
}

fn positions(net: &mut Net, viewer: LeafIndex, peer: LeafIndex) -> Result<usize, TestError> {
    Ok(super::positions_from(&net.take_events(viewer)?, peer))
}

fn requests(events: &[MeetupEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, MeetupEvent::LocationRequested { .. }))
        .count()
}

#[test]
fn a_grant_sent_while_the_requester_is_offline_arrives_from_the_log() -> TestResult {
    let mut net = Net::new(&asking(3), 60)?;
    net.fix(BEN, 51.5, -0.12)?;
    let request = net.ask(CHLOE, BEN)?;
    net.go_offline(CHLOE);
    net.act(
        BEN,
        MeetupAction::AnswerRequest {
            request,
            answer: allow(GrantScope::ThisMeetup),
        },
    )?;
    net.advance(60)?;
    assert_eq!(net.access(CHLOE, BEN)?, ExactLocation::Private);

    net.reconnect(CHLOE)?;
    assert_eq!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    assert_eq!(net.session(CHLOE)?.pending_request(BEN, net.now), None);
    net.take_events(CHLOE)?;
    net.advance(60)?;
    assert_eq!(positions(&mut net, CHLOE, BEN)?, 1);
    Ok(())
}

#[test]
fn an_ask_sent_while_the_target_is_offline_prompts_from_the_log() -> TestResult {
    let mut net = Net::new(&asking(3), 61)?;
    net.go_offline(BEN);
    net.act(CHLOE, MeetupAction::AskLocation(BEN))?;
    net.advance(ASK_EXPIRY.as_secs() - 1)?;
    assert_eq!(requests(&net.take_events(BEN)?), 0);
    net.reconnect(BEN)?;
    assert_eq!(requests(&net.device(BEN)?.events), 1);
    // Ten minutes count from when it was asked, not from when it was read.
    let request = net.last_request(BEN)?;
    net.advance(1)?;
    assert!(matches!(
        net.act(
            BEN,
            MeetupAction::AnswerRequest {
                request,
                answer: allow(GrantScope::ThisMeetup),
            },
        ),
        Err(TestError::Meetup(MeetupError::UnknownRequest))
    ));
    Ok(())
}

#[test]
fn an_ask_read_from_the_log_after_ten_minutes_has_expired() -> TestResult {
    let mut net = Net::new(&asking(3), 62)?;
    net.go_offline(BEN);
    net.act(CHLOE, MeetupAction::AskLocation(BEN))?;
    net.advance(ASK_EXPIRY.as_secs())?;
    net.take_sent(BEN)?;
    net.reconnect(BEN)?;
    assert_eq!(requests(&net.take_events(BEN)?), 0);
    assert_eq!(count(&net.take_sent(BEN)?, "grant"), 0);
    Ok(())
}

#[test]
fn revoking_while_members_are_offline_keeps_the_new_key_from_the_revoked() -> TestResult {
    let mut net = Net::new(&asking(4), 63)?;
    net.fix(BEN, 51.5, -0.12)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    net.grant(ALICE, BEN, GrantScope::ThisMeetup)?;
    net.go_offline(CHLOE);
    net.go_offline(ALICE);
    net.act(BEN, MeetupAction::RevokeLocation(CHLOE))?;
    net.advance(60)?;

    net.reconnect(CHLOE)?;
    net.reconnect(ALICE)?;
    net.take_events(CHLOE)?;
    net.take_events(ALICE)?;
    net.advance(60)?;
    // Alice, who keeps access, got the new key from the log and reads the new frames.
    assert_eq!(positions(&mut net, ALICE, BEN)?, 1);
    assert!(matches!(
        net.access(ALICE, BEN)?,
        ExactLocation::Shared { .. }
    ));
    // Chloé did not: the first new frame shows her she was rotated away.
    assert_eq!(positions(&mut net, CHLOE, BEN)?, 0);
    assert_eq!(net.access(CHLOE, BEN)?, ExactLocation::Private);
    assert!(!net.session(BEN)?.shares_with(CHLOE));
    Ok(())
}

#[test]
fn a_grant_delivered_twice_changes_nothing_the_second_time() -> TestResult {
    let mut net = Net::new(&asking(3), 64)?;
    net.fix(BEN, 51.5, -0.12)?;
    net.take_sent(BEN)?;
    net.grant(CHLOE, BEN, GrantScope::FifteenMinutes)?;
    let grant = last_of(&net.take_sent(BEN)?, "grant")?;
    net.advance(60)?;
    assert_eq!(positions(&mut net, CHLOE, BEN)?, 1);
    let access = net.access(CHLOE, BEN)?;

    net.deliver(BEN, CHLOE, grant)?;
    assert!(net.take_events(CHLOE)?.is_empty());
    assert_eq!(net.access(CHLOE, BEN)?, access);
    net.advance(60)?;
    assert_eq!(positions(&mut net, CHLOE, BEN)?, 1);
    Ok(())
}

#[test]
fn a_shorter_copy_of_a_grant_arriving_last_keeps_the_longer_end() -> TestResult {
    let mut net = Net::new(&asking(3), 65)?;
    let first = net.ask(CHLOE, BEN)?;
    let second = net.ask(CHLOE, BEN)?;
    net.take_sent(BEN)?;
    net.act(
        BEN,
        MeetupAction::AnswerRequest {
            request: first,
            answer: allow(GrantScope::FifteenMinutes),
        },
    )?;
    let short = last_of(&net.take_sent(BEN)?, "grant")?;
    net.act(
        BEN,
        MeetupAction::AnswerRequest {
            request: second,
            answer: allow(GrantScope::ThisMeetup),
        },
    )?;
    net.take_events(CHLOE)?;
    // The fifteen-minute copy is read again from the log after the longer one.
    net.deliver(BEN, CHLOE, short)?;
    assert!(net.take_events(CHLOE)?.is_empty());
    assert_eq!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    Ok(())
}

#[test]
fn an_ask_delivered_again_after_its_answer_prompts_and_sends_nothing() -> TestResult {
    let mut net = Net::new(&asking(3), 66)?;
    net.take_sent(CHLOE)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    let ask = last_of(&net.take_sent(CHLOE)?, "ask")?;
    net.take_events(BEN)?;
    net.take_sent(BEN)?;
    // Chloé holds the key now, so a fresh ask would get her grant re-sent; a copy does not.
    net.deliver(CHLOE, BEN, ask)?;
    assert!(net.take_events(BEN)?.is_empty());
    assert!(net.take_sent(BEN)?.is_empty());
    Ok(())
}

#[test]
fn an_ask_declined_with_not_now_does_not_prompt_again_from_the_log() -> TestResult {
    let mut net = Net::new(&asking(3), 67)?;
    net.take_sent(CHLOE)?;
    let request = net.ask(CHLOE, BEN)?;
    let ask = last_of(&net.take_sent(CHLOE)?, "ask")?;
    net.act(
        BEN,
        MeetupAction::AnswerRequest {
            request,
            answer: crate::LocationAnswer::NotNow,
        },
    )?;
    net.take_events(BEN)?;
    net.advance(300)?;
    net.deliver(CHLOE, BEN, ask)?;
    assert_eq!(requests(&net.take_events(BEN)?), 0);
    Ok(())
}

#[test]
fn an_old_grant_read_after_being_rotated_away_gives_no_access() -> TestResult {
    let mut net = Net::new(&asking(4), 68)?;
    net.fix(BEN, 51.5, -0.12)?;
    net.take_sent(BEN)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    let old = last_of(&net.take_sent(BEN)?, "grant")?;
    net.act(BEN, MeetupAction::RevokeLocation(CHLOE))?;
    net.advance(60)?;
    assert_eq!(net.access(CHLOE, BEN)?, ExactLocation::Private);

    net.take_events(CHLOE)?;

    // A reconnect replays the old grant from the log after a newer frame was seen.
    net.deliver(BEN, CHLOE, old)?;
    assert_eq!(net.access(CHLOE, BEN)?, ExactLocation::Private);
    assert!(net.take_events(CHLOE)?.iter().all(|event| !matches!(
        event,
        MeetupEvent::LocationAccessChanged {
            access: ExactLocation::Shared { .. },
            ..
        }
    )));
    Ok(())
}
