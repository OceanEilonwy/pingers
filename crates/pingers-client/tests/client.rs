// SPDX-License-Identifier: AGPL-3.0-only
//! The composition root against a fake platform and fake crates: opening, and each method
//! reaching exactly one crate with its arguments.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pingers_client::ports::{
    AllowListChange, ClientEvent, FixRequest, Foreground, GeofenceTransition, JoinDecision,
    MeetupsSection, PartiesSection, PartyDraft, PipelineSection, ServerSection, SettingChange,
    SpotDraft, WakeInput, WakeOutcome,
};
use pingers_client::snapshot::{
    ClientSnapshot, HideTrail, InviteUrl, Name, OwnPosition, PartyColour, PostQuantum, RelayStatus,
    SnapshotError, WakePushes,
};
use pingers_client::{
    Client, ClientConfig, ClientError, Fallback, FixQuality, LocationMode, Notice, OsRegion,
    PlatformError, PushTokens, Unavailable,
};
use pingers_gate::{FocusState, GateDecision, PauseState, SuppressReason};
use pingers_geo::{Circle, Coordinate, Fence, Fix, GeoError, Meters};
use pingers_meetup::{AllowListChoice, AskBack, GrantScope, LocationAnswer, MeetPointDraft};
use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, PartyId, ProtoError,
    PushHandle, PushPlatform, PushToken, SpotId,
};
use pingers_store::StoreError;
use pingers_testkit::{
    CrateCall, CrateName, FakeCrates, FakeCratesError, FakePlatform, Geofence, PushRegistration,
    RecordedEvent, RecordingEvents, StoreAvailability, WakeRequest, test_clock,
};
use pingers_time::UnixSeconds;

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
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("secure store")]
    SecureStore,
}

const PARTY: PartyId = PartyId::from_bytes([1; 16]);
const MEETUP: GroupId = GroupId::from_bytes([2; 32]);
const PEER: LeafIndex = LeafIndex::new(3);
const KEY: [u8; 32] = [7; 32];

