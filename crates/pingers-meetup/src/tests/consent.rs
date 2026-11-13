// SPDX-License-Identifier: AGPL-3.0-only
//! Exact-location consent flows (design B.6.1): grant, revoke, expiry, leave, settings.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_crypto::{LocationAsk, seal_location_ask};
use pingers_proto::{LocationRequestId, MeetupMessage};

use super::{
    ALICE, BEN, CHLOE, DAN, Net, Setup, TestResult, allow, asking, at, count, location_frames,
    positions_from,
};
use crate::{
    ASK_EXPIRY, AllowListChoice, AskBack, ExactLocation, GrantScope, LocationAnswer,
    LocationVisibility, MeetPointDraft, MeetupAction, MeetupError, MeetupEvent, SHORT_GRANT,
};

const BEN_LATITUDE: f64 = 51.5;
const BEN_LONGITUDE: f64 = -0.12;

/// Ben has a fix; Chloé asks and Ben allows for this meetup.
fn chloe_sees_ben(seed: u64) -> Result<Net, super::TestError> {
    let mut net = Net::new(&asking(3), seed)?;
    net.fix(BEN, BEN_LATITUDE, BEN_LONGITUDE)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    Ok(net)
}

#[test]
fn an_ask_reaches_only_its_target() -> TestResult {
    let mut net = Net::new(&asking(3), 1)?;
    net.act(CHLOE, MeetupAction::AskLocation(BEN))?;
    let requested = |events: &[MeetupEvent]| {
        events.iter().any(
            |event| matches!(event, MeetupEvent::LocationRequested { from, .. } if *from == CHLOE),
        )
    };
    assert!(requested(&net.device(BEN)?.events));
    assert!(!requested(&net.device(ALICE)?.events));
    assert!(net.session(CHLOE)?.pending_request(BEN, net.now).is_some());
    Ok(())
}

#[test]
fn an_allowed_requester_sees_positions_and_nobody_else_does() -> TestResult {
    let mut net = chloe_sees_ben(2)?;
    assert_eq!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    assert_eq!(net.access(ALICE, BEN)?, ExactLocation::Private);
    assert!(net.session(BEN)?.shares_with(CHLOE));
    assert!(!net.session(BEN)?.shares_with(ALICE));
    // The grant answers Chloé's request.
    assert_eq!(net.session(CHLOE)?.pending_request(BEN, net.now), None);
    net.advance(60)?;
    let chloe = net.take_events(CHLOE)?;
    let alice = net.take_events(ALICE)?;
    assert_eq!(positions_from(&chloe, BEN), 1);
    assert_eq!(positions_from(&alice, BEN), 0);
    let position = chloe.iter().find_map(|event| match event {
        MeetupEvent::PeerLocation(location) => Some(location.position),
        _ => None,
    });
    let position = position.ok_or(super::TestError::Setup("no position"))?;
    assert!((position.latitude() - BEN_LATITUDE).abs() < 1e-6);
    assert!((position.longitude() - BEN_LONGITUDE).abs() < 1e-6);
    Ok(())
}

#[test]
fn not_now_sends_nothing_and_keeps_nothing() -> TestResult {
    let mut net = Net::new(&asking(3), 3)?;
    let request = net.ask(CHLOE, BEN)?;
    let outputs = net.act(
        BEN,
        MeetupAction::AnswerRequest {
            request,
            answer: LocationAnswer::NotNow,
        },
    )?;
    assert!(outputs.is_empty());
    assert!(matches!(
        net.act(
            BEN,
            MeetupAction::AnswerRequest {
                request,
                answer: allow(GrantScope::ThisMeetup),
            },
        ),
        Err(super::TestError::Meetup(MeetupError::UnknownRequest))
    ));
    assert_eq!(net.access(CHLOE, BEN)?, ExactLocation::Private);
    Ok(())
}

