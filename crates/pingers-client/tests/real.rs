// SPDX-License-Identifier: AGPL-3.0-only
//! The client on the real crates (`Crates::real`) over a real store in a temporary directory,
//! with a manual clock and a seeded RNG. pingers-sync has not merged, so these stop where the
//! server would be: the wake reaches the `ServerPort` stub and ends its network part there.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pingers_client::ports::{PartyDraft, SettingChange, SpotDraft};
use pingers_client::snapshot::{
    InviteStatus, Me, Name, PartyColour, PostQuantum, RelayStatus, SnapshotError, WakePushes,
};
use pingers_client::{
    Client, ClientConfig, ClientError, Crates, Instance, KeyPins, Outside, Trust, Unavailable,
};
use pingers_crypto::{AccountSigningKey, MailboxReadKey, account_id};
use pingers_gate::{GateDecision, LocationSharing, SuppressReason};
use pingers_geo::{Circle, Coordinate, Fence, Fix, GeoError, Meters};
use pingers_proto::{KeyConfigHash, ProtoError, ServerId};
use pingers_store::{
    Database, DatabaseKey, Identity, Mailbox, MailboxPurpose, MailboxState, StoreError,
};
use pingers_testkit::{FakePlatform, RecordingEvents, test_clock};
use pingers_time::{Clock as _, ManualClock, UnixSeconds};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error(transparent)]
    Store(#[from] StoreError),
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

const KEY: [u8; 32] = [9; 32];

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pingers-client-real-{}-{unique}",
            std::process::id()
        ));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn database(&self) -> PathBuf {
        self.0.join("pingers.db")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

struct Rig {
    client: Client,
    platform: FakePlatform,
    clock: ManualClock,
    _dir: TestDir,
}

/// A store with an account and one pooled invite mailbox (pingers-sync would register both),
/// then the client opened on the real crates over it.
/// The built-in server: pinned, behind a relay on a reserved name that never resolves, so
/// every request waits as it would without a network.
fn unreachable() -> Result<Instance, TestError> {
    Ok(Instance {
        server: ServerId::new("https://pingers.test".to_owned())?,
        relay: "https://relay.invalid".to_owned(),
        push_relay: None,
        gateway_pins: Some(KeyPins::from(KeyConfigHash::from_bytes([3; 32]))),
        push_relay_pins: None,
        trust: Trust::Mozilla,
        registration_code: None,
    })
}

async fn rig() -> Result<Rig, TestError> {
    let dir = TestDir::new()?;
    let clock = test_clock();
    let mut rng = ChaCha20Rng::seed_from_u64(1);
    {
        let database = Database::open(
            &dir.database(),
            DatabaseKey::from_bytes(Zeroizing::new(KEY)),
        )?;
        let session = database.session()?;
        let signing_key = AccountSigningKey::generate(&mut rng);
        session.identity().save(&Identity {
            account: account_id(&signing_key.public_key()),
            signing_key,
        })?;
        session.servers().put(&unreachable()?.record())?;
        session
            .server(&unreachable()?.server)?
            .mailboxes()
            .insert(&Mailbox {
                read_key: MailboxReadKey::generate(&mut rng),
                purpose: MailboxPurpose::Invite,
                state: MailboxState::Pooled,
                push: None,
                expires_at: UnixSeconds::new(clock.now().get().saturating_add(7 * 86_400)),
            })?;
    }
    let platform = FakePlatform::new(clock.clone());
    platform
        .secure_store_put("pingers.database-key", &KEY)
        .map_err(|_locked| TestError::SecureStore)?;
    let seeds = Arc::new(AtomicU64::new(100));
    let outside = Outside {
        clock: Arc::new(clock.clone()),
        timer: Arc::new(clock.clone()),
        rng: Arc::new(move || {
            Box::new(ChaCha20Rng::seed_from_u64(
                seeds.fetch_add(1, Ordering::Relaxed),
            ))
        }),
    };
    let crates_platform: Arc<dyn pingers_client::Platform> = Arc::new(platform.clone());
    let client = Client::open_with(
        ClientConfig {
            database: dir.database(),
            instance: Some(unreachable()?),
        },
        Arc::new(platform.clone()),
        Arc::new(RecordingEvents::new()),
        move |database| {
            Crates::real(
                database,
                crates_platform,
                outside,
                Some(&unreachable().map_err(|_bad| ClientError::Internal)?),
            )
        },
    )
    .await?;
    Ok(Rig {
        client,
        platform,
        clock,
        _dir: dir,
    })
}

fn centre() -> Result<Coordinate, GeoError> {
    Coordinate::new(51.5007, -0.1246)
}

fn spot_fence() -> Result<Fence, GeoError> {
    Ok(Fence::Circle(Circle::new(centre()?, Meters::new(150.0)?)?))
}

async fn party_with_spot(client: &Client) -> Result<(), TestError> {
    let party = client
        .create_party(PartyDraft {
            server: None,
            name: Name::new("Climbing crew")?,
            my_name: Name::new("Sam")?,
            colour: PartyColour::Teal,
            post_quantum: PostQuantum::Off,
        })
        .await?;
    client
        .save_spot(
            party,
            SpotDraft {
                id: None,
                name: Name::new("The wall")?,
                fence: spot_fence()?,
            },
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_party_its_spot_and_an_invite_show_in_the_snapshot() -> Result<(), TestError> {
    let rig = rig().await?;
    assert!(
        rig.client.snapshot().parties.is_empty(),
        "a fresh store has no Parties"
    );

    let party = rig
        .client
        .create_party(PartyDraft {
            server: None,
            name: Name::new("Climbing crew")?,
            my_name: Name::new("Sam")?,
            colour: PartyColour::Teal,
            post_quantum: PostQuantum::Off,
        })
        .await?;
    let spot = rig
        .client
        .save_spot(
            party,
            SpotDraft {
                id: None,
                name: Name::new("The wall")?,
                fence: spot_fence()?,
            },
        )
        .await?;
    let link = rig.client.create_invite(party).await?;

    let snapshot = rig.client.snapshot();
    let shown = snapshot.party(party)?;
    assert_eq!(shown.name.as_str(), "Climbing crew");
    assert_eq!(shown.colour, PartyColour::Teal);
    assert_eq!(shown.members.len(), 1, "only this device");
    assert!(shown.members.iter().all(|member| member.me == Me::Me));
    assert_eq!(
        shown.members.first().map(|member| member.name.as_str()),
        Some("Sam")
    );
    assert_eq!(shown.spots.len(), 1);
    assert_eq!(shown.spots.first().map(|shown| shown.id), Some(spot));
    assert_eq!(
        shown.spots.first().map(|shown| shown.name.as_str()),
        Some("The wall")
    );
    assert_eq!(
        shown.spots.first().map(|shown| &shown.fence),
        Some(&spot_fence()?)
    );
    assert_eq!(shown.config.post_quantum, PostQuantum::Off);
    assert_eq!(shown.config.server.host.as_str(), "pingers.test");
    assert_eq!(shown.invite_link.as_ref(), Some(&link));
    assert!(
        link.url.as_str().starts_with("https://"),
        "a shareable link"
    );
    assert_eq!(shown.invites.len(), 1);
    assert_eq!(
        shown.invites.first().map(|invite| &invite.status),
        Some(&InviteStatus::Pending {
            shared_at: rig.clock.now(),
            expires_at: link.expires_at,
        })
    );

    // Nothing reaches the server yet.
    assert_eq!(snapshot.relay, RelayStatus::Unreachable);
    assert!(matches!(
        rig.client.set_wake_pushes(WakePushes::On).await,
        Err(ClientError::Unavailable(Unavailable::NoPushToken))
    ));
    rig.platform
        .set_push_registration(Some(pingers_testkit::PushRegistration {
            platform: pingers_proto::PushPlatform::UnifiedPush,
            token: pingers_proto::PushToken::from_bytes(b"endpoint".to_vec()),
        }));
    // A token, but no instance and so no push relay to seal it to.
    assert!(matches!(
        rig.client.set_wake_pushes(WakePushes::On).await,
        Err(ClientError::Unavailable(Unavailable::NoPushRelay))
    ));
    rig.client.set_wake_pushes(WakePushes::NoWakePushes).await?;
    assert_eq!(
        rig.client.snapshot().settings.wake_pushes,
        WakePushes::NoWakePushes
    );
    Ok(())
}

#[tokio::test]
async fn deleting_the_spot_and_leaving_the_party_empty_the_snapshot() -> Result<(), TestError> {
    let rig = rig().await?;
    party_with_spot(&rig.client).await?;
    let snapshot = rig.client.snapshot();
    let party = snapshot.parties.first().map(|party| party.id);
    let spot = snapshot
        .parties
        .first()
        .and_then(|party| party.spots.first())
        .map(|spot| spot.id);
    assert!(party.is_some() && spot.is_some(), "the Party and its Spot");
    let (Some(party), Some(spot)) = (party, spot) else {
        return Ok(());
    };
    rig.client.delete_spot(party, spot).await?;
    assert!(rig.client.snapshot().party(party)?.spots.is_empty());
    rig.client.leave_party(party).await?;
    assert!(rig.client.snapshot().parties.is_empty());
    Ok(())
}

#[tokio::test]
async fn settings_are_kept_in_the_store_and_reach_the_gate() -> Result<(), TestError> {
    let rig = rig().await?;
    assert_eq!(rig.client.snapshot().gate, GateDecision::Share);
    rig.client
        .change_setting(SettingChange::LocationSharing(LocationSharing::Off))
        .await?;
    let snapshot = rig.client.snapshot();
    assert_eq!(snapshot.settings.location_sharing, LocationSharing::Off);
    assert_eq!(
        snapshot.gate,
        GateDecision::Suppress(SuppressReason::LocationSharingOff)
    );
    Ok(())
}

#[tokio::test]
async fn a_wake_inside_a_spot_runs_to_the_server_stub_and_stops_cleanly() -> Result<(), TestError> {
    let rig = rig().await?;
    party_with_spot(&rig.client).await?;
    let fix = |at: UnixSeconds| -> Result<Fix, GeoError> {
        Ok(Fix {
            coordinate: centre()?,
            accuracy: Meters::new(10.0)?,
            taken_at: at,
        })
    };

    // The first fix makes the Spot a candidate; no upload is due, so no request is made.
    rig.client.on_location(fix(rig.clock.now())?).await?;
    assert_eq!(rig.client.snapshot().gate, GateDecision::Share);
    assert!(
        rig.platform
            .location_mode_history()
            .iter()
            .all(|mode| *mode != pingers_testkit::LocationMode::Dwell),
        "not inside yet"
    );

    // A second fix 31 s later confirms it: Inside, so an upload is due, and the wake pulls the
    // group logs first, which pingers-sync would do. The stub answers Unavailable; the wake
    // still sets the mode and schedules the retry, and the call succeeds.
    rig.clock.advance(Duration::from_secs(31));
    rig.client.on_location(fix(rig.clock.now())?).await?;
    assert_eq!(
        rig.platform.location_mode(),
        pingers_testkit::LocationMode::Dwell
    );
    assert_eq!(
        rig.platform.pending_wake().map(|wake| wake.after),
        Some(pingers_wake::RETRY_AFTER),
        "a port failure retries after a minute"
    );
    assert!(rig.client.snapshot().own_position.is_some());
    Ok(())
}