/// A fresh directory under the system temporary directory, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pingers-client-test-{}-{unique}",
            std::process::id()
        ));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn config(&self) -> ClientConfig {
        ClientConfig {
            database: self.0.join("pingers.db"),
            instance: None,
        }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// A client over fresh fakes, with its handles.
struct Rig {
    client: Client,
    platform: FakePlatform,
    events: RecordingEvents,
    fakes: FakeCrates,
    _dir: TestDir,
}

fn platform_with_key(key: &[u8]) -> Result<FakePlatform, TestError> {
    let platform = FakePlatform::new(test_clock());
    platform
        .secure_store_put("pingers.database-key", key)
        .map_err(|_locked| TestError::SecureStore)?;
    Ok(platform)
}

async fn open(
    dir: &TestDir,
    platform: &FakePlatform,
    fakes: &FakeCrates,
) -> Result<Client, ClientError> {
    let crates = fakes.crates();
    Client::open_with(
        dir.config(),
        Arc::new(platform.clone()),
        Arc::new(RecordingEvents::new()),
        move |_database| Ok(crates),
    )
    .await
}

async fn rig() -> Result<Rig, TestError> {
    let dir = TestDir::new()?;
    let platform = platform_with_key(&KEY)?;
    let events = RecordingEvents::new();
    let fakes = FakeCrates::new()?;
    let crates = fakes.crates();
    let client = Client::open_with(
        dir.config(),
        Arc::new(platform.clone()),
        Arc::new(events.clone()),
        move |_database| Ok(crates),
    )
    .await?;
    Ok(Rig {
        client,
        platform,
        events,
        fakes,
        _dir: dir,
    })
}

impl Rig {
    /// Checks that the last call was `call` and that one snapshot was published after it.
    fn expect_last(&self, call: &CrateCall, reads_before: usize) {
        assert_eq!(self.fakes.calls().last(), Some(call));
        assert_eq!(self.fakes.section_reads(), reads_before.saturating_add(4));
    }
}

fn name(text: &str) -> Result<Name, SnapshotError> {
    Name::new(text)
}

#[tokio::test]
async fn open_builds_the_crates_and_publishes_their_sections() -> Result<(), TestError> {
    let dir = TestDir::new()?;
    let platform = platform_with_key(&KEY)?;
    let fakes = FakeCrates::new()?;
    let fix = Fix {
        coordinate: Coordinate::new(51.5, -0.1)?,
        accuracy: Meters::from_whole(20),
        taken_at: UnixSeconds::new(1_800_000_000),
    };
    fakes.set_parties(PartiesSection {
        parties: Vec::new(),
        joining: None,
        display_name: Some(Name::new("Sam")?),
    });
    let mut pipeline = fakes_pipeline(&fakes).await?;
    pipeline.gate = GateDecision::Suppress(SuppressReason::Paused);
    pipeline.own_position = Some(OwnPosition { fix, heading: None });
    pipeline.settings.hide_trail = HideTrail::Off;
    fakes.set_pipeline(pipeline.clone());
    fakes.set_server(ServerSection {
        fallbacks: vec![Fallback::PrivacyPassOnly],
        relay: RelayStatus::KeyMismatch,
        map_host: None,
        operators: None,
        push_delivery: pingers_client::snapshot::PushDelivery::Full,
    });
    fakes.set_meetups(MeetupsSection {
        meetups: Vec::new(),
        alert: Some(MEETUP),
        location_requests: Vec::new(),
        ended_meetup: None,
    });
    let reads = fakes.section_reads();
    let client = open(&dir, &platform, &fakes).await?;
    let snapshot = client.snapshot();
    let expected = ClientSnapshot {
        settings: pipeline.settings,
        gate: pipeline.gate,
        parties: Vec::new(),
        zones: Vec::new(),
        meetups: Vec::new(),
        alert: Some(MEETUP),
        location_requests: Vec::new(),
        ended_meetup: None,
        joining: None,
        display_name: Some(Name::new("Sam")?),
        own_position: pipeline.own_position,
        fallbacks: vec![Fallback::PrivacyPassOnly],
        relay: RelayStatus::KeyMismatch,
        map_host: None,
        operators: None,
        push_delivery: pingers_client::snapshot::PushDelivery::Full,
    };
    assert_eq!(*snapshot, expected);
    assert_eq!(fakes.section_reads(), reads.saturating_add(4));
    assert!(fakes.calls().is_empty());
    assert!(dir.config().database.exists());
    Ok(())
}

/// The pipeline section the fakes start with.
async fn fakes_pipeline(fakes: &FakeCrates) -> Result<PipelineSection, ClientError> {
    pingers_client::ports::WakePort::section(fakes).await
}

#[tokio::test]
async fn open_needs_the_database_key_from_the_secure_store() -> Result<(), TestError> {
    let dir = TestDir::new()?;
    let fakes = FakeCrates::new()?;
    let empty = FakePlatform::new(test_clock());
    assert!(matches!(
        open(&dir, &empty, &fakes).await,
        Err(ClientError::NoDatabaseKey)
    ));
    let short = platform_with_key(&[7; 31])?;
    assert!(matches!(
        open(&dir, &short, &fakes).await,
        Err(ClientError::MalformedDatabaseKey)
    ));
    let long = platform_with_key(&[7; 33])?;
    assert!(matches!(
        open(&dir, &long, &fakes).await,
        Err(ClientError::MalformedDatabaseKey)
    ));
    let locked = platform_with_key(&KEY)?;
    locked.set_store_availability(StoreAvailability::Locked);
    assert!(matches!(
        open(&dir, &locked, &fakes).await,
        Err(ClientError::Platform(PlatformError::SecureStoreUnavailable))
    ));
    assert!(!dir.config().database.exists());
    Ok(())
}

#[tokio::test]
async fn open_uses_the_stored_key_for_the_database() -> Result<(), TestError> {
    let dir = TestDir::new()?;
    let fakes = FakeCrates::new()?;
    drop(open(&dir, &platform_with_key(&KEY)?, &fakes).await?);
    assert!(open(&dir, &platform_with_key(&KEY)?, &fakes).await.is_ok());
    assert!(matches!(
        open(&dir, &platform_with_key(&[8; 32])?, &fakes).await,
        Err(ClientError::Store(StoreError::WrongKey))
    ));
    Ok(())
}

#[tokio::test]
async fn open_fails_when_building_the_crates_fails() -> Result<(), TestError> {
    let dir = TestDir::new()?;
    let platform = platform_with_key(&KEY)?;
    let result = Client::open_with(
        dir.config(),
        Arc::new(platform),
        Arc::new(RecordingEvents::new()),
        |_database| Err(ClientError::Internal),
    )
    .await;
    assert!(matches!(result, Err(ClientError::Internal)));
    let fakes = FakeCrates::new()?;
    fakes.set_failing(Some(CrateName::Sync));
    let platform = platform_with_key(&KEY)?;
    assert!(matches!(
        open(&dir, &platform, &fakes).await,
        Err(ClientError::Unavailable(Unavailable::Sync))
    ));
    Ok(())
}

#[tokio::test]
async fn party_commands_reach_parties_and_republish() -> Result<(), TestError> {
    let rig = rig().await?;
    let client = &rig.client;
    let draft = PartyDraft {
        name: name("Climbing crew")?,
        my_name: name("Ana")?,
        colour: PartyColour::Teal,
        post_quantum: PostQuantum::On,
    };
    let reads = rig.fakes.section_reads();
    assert_eq!(client.create_party(draft.clone()).await?, FakeCrates::PARTY);
    rig.expect_last(&CrateCall::CreateParty(draft), reads);

    let reads = rig.fakes.section_reads();
    let link = client.create_invite(PARTY).await?;
    assert_eq!(link.expires_at, FakeCrates::INVITE_EXPIRES);
    rig.expect_last(&CrateCall::CreateInvite(PARTY), reads);

    let url = InviteUrl::new("https://pingers.test/join#k")?;
    let reads = rig.fakes.section_reads();
    client.open_invite(url.clone(), name("Ben")?).await?;
    rig.expect_last(&CrateCall::OpenInvite(url, name("Ben")?), reads);

    let reads = rig.fakes.section_reads();
    client.withdraw_join().await?;
    rig.expect_last(&CrateCall::WithdrawJoin, reads);

    let reads = rig.fakes.section_reads();
    client.dismiss_join().await?;
    rig.expect_last(&CrateCall::DismissJoin, reads);

    let invite = MailboxId::from_bytes([9; 16]);
    let reads = rig.fakes.section_reads();
    client
        .decide_join(PARTY, invite, JoinDecision::Refuse)
        .await?;
    rig.expect_last(
        &CrateCall::DecideJoin(PARTY, invite, JoinDecision::Refuse),
        reads,
    );

    let reads = rig.fakes.section_reads();
    client.cancel_invite(PARTY, invite).await?;
    rig.expect_last(&CrateCall::CancelInvite(PARTY, invite), reads);

    let reads = rig.fakes.section_reads();
    client.set_display_name(name("Sam")?).await?;
    rig.expect_last(&CrateCall::SetDisplayName(name("Sam")?), reads);
    assert_eq!(client.snapshot().display_name, Some(name("Sam")?));

    let reads = rig.fakes.section_reads();
    client.remove_member(PARTY, PEER).await?;
    rig.expect_last(&CrateCall::RemoveMember(PARTY, PEER), reads);

    let reads = rig.fakes.section_reads();
    client.leave_party(PARTY).await?;
    rig.expect_last(&CrateCall::LeaveParty(PARTY), reads);
    Ok(())
}

#[tokio::test]
async fn spot_and_allow_list_commands_reach_parties() -> Result<(), TestError> {
    let rig = rig().await?;
    let client = &rig.client;
    let spot = SpotDraft {
        id: None,
        name: name("Boulder hall")?,
        fence: Fence::Circle(Circle::new(
            Coordinate::new(51.5, -0.1)?,
            Meters::from_whole(150),
        )?),
    };
    let reads = rig.fakes.section_reads();
    assert_eq!(
        client.save_spot(PARTY, spot.clone()).await?,
        FakeCrates::SPOT
    );
    rig.expect_last(&CrateCall::SaveSpot(PARTY, spot), reads);

    let id = SpotId::from_bytes([4; 16]);
    let reads = rig.fakes.section_reads();
    client.delete_spot(PARTY, id).await?;
    rig.expect_last(&CrateCall::DeleteSpot(PARTY, id), reads);

    let reads = rig.fakes.section_reads();
    client
        .set_always_allow(PARTY, PEER, AllowListChange::Remove)
        .await?;
    rig.expect_last(
        &CrateCall::SetAlwaysAllow(PARTY, PEER, AllowListChange::Remove),
        reads,
    );
    Ok(())
}

#[tokio::test]
async fn settings_reach_wake_and_push_settings_reach_sync_with_the_tokens() -> Result<(), TestError>
{
    let rig = rig().await?;
    let client = &rig.client;
    let change = SettingChange::Pause(PauseState::Paused);
    let reads = rig.fakes.section_reads();
    client.change_setting(change.clone()).await?;
    rig.expect_last(&CrateCall::ChangeSetting(change), reads);

    let reads = rig.fakes.section_reads();
    client.set_wake_pushes(WakePushes::NoWakePushes).await?;
    rig.expect_last(
        &CrateCall::SetWakePushes(WakePushes::NoWakePushes, None),
        reads,
    );

    let token = PushToken::from_bytes(b"token".to_vec());
    rig.platform.set_push_registration(Some(PushRegistration {
        platform: PushPlatform::UnifiedPush,
        token: token.clone(),
    }));
    let tokens = Some(PushTokens {
        platform: PushPlatform::UnifiedPush,
        token,
    });
    let reads = rig.fakes.section_reads();
    client.set_wake_pushes(WakePushes::On).await?;
    rig.expect_last(
        &CrateCall::SetWakePushes(WakePushes::On, tokens.clone()),
        reads,
    );

    let reads = rig.fakes.section_reads();
    client.push_tokens_changed().await?;
    rig.expect_last(&CrateCall::PushTokensChanged(tokens), reads);
    Ok(())
}

#[tokio::test]
async fn a_reachability_check_reaches_sync_and_answers_its_status() -> Result<(), TestError> {
    let rig = rig().await?;
    rig.fakes.set_server(ServerSection {
        fallbacks: Vec::new(),
        relay: RelayStatus::Unreachable,
        map_host: None,
        operators: None,
        push_delivery: pingers_client::snapshot::PushDelivery::Full,
    });
    let reads = rig.fakes.section_reads();
    assert_eq!(
        rig.client.check_reachability().await?,
        RelayStatus::Unreachable
    );
    rig.expect_last(&CrateCall::CheckReachability, reads);
    assert_eq!(rig.client.snapshot().relay, RelayStatus::Unreachable);
    Ok(())
}

#[tokio::test]
async fn meetup_and_consent_commands_reach_the_meetup_session() -> Result<(), TestError> {
    let rig = rig().await?;
    let client = &rig.client;
    let body = ChatBody::new(String::from("Here"))?;
    let parent = Some(ChatMessageId::from_bytes([5; 16]));
    let reads = rig.fakes.section_reads();
    assert_eq!(
        client.send_chat(MEETUP, body.clone(), parent).await?,
        FakeCrates::MESSAGE
    );
    rig.expect_last(&CrateCall::SendChat(MEETUP, body, parent), reads);

    let reads = rig.fakes.section_reads();
    client.set_meet_point(MEETUP, MeetPointDraft::AtMe).await?;
    rig.expect_last(
        &CrateCall::SetMeetPoint(MEETUP, MeetPointDraft::AtMe),
        reads,
    );

    let reads = rig.fakes.section_reads();
    assert_eq!(
        client.ask_location(MEETUP, PEER).await?,
        FakeCrates::REQUEST
    );
    rig.expect_last(&CrateCall::AskLocation(MEETUP, PEER), reads);

    let request = LocationRequestId::from_bytes([6; 16]);
    let answer = LocationAnswer::Allow {
        scope: GrantScope::FifteenMinutes,
        ask_back: AskBack::AskBack,
        always: AllowListChoice::JustThisTime,
    };
    let reads = rig.fakes.section_reads();
    client
        .answer_location_request(MEETUP, request, answer)
        .await?;
    rig.expect_last(
        &CrateCall::AnswerLocationRequest(MEETUP, request, answer),
        reads,
    );

    let reads = rig.fakes.section_reads();
    client.revoke_location(MEETUP, PEER).await?;
    rig.expect_last(&CrateCall::RevokeLocation(MEETUP, PEER), reads);

    let reads = rig.fakes.section_reads();
    client.leave_meetup(MEETUP).await?;
    rig.expect_last(&CrateCall::LeaveMeetup(MEETUP), reads);
    Ok(())
}

#[tokio::test]
async fn each_pipeline_input_reaches_wake_as_itself() -> Result<(), TestError> {
    let rig = rig().await?;
    let client = &rig.client;
    let fix = Fix {
        coordinate: Coordinate::new(51.5, -0.1)?,
        accuracy: Meters::from_whole(20),
        taken_at: UnixSeconds::new(1_800_000_000),
    };
    let handle = PushHandle::from_bytes([8; 8]);
    rig.platform.set_focus(FocusState::On);
    client.on_location(fix).await?;
    client
        .on_os_geofence(String::from("spot-1"), GeofenceTransition::Exit)
        .await?;
    client.on_wake().await?;
    client.on_push(handle).await?;
    client.on_dnd_changed().await?;
    client.on_foreground(Foreground::Hidden).await?;
    assert_eq!(
        rig.fakes.calls(),
        [
            CrateCall::Wake(WakeInput::Fix(fix)),
            CrateCall::Wake(WakeInput::Geofence {
                region: String::from("spot-1"),
                transition: GeofenceTransition::Exit
            }),
            CrateCall::Wake(WakeInput::Scheduled),
            CrateCall::Wake(WakeInput::Push(handle)),
            CrateCall::Wake(WakeInput::FocusChanged(FocusState::On)),
            CrateCall::Wake(WakeInput::Foreground(Foreground::Hidden)),
        ]
    );
    // One snapshot at open, one per input.
    assert_eq!(rig.fakes.section_reads(), 4 * 7);
    Ok(())
}

#[tokio::test]
async fn a_wake_outcome_goes_to_the_platform_and_the_events() -> Result<(), TestError> {
    let rig = rig().await?;
    let region = Circle::new(Coordinate::new(51.5, -0.1)?, Meters::from_whole(200))?;
    let party = PartyId::from_bytes([10; 16]);
    rig.fakes.set_wake_outcome(WakeOutcome {
        geofences: Some(vec![OsRegion {
            id: String::from("spot-9"),
            region,
        }]),
        location_mode: Some(LocationMode::Dwell),
        fix: Some(FixRequest {
            quality: FixQuality::High,
            timeout: Duration::from_secs(15),
        }),
        next_wake: Some(Duration::from_mins(15)),
        events: vec![
            ClientEvent::PartiesChanged,
            ClientEvent::JoinRequested(party),
            ClientEvent::LetIn(party),
            ClientEvent::MeetupStarted(MEETUP),
            ClientEvent::MeetupUpdated(MEETUP),
            ClientEvent::MeetupEnded(MEETUP),
            ClientEvent::Notice(Notice::ClockSkew),
            ClientEvent::PrivacyStatusChanged,
        ],
    });
    rig.client.on_wake().await?;
    let platform = &rig.platform;
    assert_eq!(
        platform.os_geofences(),
        [Geofence {
            id: String::from("spot-9"),
            region
        }]
    );
    assert_eq!(
        platform.location_mode(),
        pingers_testkit::LocationMode::Dwell
    );
    let requests = platform.fix_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests
            .first()
            .map(|request| (request.quality, request.timeout)),
        Some((pingers_testkit::FixQuality::High, Duration::from_secs(15)))
    );
    assert_eq!(
        platform.pending_wake().map(|wake: WakeRequest| wake.after),
        Some(Duration::from_mins(15))
    );
    assert_eq!(
        rig.events.events(),
        [
            RecordedEvent::PartiesChanged,
            RecordedEvent::JoinRequested { party },
            RecordedEvent::LetIn { party },
            RecordedEvent::MeetupStarted { meetup: MEETUP },
            RecordedEvent::MeetupUpdated { meetup: MEETUP },
            RecordedEvent::MeetupEnded { meetup: MEETUP },
            RecordedEvent::Notice(Notice::ClockSkew),
            RecordedEvent::PrivacyStatusChanged,
        ]
    );
    Ok(())
}