#[test]
fn not_now_looks_the_same_as_never_answering() -> TestResult {
    let mut declined = Net::new(&asking(3), 4)?;
    let mut ignored = Net::new(&asking(3), 4)?;
    for net in [&mut declined, &mut ignored] {
        let _request = net.ask(CHLOE, BEN)?;
        net.take_sent(BEN)?;
        net.take_events(CHLOE)?;
    }
    let request = declined.last_request(BEN)?;
    declined.act(
        BEN,
        MeetupAction::AnswerRequest {
            request,
            answer: LocationAnswer::NotNow,
        },
    )?;
    declined.advance(900)?;
    ignored.advance(900)?;
    assert_eq!(
        super::shapes(&declined.take_sent(BEN)?)?,
        super::shapes(&ignored.take_sent(BEN)?)?
    );
    assert_eq!(declined.take_events(CHLOE)?, ignored.take_events(CHLOE)?);
    assert_eq!(
        declined.session(BEN)?.next_deadline(),
        ignored.session(BEN)?.next_deadline()
    );
    Ok(())
}

#[test]
fn a_request_can_be_answered_until_it_expires_after_ten_minutes() -> TestResult {
    let mut net = Net::new(&asking(3), 5)?;
    let request = net.ask(CHLOE, BEN)?;
    let expiry = ASK_EXPIRY.as_secs();
    net.advance(expiry - 1)?;
    assert!(net.session(CHLOE)?.pending_request(BEN, net.now).is_some());
    net.act(
        BEN,
        MeetupAction::AnswerRequest {
            request,
            answer: allow(GrantScope::ThisMeetup),
        },
    )?;
    assert!(matches!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { .. }
    ));

    let late = net.ask(ALICE, BEN)?;
    net.advance(expiry)?;
    assert_eq!(net.session(ALICE)?.pending_request(BEN, net.now), None);
    assert!(matches!(
        net.act(
            BEN,
            MeetupAction::AnswerRequest {
                request: late,
                answer: allow(GrantScope::ThisMeetup),
            },
        ),
        Err(super::TestError::Meetup(MeetupError::UnknownRequest))
    ));
    Ok(())
}

#[test]
fn revoking_rotates_and_the_revoked_member_cannot_read_new_frames() -> TestResult {
    let mut net = chloe_sees_ben(6)?;
    net.grant(ALICE, BEN, GrantScope::ThisMeetup)?;
    let before = net.session(BEN)?.generation();
    net.take_events(CHLOE)?;
    net.take_events(ALICE)?;
    net.act(BEN, MeetupAction::RevokeLocation(CHLOE))?;
    assert_eq!(before.checked_next(), Some(net.session(BEN)?.generation()));
    assert!(!net.session(BEN)?.shares_with(CHLOE));
    // Alice was re-granted the new generation; Chloé was not.
    assert!(matches!(
        net.access(ALICE, BEN)?,
        ExactLocation::Shared { .. }
    ));
    net.advance(60)?;
    let chloe = net.take_events(CHLOE)?;
    assert_eq!(positions_from(&chloe, BEN), 0);
    assert!(chloe.contains(&MeetupEvent::LocationAccessChanged {
        peer: BEN,
        access: ExactLocation::Private,
    }));
    assert_eq!(net.access(CHLOE, BEN)?, ExactLocation::Private);
    assert_eq!(positions_from(&net.take_events(ALICE)?, BEN), 1);
    Ok(())
}

#[test]
fn a_stale_frame_of_an_older_generation_is_ignored() -> TestResult {
    let mut net = chloe_sees_ben(7)?;
    net.advance(60)?;
    let old_frame = location_frames(&net.take_sent(BEN)?)
        .last()
        .map(|frame| (*frame).clone())
        .ok_or(super::TestError::Setup("no frame"))?;
    net.act(BEN, MeetupAction::RevokeLocation(ALICE))?;
    net.take_events(CHLOE)?;
    net.deliver(BEN, CHLOE, MeetupMessage::SealedLocation(old_frame))?;
    assert!(net.take_events(CHLOE)?.is_empty());
    assert!(matches!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { .. }
    ));
    Ok(())
}

