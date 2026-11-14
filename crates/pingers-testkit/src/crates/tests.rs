// SPDX-License-Identifier: AGPL-3.0-only
//! [`FakeCrates`] itself: what each port records and answers, how a failing crate fails, and
//! what the sections read. pingers-client's tests rely on all of it; these pin it down here, in
//! the crate that owns it.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use pingers_client::snapshot::{PartyColour, PostQuantum};
use pingers_client::{Fallback, Unavailable};
use pingers_gate::{PauseState, SuppressReason};
use pingers_geo::{Circle, Coordinate, Fence, GeoError, Meters};
use pingers_meetup::{AllowListChoice, AskBack, GrantScope};
use pingers_proto::{PushPlatform, PushToken, SecretText};

use super::*;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error(transparent)]
    Fakes(#[from] FakeCratesError),
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Geo(#[from] GeoError),
    #[error(transparent)]
    Gate(#[from] GateError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error("a fake port call did not finish at once")]
    Pending,
}

type TestResult = Result<(), TestError>;

const PARTY: PartyId = PartyId::from_bytes([1; 16]);
const MEETUP: GroupId = GroupId::from_bytes([2; 32]);
const PEER: LeafIndex = LeafIndex::new(3);
const INVITE: MailboxId = MailboxId::from_bytes([4; 16]);
const REQUEST: LocationRequestId = LocationRequestId::from_bytes([5; 16]);

/// Every fake port call finishes without waiting, so one poll gives its answer.
fn now<T>(future: impl Future<Output = T>) -> Result<T, TestError> {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Ok(value),
        Poll::Pending => Err(TestError::Pending),
    }
}

fn party_draft() -> Result<PartyDraft, TestError> {
    Ok(PartyDraft {
        server: None,
        name: Name::new("Climbing crew")?,
        my_name: Name::new("Ana")?,
        colour: PartyColour::Teal,
        post_quantum: PostQuantum::On,
    })
}

fn spot_draft() -> Result<SpotDraft, TestError> {
    Ok(SpotDraft {
        id: None,
        name: Name::new("Boulder hall")?,
        fence: Fence::Circle(Circle::new(
            Coordinate::new(51.5, -0.1)?,
            Meters::from_whole(150),
        )?),
    })
}

fn tokens() -> PushTokens {
    PushTokens {
        platform: PushPlatform::UnifiedPush,
        token: PushToken::from_bytes(b"token".to_vec()),
    }
}

fn allow() -> LocationAnswer {
    LocationAnswer::Allow {
        scope: GrantScope::FifteenMinutes,
        ask_back: AskBack::AskBack,
        always: AllowListChoice::JustThisTime,
    }
}

/// Calls every port method once through [`FakeCrates::crates`], checking each answer, and
/// returns the calls it expects to have been recorded, in order.
fn call_every_port(fakes: &FakeCrates) -> Result<Vec<CrateCall>, TestError> {
    let crates = fakes.crates();
    let (parties, sync, wake, meetups) =
        (&crates.parties, &crates.sync, &crates.wake, &crates.meetups);
    let url = InviteUrl::new("https://pingers.test/join#k")?;
    let body = ChatBody::new(String::from("Here"))?;
    let change = SettingChange::Pause(PauseState::Paused);

    assert_eq!(
        now(parties.create_party(party_draft()?))??,
        FakeCrates::PARTY
    );
    let link = now(parties.create_invite(PARTY))??;
    assert_eq!(link.url.as_str(), "https://pingers.test/join#secret");
    assert_eq!(link.expires_at, FakeCrates::INVITE_EXPIRES);
    now(parties.open_invite(url.clone(), Name::new("Bo")?))??;
    now(parties.withdraw_join())??;
    now(parties.dismiss_join())??;
    now(parties.decide_join(PARTY, INVITE, JoinDecision::Refuse))??;
    now(parties.cancel_invite(PARTY, INVITE))??;
    now(parties.set_display_name(Name::new("Sam")?))??;
    now(parties.remove_member(PARTY, PEER))??;
    now(parties.leave_party(PARTY))??;
    assert_eq!(
        now(parties.save_spot(PARTY, spot_draft()?))??,
        FakeCrates::SPOT
    );
    now(parties.delete_spot(PARTY, FakeCrates::SPOT))??;
    now(parties.set_always_allow(PARTY, PEER, AllowListChange::Remove))??;
    assert_eq!(
        now(parties.safety_code(PARTY, PEER))??.as_str(),
        FakeCrates::SAFETY_CODE
    );
    now(parties.mark_checked(PARTY, PEER))??;
    now(sync.push_tokens_changed(Some(tokens())))??;
    now(sync.set_wake_pushes(WakePushes::On, None))??;
    assert_eq!(now(sync.check_reachability())??, RelayStatus::Reachable);
    now(sync.submit_registration_code(None, SecretText::new("otter".into())))??;
    assert_eq!(
        now(wake.run(WakeInput::Scheduled))??,
        WakeOutcome::default()
    );
    now(wake.change_setting(change.clone()))??;
    assert_eq!(
        now(meetups.send_chat(MEETUP, body.clone(), None))??,
        FakeCrates::MESSAGE
    );
    now(meetups.set_meet_point(MEETUP, MeetPointDraft::AtMe))??;
    assert_eq!(
        now(meetups.ask_location(MEETUP, PEER))??,
        FakeCrates::REQUEST
    );
    now(meetups.answer_location_request(MEETUP, REQUEST, allow()))??;
    now(meetups.revoke_location(MEETUP, PEER))??;
    now(meetups.leave_meetup(MEETUP))??;

    Ok(vec![
        CrateCall::CreateParty(party_draft()?),
        CrateCall::CreateInvite(PARTY),
        CrateCall::OpenInvite(url, Name::new("Bo")?),
        CrateCall::WithdrawJoin,
        CrateCall::DismissJoin,
        CrateCall::DecideJoin(PARTY, INVITE, JoinDecision::Refuse),
        CrateCall::CancelInvite(PARTY, INVITE),
        CrateCall::SetDisplayName(Name::new("Sam")?),
        CrateCall::RemoveMember(PARTY, PEER),
        CrateCall::LeaveParty(PARTY),
        CrateCall::SaveSpot(PARTY, spot_draft()?),
        CrateCall::DeleteSpot(PARTY, FakeCrates::SPOT),
        CrateCall::SetAlwaysAllow(PARTY, PEER, AllowListChange::Remove),
        CrateCall::SafetyCode(PARTY, PEER),
        CrateCall::MarkChecked(PARTY, PEER),
        CrateCall::PushTokensChanged(Some(tokens())),
        CrateCall::SetWakePushes(WakePushes::On, None),
        CrateCall::CheckReachability,
        CrateCall::SubmitRegistrationCode(None, "otter".into()),
        CrateCall::Wake(WakeInput::Scheduled),
        CrateCall::ChangeSetting(change),
        CrateCall::SendChat(MEETUP, body, None),
        CrateCall::SetMeetPoint(MEETUP, MeetPointDraft::AtMe),
        CrateCall::AskLocation(MEETUP, PEER),
        CrateCall::AnswerLocationRequest(MEETUP, REQUEST, allow()),
        CrateCall::RevokeLocation(MEETUP, PEER),
        CrateCall::LeaveMeetup(MEETUP),
    ])
}

#[test]
fn every_port_call_is_recorded_in_order_with_its_arguments() -> TestResult {
    let fakes = FakeCrates::new()?;
    assert_eq!(fakes.calls(), Vec::new());
    let expected = call_every_port(&fakes)?;
    assert_eq!(fakes.calls(), expected);
    assert_eq!(fakes.section_reads(), 0);
    Ok(())
}

/// One call to `crate_name`'s port and one read of its section, answers discarded.
fn touch(
    fakes: &FakeCrates,
    crate_name: CrateName,
) -> Result<[Result<(), ClientError>; 2], TestError> {
    let crates = fakes.crates();
    Ok(match crate_name {
        CrateName::Parties => [
            now(crates.parties.leave_party(PARTY))?,
            now(crates.parties.section())?.map(drop),
        ],
        CrateName::Sync => [
            now(crates.sync.check_reachability())?.map(drop),
            now(crates.sync.section())?.map(drop),
        ],
        CrateName::Wake => [
            now(crates.wake.run(WakeInput::Scheduled))?.map(drop),
            now(crates.wake.section())?.map(drop),
        ],
        CrateName::Meetups => [
            now(crates.meetups.leave_meetup(MEETUP))?,
            now(crates.meetups.section())?.map(drop),
        ],
    })
}

const ALL: [CrateName; 4] = [
    CrateName::Parties,
    CrateName::Sync,
    CrateName::Wake,
    CrateName::Meetups,
];

#[test]
fn a_failing_crate_fails_only_its_own_calls_with_its_own_error() -> TestResult {
    let fakes = FakeCrates::new()?;
    for failing in ALL {
        fakes.set_failing(Some(failing));
        for crate_name in ALL {
            for outcome in touch(&fakes, crate_name)? {
                if crate_name == failing {
                    let message = outcome.err().map(|error| format!("{error:?}"));
                    assert_eq!(message, Some(format!("{:?}", error(failing))));
                } else {
                    assert!(outcome.is_ok(), "{crate_name:?} failed with {failing:?}");
                }
            }
        }
    }
    fakes.set_failing(None);
    for crate_name in ALL {
        assert!(touch(&fakes, crate_name)?.iter().all(Result::is_ok));
    }
    // A failing call is still recorded: one port call per touch, five rounds of four.
    assert_eq!(fakes.calls().len(), 20);
    assert_eq!(fakes.section_reads(), 20);
    Ok(())
}

#[test]
fn each_crate_fails_with_an_error_of_its_own_type() {
    assert!(matches!(
        error(CrateName::Parties),
        ClientError::Parties(pingers_parties::PartiesError::Internal)
    ));
    assert!(matches!(
        error(CrateName::Sync),
        ClientError::Unavailable(Unavailable::Sync)
    ));
    assert!(matches!(
        error(CrateName::Wake),
        ClientError::Wake(pingers_wake::WakeError::Presence(
            pingers_presence::PresenceError::NotInside
        ))
    ));
    assert!(matches!(
        error(CrateName::Meetups),
        ClientError::MeetupSession(pingers_meetup_session::SessionError::Internal)
    ));
}

#[test]
fn new_fakes_share_with_the_gate_open_and_the_relay_reachable() -> TestResult {
    let fakes = FakeCrates::new()?;
    let crates = fakes.crates();
    let pipeline = now(crates.wake.section())??;
    assert_eq!(pipeline.gate, GateDecision::Share);
    assert_eq!(pipeline.settings.location_sharing, LocationSharing::On);
    assert_eq!(pipeline.settings.pause_in_focus, PauseInFocus::On);
    assert_eq!(pipeline.settings.focus, FocusState::Off);
    assert_eq!(pipeline.settings.quiet_hours.state, Toggle::Off);
    assert_eq!(
        pipeline.settings.quiet_hours.window,
        DailyWindow::new(MinuteOfDay::new(1380)?, MinuteOfDay::new(420)?)?
    );
    assert_eq!(
        pipeline.settings.visibility,
        LocationVisibility::AskEachTime
    );
    assert_eq!(pipeline.settings.wake_pushes, WakePushes::On);
    assert_eq!(pipeline.settings.hide_trail, HideTrail::On);
    assert!(pipeline.zones.is_empty() && pipeline.own_position.is_none());
    let server = now(crates.sync.section())??;
    assert_eq!(server.relay, RelayStatus::Reachable);
    assert!(server.fallbacks.is_empty() && server.map_host.is_none());
    assert_eq!(now(crates.parties.section())??, PartiesSection::default());
    assert_eq!(now(crates.meetups.section())??, MeetupsSection::default());
    assert_eq!(
        now(crates.wake.run(WakeInput::Scheduled))??,
        WakeOutcome::default()
    );
    Ok(())
}

#[test]
fn sections_and_answers_follow_what_the_test_set_and_reads_are_counted() -> TestResult {
    let fakes = FakeCrates::new()?;
    let crates = fakes.crates();

    let parties = PartiesSection {
        display_name: Some(Name::new("Kit")?),
        ..PartiesSection::default()
    };
    fakes.set_parties(parties.clone());
    let mut pipeline = now(crates.wake.section())??;
    pipeline.gate = GateDecision::Suppress(SuppressReason::Paused);
    fakes.set_pipeline(pipeline.clone());
    let server = ServerSection {
        fallbacks: vec![Fallback::PrivacyPassOnly],
        relay: RelayStatus::KeyMismatch,
        map_host: None,
        operators: None,
    };
    fakes.set_server(server.clone());
    let meetups = MeetupsSection {
        alert: Some(MEETUP),
        ..MeetupsSection::default()
    };
    fakes.set_meetups(meetups.clone());
    let outcome = WakeOutcome {
        next_wake: Some(Duration::from_mins(15)),
        ..WakeOutcome::default()
    };
    fakes.set_wake_outcome(outcome.clone());

    let reads = fakes.section_reads();
    assert_eq!(now(crates.parties.section())??, parties);
    assert_eq!(now(crates.wake.section())??, pipeline);
    assert_eq!(now(crates.sync.section())??, server);
    assert_eq!(now(crates.meetups.section())??, meetups);
    assert_eq!(fakes.section_reads(), reads.saturating_add(4));
    assert_eq!(
        now(crates.sync.check_reachability())??,
        RelayStatus::KeyMismatch
    );
    assert_eq!(now(crates.wake.run(WakeInput::Scheduled))??, outcome);
    // Answers are not section reads.
    assert_eq!(fakes.section_reads(), reads.saturating_add(4));
    Ok(())
}

#[test]
fn a_display_name_shows_in_the_section_unless_parties_is_failing() -> TestResult {
    let fakes = FakeCrates::new()?;
    let parties = fakes.crates().parties;
    fakes.set_failing(Some(CrateName::Parties));
    assert!(now(parties.set_display_name(Name::new("Lee")?))?.is_err());
    fakes.set_failing(None);
    assert_eq!(now(parties.section())??.display_name, None);
    now(parties.set_display_name(Name::new("Sam")?))??;
    assert_eq!(
        now(parties.section())??.display_name,
        Some(Name::new("Sam")?)
    );
    Ok(())
}

#[test]
fn clones_share_one_state() -> TestResult {
    let fakes = FakeCrates::new()?;
    let clone = fakes.clone();
    now(clone.crates().meetups.leave_meetup(MEETUP))??;
    clone.set_failing(Some(CrateName::Meetups));
    assert_eq!(fakes.calls(), vec![CrateCall::LeaveMeetup(MEETUP)]);
    assert!(now(fakes.crates().meetups.leave_meetup(MEETUP))?.is_err());
    Ok(())
}