#[tokio::test]
async fn an_empty_wake_outcome_leaves_the_platform_alone() -> Result<(), TestError> {
    let rig = rig().await?;
    rig.client.on_wake().await?;
    assert!(rig.platform.os_geofences().is_empty());
    assert_eq!(rig.platform.geofence_updates(), 0);
    assert!(rig.platform.location_mode_history().is_empty());
    assert!(rig.platform.fix_requests().is_empty());
    assert_eq!(rig.platform.pending_wake(), None);
    rig.events
        .expect_quiet()
        .map_err(|_noisy| TestError::SecureStore)?;
    Ok(())
}

#[tokio::test]
async fn a_failing_crate_fails_the_call_and_keeps_the_snapshot() -> Result<(), TestError> {
    let rig = rig().await?;
    let before = rig.client.snapshot();
    rig.fakes.set_failing(Some(CrateName::Parties));
    let reads = rig.fakes.section_reads();
    assert!(matches!(
        rig.client.leave_party(PARTY).await,
        Err(ClientError::Parties(_))
    ));
    assert_eq!(rig.fakes.section_reads(), reads);
    rig.fakes.set_failing(Some(CrateName::Wake));
    rig.fakes.set_wake_outcome(WakeOutcome {
        next_wake: Some(Duration::from_mins(1)),
        ..WakeOutcome::default()
    });
    assert!(matches!(
        rig.client.on_wake().await,
        Err(ClientError::Wake(_))
    ));
    assert_eq!(rig.platform.pending_wake(), None);
    rig.fakes.set_failing(Some(CrateName::Meetups));
    assert!(matches!(
        rig.client.leave_meetup(MEETUP).await,
        Err(ClientError::MeetupSession(_))
    ));
    assert!(Arc::ptr_eq(&before, &rig.client.snapshot()));
    Ok(())
}