#[test]
fn a_fifteen_minute_grant_lapses_and_rotates() -> TestResult {
    let mut net = Net::new(&asking(3), 8)?;
    // Off the 60 s frame grid, so the lapse is only on time if it is a deadline of its own.
    net.advance(30)?;
    net.fix(BEN, BEN_LATITUDE, BEN_LONGITUDE)?;
    net.grant(CHLOE, BEN, GrantScope::FifteenMinutes)?;
    let expires = net
        .now
        .checked_add(SHORT_GRANT)
        .ok_or(super::TestError::Setup("time"))?;
    assert_eq!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared {
            expires: Some(expires)
        }
    );
    let before = net.session(BEN)?.generation();
    net.advance(SHORT_GRANT.as_secs() - 1)?;
    assert!(net.session(BEN)?.shares_with(CHLOE));
    assert_eq!(net.session(BEN)?.generation(), before);
    net.advance(1)?;
    assert!(!net.session(BEN)?.shares_with(CHLOE));
    assert_eq!(before.checked_next(), Some(net.session(BEN)?.generation()));
    assert_eq!(net.access(CHLOE, BEN)?, ExactLocation::Private);
    Ok(())
}

#[test]
fn approving_twice_keeps_the_longer_scope() -> TestResult {
    let mut net = Net::new(&asking(3), 9)?;
    // Two requests answered in turn: the meetup-long one first, then the short one.
    let first = net.ask(CHLOE, BEN)?;
    let second = net.ask(CHLOE, BEN)?;
    for (request, scope) in [
        (first, GrantScope::ThisMeetup),
        (second, GrantScope::FifteenMinutes),
    ] {
        net.act(
            BEN,
            MeetupAction::AnswerRequest {
                request,
                answer: allow(scope),
            },
        )?;
    }
    assert_eq!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    net.advance(SHORT_GRANT.as_secs() + 60)?;
    assert!(net.session(BEN)?.shares_with(CHLOE));
    Ok(())
}

#[test]
fn any_member_leaving_rotates_so_the_leaver_reveals_nothing() -> TestResult {
    let mut net = chloe_sees_ben(10)?;
    let before = net.session(BEN)?.generation();
    // Alice never held Ben's key, yet her leaving still rotates.
    net.remove(ALICE)?;
    assert_eq!(before.checked_next(), Some(net.session(BEN)?.generation()));
    assert!(matches!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { .. }
    ));
    net.take_events(CHLOE)?;
    net.advance(60)?;
    assert_eq!(positions_from(&net.take_events(CHLOE)?, BEN), 1);
    Ok(())
}

#[test]
fn a_grantee_leaving_loses_the_key() -> TestResult {
    let mut net = chloe_sees_ben(11)?;
    let chloe = net.remove(CHLOE)?;
    assert!(!net.session(BEN)?.shares_with(CHLOE));
    // Chloé's old key cannot open Ben's new frames.
    net.advance(60)?;
    let frame = location_frames(&net.take_sent(BEN)?)
        .last()
        .map(|frame| (*frame).clone())
        .ok_or(super::TestError::Setup("no frame"))?;
    let mut chloe = chloe;
    let outputs = chloe.session.handle(
        crate::MeetupInput::Received {
            sender: BEN,
            epoch: net.epoch,
            message: MeetupMessage::SealedLocation(frame),
        },
        net.now,
        &mut net.rng,
    )?;
    assert!(!outputs.iter().any(|output| matches!(
        output,
        crate::MeetupOutput::Event(MeetupEvent::PeerLocation(_))
    )));
    Ok(())
}

#[test]
fn nobody_drops_requests_silently() -> TestResult {
    let mut setups = asking(3);
    if let Some(ben) = setups.get_mut(1) {
        ben.visibility = LocationVisibility::Nobody;
    }
    let mut net = Net::new(&setups, 12)?;
    net.take_sent(BEN)?;
    net.act(CHLOE, MeetupAction::AskLocation(BEN))?;
    assert!(net.take_events(BEN)?.is_empty());
    assert!(net.take_sent(BEN)?.is_empty());
    assert_eq!(net.access(CHLOE, BEN)?, ExactLocation::Private);
    Ok(())
}

#[test]
fn everyone_grants_every_member_at_start_and_on_joining() -> TestResult {
    let mut setups = asking(3);
    if let Some(ben) = setups.get_mut(1) {
        ben.visibility = LocationVisibility::Everyone;
    }
    let mut net = Net::new(&setups, 13)?;
    assert_eq!(
        net.access(ALICE, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    assert_eq!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    assert_eq!(net.access(BEN, ALICE)?, ExactLocation::Private);
    net.add(DAN, &Setup::with(LocationVisibility::AskEachTime))?;
    assert_eq!(
        net.access(DAN, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    Ok(())
}

#[test]
fn everyone_answers_an_ask_with_a_grant_and_no_prompt() -> TestResult {
    let mut setups = asking(3);
    if let Some(ben) = setups.get_mut(1) {
        ben.visibility = LocationVisibility::Everyone;
    }
    let mut net = Net::new(&setups, 14)?;
    net.take_sent(BEN)?;
    net.act(CHLOE, MeetupAction::AskLocation(BEN))?;
    assert!(net.take_events(BEN)?.is_empty());
    assert_eq!(count(&net.take_sent(BEN)?, "grant"), 1);
    Ok(())
}

#[test]
fn the_allow_list_approves_without_a_prompt() -> TestResult {
    let mut setups = asking(3);
    if let Some(ben) = setups.get_mut(1) {
        ben.always_allow = vec![CHLOE];
    }
    let mut net = Net::new(&setups, 15)?;
    net.act(CHLOE, MeetupAction::AskLocation(BEN))?;
    assert!(net.take_events(BEN)?.is_empty());
    assert_eq!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    // Alice is not on the list: she gets a prompt.
    net.act(ALICE, MeetupAction::AskLocation(BEN))?;
    assert_eq!(net.access(ALICE, BEN)?, ExactLocation::Private);
    assert!(net.last_request(BEN).is_ok());
    Ok(())
}

#[test]
fn always_allow_adds_the_requester_and_says_so() -> TestResult {
    let mut net = Net::new(&asking(3), 16)?;
    let request = net.ask(CHLOE, BEN)?;
    net.act(
        BEN,
        MeetupAction::AnswerRequest {
            request,
            answer: LocationAnswer::Allow {
                scope: GrantScope::FifteenMinutes,
                ask_back: AskBack::DoNotAskBack,
                always: AllowListChoice::AddToAllowList,
            },
        },
    )?;
    assert!(
        net.take_events(BEN)?
            .contains(&MeetupEvent::AlwaysAllowAdded(CHLOE))
    );
    // After the grant lapses, Chloé's next ask is approved without a prompt.
    net.advance(SHORT_GRANT.as_secs())?;
    assert_eq!(net.access(CHLOE, BEN)?, ExactLocation::Private);
    net.act(CHLOE, MeetupAction::AskLocation(BEN))?;
    assert!(net.take_events(BEN)?.is_empty());
    assert_eq!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    Ok(())
}

#[test]
fn asking_back_sends_a_request_to_the_requester() -> TestResult {
    let mut net = Net::new(&asking(3), 17)?;
    let request = net.ask(CHLOE, BEN)?;
    net.act(
        BEN,
        MeetupAction::AnswerRequest {
            request,
            answer: LocationAnswer::Allow {
                scope: GrantScope::ThisMeetup,
                ask_back: AskBack::AskBack,
                always: AllowListChoice::JustThisTime,
            },
        },
    )?;
    assert!(
        net.take_events(CHLOE)?.iter().any(
            |event| matches!(event, MeetupEvent::LocationRequested { from, .. } if *from == BEN)
        )
    );
    assert!(
        !net.take_events(BEN)?
            .contains(&MeetupEvent::AlwaysAllowAdded(CHLOE))
    );
    assert!(net.session(BEN)?.pending_request(CHLOE, net.now).is_some());
    Ok(())
}

#[test]
fn an_ask_naming_someone_other_than_its_sender_is_ignored() -> TestResult {
    let mut net = Net::new(&asking(3), 18)?;
    let ben_key = net.device(BEN)?.member.grant_key.clone();
    let forged = LocationAsk {
        group: net.group,
        requester: CHLOE,
        request: LocationRequestId::from_bytes([9; 16]),
        asked_at: net.now,
    };
    let sealed = seal_location_ask(&ben_key, &forged, &mut net.rng)?;
    net.deliver(ALICE, BEN, MeetupMessage::LocationAsk(sealed))?;
    assert!(net.take_events(BEN)?.is_empty());
    Ok(())
}

#[test]
fn an_ask_older_than_ten_minutes_is_ignored() -> TestResult {
    let mut net = Net::new(&asking(3), 19)?;
    net.advance(ASK_EXPIRY.as_secs())?;
    let ben_key = net.device(BEN)?.member.grant_key.clone();
    let group = net.group;
    let stale = |asked_at| LocationAsk {
        group,
        requester: CHLOE,
        request: LocationRequestId::from_bytes([9; 16]),
        asked_at,
    };
    let old = seal_location_ask(&ben_key, &stale(at(0)), &mut net.rng)?;
    net.deliver(CHLOE, BEN, MeetupMessage::LocationAsk(old))?;
    assert!(net.take_events(BEN)?.is_empty());
    let recent = seal_location_ask(&ben_key, &stale(at(1)), &mut net.rng)?;
    net.deliver(CHLOE, BEN, MeetupMessage::LocationAsk(recent))?;
    assert_eq!(net.take_events(BEN)?.len(), 1);
    Ok(())
}

#[test]
fn a_repeated_ask_prompts_once() -> TestResult {
    let mut net = Net::new(&asking(3), 20)?;
    let ben_key = net.device(BEN)?.member.grant_key.clone();
    let ask = LocationAsk {
        group: net.group,
        requester: CHLOE,
        request: LocationRequestId::from_bytes([9; 16]),
        asked_at: net.now,
    };
    for _ in 0..2 {
        let sealed = seal_location_ask(&ben_key, &ask, &mut net.rng)?;
        net.deliver(CHLOE, BEN, MeetupMessage::LocationAsk(sealed))?;
    }
    assert_eq!(net.take_events(BEN)?.len(), 1);
    Ok(())
}

#[test]
fn a_holder_who_asks_again_gets_the_key_again() -> TestResult {
    let mut net = chloe_sees_ben(21)?;
    net.advance(60)?;
    net.take_sent(BEN)?;
    net.take_events(BEN)?;
    net.take_events(CHLOE)?;
    net.act(CHLOE, MeetupAction::AskLocation(BEN))?;
    assert!(net.take_events(BEN)?.is_empty());
    assert_eq!(count(&net.take_sent(BEN)?, "grant"), 1);
    // The same key again: no access change, and the last position is kept for the compass.
    assert!(net.take_events(CHLOE)?.is_empty());
    let observer = crate::Observer {
        fix: pingers_geo::Fix {
            coordinate: pingers_geo::Coordinate::new(0.0, 0.0)?,
            accuracy: pingers_geo::Meters::from_whole(5),
            taken_at: net.now,
        },
        heading: None,
    };
    assert!(
        net.session(CHLOE)?
            .compass(crate::CompassTarget::Member(BEN), &observer, net.now)
            .is_some()
    );
    Ok(())
}

#[test]
fn a_first_grant_raises_one_access_change() -> TestResult {
    let mut net = chloe_sees_ben(30)?;
    let changes = net
        .take_events(CHLOE)?
        .into_iter()
        .filter(|event| matches!(event, MeetupEvent::LocationAccessChanged { .. }))
        .count();
    assert_eq!(changes, 1);
    Ok(())
}

#[test]
fn a_new_grant_of_the_same_generation_updates_the_end() -> TestResult {
    let mut net = Net::new(&asking(3), 31)?;
    let first = net.ask(CHLOE, BEN)?;
    let second = net.ask(CHLOE, BEN)?;
    for (request, scope) in [
        (first, GrantScope::FifteenMinutes),
        (second, GrantScope::ThisMeetup),
    ] {
        net.act(
            BEN,
            MeetupAction::AnswerRequest {
                request,
                answer: allow(scope),
            },
        )?;
    }
    assert_eq!(
        net.access(CHLOE, BEN)?,
        ExactLocation::Shared { expires: None }
    );
    Ok(())
}

#[test]
fn an_old_grant_arriving_after_a_rotation_is_ignored() -> TestResult {
    let mut net = chloe_sees_ben(32)?;
    let old_grant = net
        .device(BEN)?
        .sent
        .iter()
        .rev()
        .find_map(|sent| match &sent.frame.message {
            MeetupMessage::LocationGrant(grant) => Some(grant.clone()),
            _ => None,
        })
        .ok_or(super::TestError::Setup("no grant"))?;
    net.act(BEN, MeetupAction::RevokeLocation(ALICE))?;
    net.deliver(BEN, CHLOE, MeetupMessage::LocationGrant(old_grant))?;
    net.take_events(CHLOE)?;
    net.advance(60)?;
    assert_eq!(positions_from(&net.take_events(CHLOE)?, BEN), 1);
    Ok(())
}

#[test]
fn a_member_who_leaves_takes_pending_requests_with_them() -> TestResult {
    let mut net = Net::new(&asking(3), 33)?;
    let request = net.ask(CHLOE, BEN)?;
    net.act(BEN, MeetupAction::AskLocation(ALICE))?;
    net.remove(CHLOE)?;
    net.remove(ALICE)?;
    assert_eq!(net.session(BEN)?.pending_request(ALICE, net.now), None);
    assert!(matches!(
        net.act(
            BEN,
            MeetupAction::AnswerRequest {
                request,
                answer: allow(GrantScope::ThisMeetup),
            },
        ),
        Err(super::TestError::Meetup(MeetupError::UnknownRequest))
    ));
    Ok(())
}

#[test]
fn a_newcomer_on_a_freed_leaf_inherits_no_approval_or_revoke() -> TestResult {
    let mut net = Net::new(&asking(3), 34)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    net.remove(CHLOE)?;
    // Someone new gets Chloé's leaf: Chloé's approval does not carry over.
    net.add(CHLOE, &Setup::with(LocationVisibility::AskEachTime))?;
    assert!(!net.session(BEN)?.shares_with(CHLOE));
    // Someone new gets Alice's leaf after Alice was revoked: the revoke does not carry over.
    net.act(
        BEN,
        MeetupAction::SetVisibility(LocationVisibility::Everyone),
    )?;
    net.act(BEN, MeetupAction::RevokeLocation(ALICE))?;
    net.remove(ALICE)?;
    net.add(ALICE, &Setup::with(LocationVisibility::AskEachTime))?;
    assert!(net.session(BEN)?.shares_with(ALICE));
    Ok(())
}

#[test]
fn someone_elses_meet_at_me_grants_nothing() -> TestResult {
    let mut net = Net::new(&asking(3), 35)?;
    net.act(CHLOE, MeetupAction::SetMeetPoint(MeetPointDraft::AtMe))?;
    assert!(!net.session(BEN)?.shares_with(ALICE));
    Ok(())
}

#[test]
fn switching_to_nobody_revokes_everyone_and_drops_pending_requests() -> TestResult {
    let mut net = chloe_sees_ben(22)?;
    let pending = net.ask(ALICE, BEN)?;
    let before = net.session(BEN)?.generation();
    net.act(BEN, MeetupAction::SetVisibility(LocationVisibility::Nobody))?;
    assert_eq!(net.session(BEN)?.visibility(), LocationVisibility::Nobody);
    assert_eq!(before.checked_next(), Some(net.session(BEN)?.generation()));
    assert!(!net.session(BEN)?.shares_with(CHLOE));
    assert!(matches!(
        net.act(
            BEN,
            MeetupAction::AnswerRequest {
                request: pending,
                answer: allow(GrantScope::ThisMeetup),
            },
        ),
        Err(super::TestError::Meetup(MeetupError::UnknownRequest))
    ));
    Ok(())
}

#[test]
fn leaving_everyone_for_ask_each_time_revokes_the_automatic_grants() -> TestResult {
    let mut net = Net::new(&asking(3), 23)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    net.act(
        BEN,
        MeetupAction::SetVisibility(LocationVisibility::Everyone),
    )?;
    assert!(net.session(BEN)?.shares_with(ALICE));
    net.act(
        BEN,
        MeetupAction::SetVisibility(LocationVisibility::AskEachTime),
    )?;
    // Chloé was approved explicitly and keeps access; Alice only had the automatic grant.
    assert!(net.session(BEN)?.shares_with(CHLOE));
    assert!(!net.session(BEN)?.shares_with(ALICE));
    Ok(())
}

#[test]
fn setting_the_same_visibility_sends_nothing() -> TestResult {
    let mut net = Net::new(&asking(3), 24)?;
    let outputs = net.act(
        BEN,
        MeetupAction::SetVisibility(LocationVisibility::AskEachTime),
    )?;
    assert!(outputs.is_empty());
    Ok(())
}

#[test]
fn revoking_under_everyone_excludes_that_member_until_approved_again() -> TestResult {
    let mut setups = asking(3);
    if let Some(ben) = setups.get_mut(1) {
        ben.visibility = LocationVisibility::Everyone;
    }
    let mut net = Net::new(&setups, 25)?;
    net.act(BEN, MeetupAction::RevokeLocation(CHLOE))?;
    assert!(!net.session(BEN)?.shares_with(CHLOE));
    assert!(net.session(BEN)?.shares_with(ALICE));
    // Asking again prompts instead of granting automatically.
    let request = net.ask(CHLOE, BEN)?;
    assert!(!net.session(BEN)?.shares_with(CHLOE));
    net.act(
        BEN,
        MeetupAction::AnswerRequest {
            request,
            answer: allow(GrantScope::ThisMeetup),
        },
    )?;
    assert!(net.session(BEN)?.shares_with(CHLOE));
    Ok(())
}

#[test]
fn meet_at_me_grants_everyone_until_the_meet_point_moves() -> TestResult {
    let mut net = Net::new(&asking(3), 26)?;
    net.act(BEN, MeetupAction::SetMeetPoint(MeetPointDraft::AtMe))?;
    assert!(net.session(BEN)?.shares_with(ALICE));
    assert!(net.session(BEN)?.shares_with(CHLOE));
    let before = net.session(BEN)?.generation();
    // Alice moves the meet point: Ben's implicit grants end and his key rotates.
    net.act(ALICE, MeetupAction::SetMeetPoint(MeetPointDraft::Clear))?;
    assert!(!net.session(BEN)?.shares_with(ALICE));
    assert_eq!(before.checked_next(), Some(net.session(BEN)?.generation()));
    Ok(())
}

#[test]
fn meet_at_me_does_not_override_nobody() -> TestResult {
    let mut setups = asking(3);
    if let Some(ben) = setups.get_mut(1) {
        ben.visibility = LocationVisibility::Nobody;
    }
    let mut net = Net::new(&setups, 27)?;
    net.act(BEN, MeetupAction::SetMeetPoint(MeetPointDraft::AtMe))?;
    assert!(!net.session(BEN)?.shares_with(ALICE));
    Ok(())
}

#[test]
fn actions_naming_self_or_a_stranger_are_rejected() -> TestResult {
    let mut net = Net::new(&asking(2), 28)?;
    assert!(matches!(
        net.act(BEN, MeetupAction::AskLocation(BEN)),
        Err(super::TestError::Meetup(MeetupError::CannotTargetSelf))
    ));
    assert!(matches!(
        net.act(BEN, MeetupAction::RevokeLocation(DAN)),
        Err(super::TestError::Meetup(MeetupError::NotAMember))
    ));
    Ok(())
}

#[test]
fn a_grant_for_someone_else_is_not_opened() -> TestResult {
    let mut net = chloe_sees_ben(29)?;
    // Alice received the same grant frame that Chloé opened.
    assert_eq!(net.access(ALICE, BEN)?, ExactLocation::Private);
    assert!(
        !net.take_events(ALICE)?
            .iter()
            .any(|event| matches!(event, MeetupEvent::LocationAccessChanged { .. }))
    );
    Ok(())
}