#[tokio::test]
async fn subscribers_see_each_published_snapshot() -> Result<(), TestError> {
    let rig = rig().await?;
    let mut receiver = rig.client.subscribe();
    assert!(!receiver.has_changed().unwrap_or(true));
    rig.fakes.set_server(ServerSection {
        fallbacks: Vec::new(),
        relay: RelayStatus::Unreachable,
        map_host: None,
        operators: None,
        push_delivery: pingers_client::snapshot::PushDelivery::Full,
    });
    let published = rig.client.publish_snapshot().await?;
    assert!(receiver.has_changed().unwrap_or(false));
    assert_eq!(receiver.borrow_and_update().relay, RelayStatus::Unreachable);
    assert!(Arc::ptr_eq(&published, &rig.client.snapshot()));
    Ok(())
}

/// What the live Meetups raise on their own is published before the shell hears of it, and
/// events that arrive together are passed on together, once each.
#[tokio::test]
async fn meetup_events_republish_the_snapshot_then_tell_the_shell_once_each()
-> Result<(), TestError> {
    let rig = rig().await?;
    let meetup = GroupId::from_bytes([7; 32]);
    rig.fakes.set_meetups(MeetupsSection {
        alert: Some(meetup),
        ..MeetupsSection::default()
    });
    let reads = rig.fakes.section_reads();
    let moved = ClientEvent::PeerLocation {
        meetup,
        peer: LeafIndex::new(1),
    };
    rig.fakes.raise(ClientEvent::MeetupMessage(meetup));
    rig.fakes.raise(moved);
    rig.fakes.raise(ClientEvent::MeetupMessage(meetup));
    let mut ticks = tokio::time::interval(Duration::from_millis(10));
    let heard = tokio::time::timeout(Duration::from_secs(10), async {
        while rig.events.events().len() < 2 {
            ticks.tick().await;
        }
    })
    .await;
    assert!(heard.is_ok());
    assert_eq!(
        rig.events.events(),
        vec![
            RecordedEvent::MeetupMessage { meetup },
            RecordedEvent::PeerLocation {
                meetup,
                peer: LeafIndex::new(1)
            },
        ]
    );
    // One snapshot for the batch, published before the events.
    assert_eq!(rig.fakes.section_reads(), reads.saturating_add(4));
    assert_eq!(rig.client.snapshot().alert, Some(meetup));
    Ok(())
}

#[tokio::test]
async fn a_push_priority_report_raises_the_notice_pingers_sync_returns() -> Result<(), TestError> {
    use pingers_wake::PushPriority;
    let rig = rig().await?;
    rig.client
        .report_push_priority(PushPriority::High, PushPriority::Normal)
        .await?;
    assert!(rig.events.events().is_empty());
    rig.fakes.set_priority_notice(Some(Notice::PushDowngraded));
    rig.client
        .report_push_priority(PushPriority::High, PushPriority::Normal)
        .await?;
    assert_eq!(
        rig.events.events(),
        [RecordedEvent::Notice(Notice::PushDowngraded)]
    );
    assert!(rig.fakes.calls().contains(&CrateCall::ReportPushPriority(
        PushPriority::High,
        PushPriority::Normal
    )));
    Ok(())
}

#[tokio::test]
async fn map_calls_reach_the_maps_port_and_pass_its_errors_on() -> Result<(), TestError> {
    use pingers_client::ports::{MapHosts, MapRoute};

    let rig = rig().await?;
    let hosts = MapHosts {
        user_override: None,
        publisher: None,
    };
    rig.client
        .map_catalog(hosts.clone(), MapRoute::Direct)
        .await?;
    assert_eq!(
        rig.fakes.calls().last(),
        Some(&CrateCall::MapCatalog(hosts, MapRoute::Direct))
    );
    rig.client.forget_map_host_key().await?;
    assert_eq!(rig.fakes.calls().last(), Some(&CrateCall::ForgetMapHostKey));
    let missing = std::env::temp_dir().join("pingers-client-no-maps-here");
    assert!(rig.client.installed_maps(missing.clone()).await?.is_empty());
    rig.client
        .delete_map(missing, "oceania/none".to_owned())
        .await?;
    rig.fakes.set_failing(Some(CrateName::Maps));
    assert!(matches!(
        rig.client.forget_map_host_key().await,
        Err(ClientError::Maps(_))
    ));
    Ok(())
}
