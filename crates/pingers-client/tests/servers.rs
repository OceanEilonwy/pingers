// SPDX-License-Identifier: AGPL-3.0-only
//! Parties on two servers at once (decided 2026-11-14), against two in-process harnesses that
//! share a clock: server A is both devices' built-in server; server B is another, with its own
//! relay, keys and registration code.
//!
//! - Ana looks at B by address, trusts its keys, needs and then gives B's code, and creates a
//!   Party on each server; her invites carry each server's own pins.
//! - Ben, who has never used B, opens Ana's invite to the Party on B: he records B with the
//!   pins from the invite (never the first key he is shown), registers there with the code
//!   the link carries, and his join request lands on B.
//! - An invite whose pins B's keys do not match is refused with the relay key warning, and
//!   nothing is recorded.
//! - With B gone, a wake still uploads Ana's lease to A.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pingers_client::ports::PartyDraft;
use pingers_client::snapshot::{
    Host, InviteUrl, KeyTrust, Name, PartyColour, PostQuantum, PushRelayKind,
};
use pingers_client::{
    Client, ClientConfig, ClientError, Crates, Instance, KeyPins, Outside, ServerProblem, Trust,
};
use pingers_proto::{KeyConfigHash, PartyId, SecretText, ServerId};
use pingers_store::{Database, DatabaseKey, ServerTrust};
use pingers_testkit::{FakePlatform, HarnessConfig, InProcessHarness, NoticeKind, RecordingEvents};
use pingers_time::ManualClock;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error(transparent)]
    Harness(#[from] pingers_testkit::HarnessError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error(transparent)]
    Parties(#[from] pingers_parties::PartiesError),
    #[error(transparent)]
    Store(#[from] pingers_store::StoreError),
    #[error(transparent)]
    Ohttp(#[from] pingers_ohttp::OhttpError),
    #[error(transparent)]
    Snapshot(#[from] pingers_client::snapshot::SnapshotError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Events(#[from] pingers_testkit::EventAssertion),
    #[error("setup: {0}")]
    Setup(&'static str),
}

type TestResult = Result<(), TestError>;

const KEY: [u8; 32] = [8; 32];
const B_CODE: &str = "b-friends";
const STEP: Duration = Duration::from_secs(90);

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "pingers-client-servers-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// Server A and server B on one clock.
struct World {
    clock: ManualClock,
    a: InProcessHarness,
    b: Option<InProcessHarness>,
    /// Both harnesses' test CAs: a device built against A's test stack trusts both, as a
    /// debug build's `PINGERS_EXTRA_CA` bundle would.
    trust: String,
}

impl World {
    fn start() -> Result<Self, TestError> {
        let a_config = HarnessConfig {
            seed: 1,
            operator: Some("Server A".to_owned()),
            ..HarnessConfig::default()
        };
        let a = InProcessHarness::start(&a_config)?;
        let b = InProcessHarness::start(&HarnessConfig {
            seed: 2,
            clock: a_config.clock.clone(),
            operator: Some("Server B".to_owned()),
            registration_codes: vec![B_CODE.to_owned()],
            ..HarnessConfig::default()
        })?;
        let trust = format!(
            "{}\n{}",
            a.endpoints().push_relay_ca,
            b.endpoints().push_relay_ca
        );
        Ok(Self {
            clock: a_config.clock,
            a,
            b: Some(b),
            trust,
        })
    }

    fn b(&self) -> Result<&InProcessHarness, TestError> {
        self.b.as_ref().ok_or(TestError::Setup("server B is down"))
    }

    fn b_server(&self) -> Result<ServerId, TestError> {
        Ok(ServerId::new(self.b()?.endpoints().server_id.clone())?)
    }

    /// The built-in server A, as a build pinned to it carries it.
    fn instance(&self) -> Result<Instance, TestError> {
        let endpoints = self.a.endpoints();
        Ok(Instance {
            server: ServerId::new(endpoints.server_id.clone())?,
            relay: format!("http://{}/ohttp", endpoints.ohttp_relay),
            push_relay: Some(format!("https://{}", endpoints.push_relay)),
            gateway_pins: Some(KeyPins::from(pingers_crypto::key_config_hash(
                &endpoints.ohttp_key_config,
            ))),
            push_relay_pins: None,
            trust: Trust::from_pem(&self.trust)?,
            registration_code: None,
        })
    }
}

/// One device.
struct Device {
    client: Client,
    events: Arc<RecordingEvents>,
    platform: FakePlatform,
    database: PathBuf,
    _dir: TestDir,
}

impl Device {
    async fn open(world: &World, name: &str, seed: u64) -> Result<Self, TestError> {
        let dir = TestDir::new(name)?;
        let database = dir.0.join("pingers.db");
        let platform = FakePlatform::new(world.clock.clone());
        platform
            .secure_store_put("pingers.database-key", &KEY)
            .map_err(|_locked| TestError::Setup("secure store"))?;
        let seeds = Arc::new(AtomicU64::new(seed));
        let outside = Outside {
            clock: Arc::new(world.clock.clone()),
            timer: Arc::new(world.clock.clone()),
            rng: Arc::new(move || {
                Box::new(ChaCha20Rng::seed_from_u64(
                    seeds.fetch_add(1, Ordering::Relaxed),
                ))
            }),
        };
        let instance = world.instance()?;
        let opened = instance.clone();
        let events = Arc::new(RecordingEvents::new());
        let crates_platform: Arc<dyn pingers_client::Platform> = Arc::new(platform.clone());
        let client = Client::open_with(
            ClientConfig {
                database: database.clone(),
                instance: Some(instance),
            },
            Arc::new(platform.clone()),
            {
                let shared: Arc<RecordingEvents> = Arc::clone(&events);
                let events: Arc<dyn pingers_client::Events> = shared;
                events
            },
            move |database| Crates::real(database, crates_platform, outside, Some(&opened)),
        )
        .await?;
        Ok(Self {
            client,
            events,
            platform,
            database,
            _dir: dir,
        })
    }

    /// The servers this device has recorded, read from its store (the client holds the
    /// store's lock only per call).
    fn servers(&self) -> Result<Vec<pingers_store::ServerRecord>, TestError> {
        let database =
            Database::open(&self.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
        let session = database.session()?;
        Ok(session.servers().list()?)
    }

    /// Whether this device's last upload to `server`'s board is recorded.
    fn uploaded_to(&self, server: &ServerId) -> Result<bool, TestError> {
        let database =
            Database::open(&self.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
        let session = database.session()?;
        Ok(session.server(server)?.board_upload().last()?.is_some())
    }

    async fn create_party(
        &self,
        server: Option<ServerId>,
        name: &str,
    ) -> Result<PartyId, ClientError> {
        let draft = PartyDraft {
            server,
            name: Name::new(name).map_err(|_bad| ClientError::Internal)?,
            my_name: Name::new("Ana").map_err(|_bad| ClientError::Internal)?,
            colour: PartyColour::Blue,
            post_quantum: PostQuantum::Off,
        };
        tokio::time::timeout(STEP, self.client.create_party(draft))
            .await
            .map_err(|_elapsed| ClientError::Internal)?
    }
}

/// The hash of a harness push relay's key config.
fn push_relay_hash(harness: &InProcessHarness) -> Result<KeyConfigHash, TestError> {
    let public_key: [u8; 32] = harness
        .endpoints()
        .push_relay_key
        .as_slice()
        .try_into()
        .map_err(|_length| TestError::Setup("push relay key length"))?;
    Ok(pingers_crypto::key_config_hash(
        &pingers_proto::PushRelayKeyConfig {
            key_id: pingers_proto::PushRelayKeyId::new(1),
            public_key,
        }
        .to_bytes(),
    ))
}

fn payload(url: &InviteUrl) -> Result<pingers_proto::InvitePayload, TestError> {
    Ok(pingers_parties::InviteLink::from_text(url.as_str().to_owned()).payload()?)
}

/// Ana: a Party on A, B looked at and trusted, B's code given, a Party on B.
async fn ana_on_both(world: &World) -> Result<(Device, PartyId, PartyId), TestError> {
    let ana = Device::open(world, "ana", 100).await?;
    let on_a = ana.create_party(None, "Climbing").await?;

    // B by address: its operator, relay and keys, nothing recorded yet.
    let b = world.b_server()?;
    let probe = ana.client.probe_server(b.as_str().to_owned()).await?;
    assert_eq!(probe.server, b);
    assert!(!probe.known);
    assert_eq!(
        probe
            .operator
            .as_ref()
            .map(pingers_client::snapshot::Operator::as_str),
        Some("Server B")
    );
    assert_eq!(
        probe.gateway_key,
        pingers_crypto::key_config_hash(&world.b()?.endpoints().ohttp_key_config)
    );
    assert!(probe.push_relay.is_some(), "B lists its push relay");
    assert_eq!(ana.servers()?.len(), 1, "looking records nothing");
    // A Party cannot go on B before its keys are trusted.
    assert!(matches!(
        ana.create_party(Some(b.clone()), "Book club").await,
        Err(ClientError::Unavailable(_))
    ));
    ana.client.trust_server(b.clone()).await?;

    // B takes only devices with its code: missing, then refused, are told apart.
    assert!(matches!(
        ana.create_party(Some(b.clone()), "Book club").await,
        Err(ClientError::RegistrationCodeRequired)
    ));
    assert!(matches!(
        ana.client
            .submit_registration_code(b.clone(), SecretText::new("b-fiends".to_owned()))
            .await,
        Err(ClientError::RegistrationCodeRefused)
    ));
    ana.client
        .submit_registration_code(b.clone(), SecretText::new(B_CODE.to_owned()))
        .await?;
    let on_b = ana.create_party(Some(b.clone()), "Book club").await?;
    // Looked at again, B is one this device already trusts.
    assert!(ana.client.probe_server(b.as_str().to_owned()).await?.known);
    Ok((ana, on_a, on_b))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_device_holds_parties_on_two_servers_with_their_own_pins() -> TestResult {
    let world = World::start()?;
    let (ana, on_a, on_b) = ana_on_both(&world).await?;
    let a = ServerId::new(world.a.endpoints().server_id.clone())?;
    let b = world.b_server()?;

    // Each Party shows its own server, and how its keys were trusted.
    let snapshot = ana.client.snapshot();
    let config = |party| snapshot.party(party).map(|shown| shown.config.clone());
    assert_eq!(config(on_a)?.origin, a);
    assert_eq!(config(on_a)?.keys, KeyTrust::BuiltIn);
    assert_eq!(config(on_b)?.origin, b);
    assert_eq!(config(on_b)?.keys, KeyTrust::Confirmed);
    assert_eq!(config(on_b)?.server.operator.as_str(), "Server B");
    // A uses the build's push relay; B its own, shown by its host.
    assert_eq!(config(on_a)?.push_relay.kind, PushRelayKind::Official);
    assert_eq!(
        config(on_b)?.push_relay.kind,
        PushRelayKind::Custom(Host::new(&world.b()?.endpoints().push_relay.to_string())?)
    );

    // Each invite carries its own server's pins, and the code registered there.
    let link_a = ana.client.create_invite(on_a).await?;
    let link_b = ana.client.create_invite(on_b).await?;
    let (invite_a, invite_b) = (payload(&link_a.url)?, payload(&link_b.url)?);
    assert_eq!(invite_a.server, a);
    assert_eq!(
        invite_a.gateway_pins,
        [pingers_crypto::key_config_hash(
            &world.a.endpoints().ohttp_key_config
        )]
    );
    assert!(invite_a.registration_code.is_none());
    assert_eq!(invite_b.server, b);
    assert_eq!(
        invite_b.gateway_pins,
        [pingers_crypto::key_config_hash(
            &world.b()?.endpoints().ohttp_key_config
        )]
    );
    assert_eq!(
        invite_b.registration_code.as_ref().map(SecretText::expose),
        Some(B_CODE)
    );
    // B's own push relay (not the build's): its key, as Ana trusted it, goes in B's invites.
    assert_eq!(invite_b.push_relay_pins, [push_relay_hash(world.b()?)?]);

    // Cancelling B's invite deletes its mailbox on B, which then forgets it here too.
    ana.client
        .cancel_invite(on_b, invite_b.invite_mailbox)
        .await?;
    let database = Database::open(&ana.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    let session = database.session()?;
    assert!(
        session
            .server(&b)?
            .mailboxes()
            .get(&invite_b.invite_mailbox)?
            .is_none(),
        "the invite mailbox was deleted on B"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_joiner_new_to_a_server_registers_there_with_the_invite_s_pins() -> TestResult {
    use pingers_client::ports::{Foreground, JoinDecision};
    use pingers_client::snapshot::{Joining, WakePushes};

    let world = World::start()?;
    let (ana, _on_a, on_b) = ana_on_both(&world).await?;
    let link = ana.client.create_invite(on_b).await?;
    let invited = payload(&link.url)?;

    let ben = Device::open(&world, "ben", 500).await?;
    let b = world.b_server()?;
    // Ben has wake-up pushes off: the join request goes without a subscription.
    ben.client.set_wake_pushes(WakePushes::NoWakePushes).await?;
    tokio::time::timeout(
        STEP,
        ben.client.open_invite(link.url.clone(), Name::new("Ben")?),
    )
    .await
    .map_err(|_elapsed| TestError::Setup("opening the invite did not finish"))??;
    assert!(matches!(
        ben.client.snapshot().joining,
        Some(Joining::Asked { .. })
    ));

    // Ana reads her invite on B, lets Ben in, and Ben's next wake finds the Welcome on B.
    ana.client.on_foreground(Foreground::Visible).await?;
    let request = ana
        .client
        .snapshot()
        .party(on_b)?
        .join_requests
        .first()
        .map(|request| request.id)
        .ok_or(TestError::Setup(
            "Ben's request did not reach Ana through B",
        ))?;
    ana.client
        .decide_join(on_b, request, JoinDecision::Admit)
        .await?;
    ben.client.on_wake().await?;
    let snapshot = ben.client.snapshot();
    assert!(
        matches!(snapshot.joining, Some(Joining::LetIn { party, .. }) if party == on_b),
        "{:?}",
        snapshot.joining
    );
    let joined = snapshot.party(on_b)?;
    assert_eq!(joined.config.origin, b, "the Party Ben joined is on B");
    assert_eq!(joined.config.keys, KeyTrust::FromInvite);

    // B was recorded with the invite's pins, as an invited server, and Ben registered there
    // with the link's code.
    let recorded = ben
        .servers()?
        .into_iter()
        .find(|record| record.server == b)
        .ok_or(TestError::Setup("B not recorded"))?;
    assert_eq!(recorded.trust, ServerTrust::Invite);
    assert_eq!(recorded.gateway_pins, invited.gateway_pins);
    let database = Database::open(&ben.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    let session = database.session()?;
    assert!(matches!(
        session.server(&b)?.account().registration()?,
        pingers_store::AccountRegistration::Registered { code: Some(code) } if code.expose() == B_CODE
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invite_whose_pins_the_server_s_keys_do_not_match_is_refused() -> TestResult {
    let world = World::start()?;
    let (ana, _on_a, on_b) = ana_on_both(&world).await?;
    let link = ana.client.create_invite(on_b).await?;
    // Someone swapped the pins: B's real key is not among them.
    let mut forged = payload(&link.url)?;
    forged.gateway_pins = vec![KeyConfigHash::from_bytes([0x5a; 32])];
    let forged = pingers_parties::InviteLink::new(&forged)?;

    let ben = Device::open(&world, "ben", 700).await?;
    let opened = ben
        .client
        .open_invite(InviteUrl::new(forged.as_str())?, Name::new("Ben")?)
        .await;
    assert!(
        matches!(opened, Err(ClientError::Server(ServerProblem::KeyMismatch))),
        "{opened:?}"
    );
    // Nothing about B was recorded, and the user is warned as for any relay key mismatch.
    let b = world.b_server()?;
    assert!(ben.servers()?.iter().all(|record| record.server != b));
    ben.client.on_wake().await?;
    ben.events.expect_notice(NoticeKind::RelayKeyMismatch)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_is_gone_does_not_stop_the_other_server_s_upload() -> TestResult {
    let mut world = World::start()?;
    let (ana, _on_a, _on_b) = ana_on_both(&world).await?;
    let a = ServerId::new(world.a.endpoints().server_id.clone())?;
    let b = world.b_server()?;
    assert_eq!(
        ana.client.check_reachability().await?,
        pingers_client::snapshot::RelayStatus::Reachable
    );
    if let Some(gone) = world.b.take() {
        gone.shutdown();
    }
    // The check sees B's relay gone, and B's Party says so; A still answers.
    assert_eq!(
        ana.client.check_reachability().await?,
        pingers_client::snapshot::RelayStatus::Reachable
    );
    let snapshot = ana.client.snapshot();
    let on_b_status = snapshot
        .parties
        .iter()
        .find(|party| party.config.origin == b)
        .map(|party| party.config.relay_status);
    assert_eq!(
        on_b_status,
        Some(pingers_client::snapshot::RelayStatus::Unreachable)
    );
    tokio::time::timeout(STEP, ana.client.on_wake())
        .await
        .map_err(|_elapsed| TestError::Setup("the wake did not finish"))??;
    assert!(ana.uploaded_to(&a)?, "A's board has Ana's lease");
    assert!(!ana.uploaded_to(&b)?, "B is gone");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wake_restocks_each_server_s_tokens() -> TestResult {
    let world = World::start()?;
    let (ana, _on_a, _on_b) = ana_on_both(&world).await?;
    let servers = [
        ServerId::new(world.a.endpoints().server_id.clone())?,
        world.b_server()?,
    ];
    let tokens = |server: &ServerId| -> Result<u32, TestError> {
        let database = Database::open(&ana.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
        let session = database.session()?;
        Ok(session.server(server)?.privacy_pass_tokens().count()?)
    };
    {
        let database = Database::open(&ana.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
        let session = database.session()?;
        for server in &servers {
            let _spent = session
                .server(server)?
                .privacy_pass_tokens()
                .retain_keys(&[])?;
        }
    }
    for server in &servers {
        assert_eq!(tokens(server)?, 0);
    }
    // The next day, when each server issues a fresh allowance of tokens.
    world.clock.advance(std::time::Duration::from_hours(25));
    tokio::time::timeout(STEP, ana.client.on_wake())
        .await
        .map_err(|_elapsed| TestError::Setup("the wake did not finish"))??;
    for server in &servers {
        let held = tokens(server)?;
        assert!(held > 0, "{server:?}: {held} tokens after the wake");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_record_on_server_b_takes_a_knock_mailbox_of_server_b() -> TestResult {
    use pingers_geo::{Circle, Coordinate, Fence, Fix, Meters};
    let world = World::start()?;
    let (ana, _on_a, on_b) = ana_on_both(&world).await?;
    let b = world.b_server()?;
    let centre =
        Coordinate::new(51.5007, -0.1246).map_err(|_bad| TestError::Setup("coordinate"))?;
    let fence = Fence::Circle(
        Circle::new(
            centre,
            Meters::new(150.0).map_err(|_bad| TestError::Setup("radius"))?,
        )
        .map_err(|_bad| TestError::Setup("circle"))?,
    );
    ana.client
        .save_spot(
            on_b,
            pingers_client::ports::SpotDraft {
                id: None,
                name: Name::new("The wall")?,
                fence,
            },
        )
        .await?;
    let pooled = |server: &ServerId| -> Result<u32, TestError> {
        let database = Database::open(&ana.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
        let session = database.session()?;
        Ok(session.server(server)?.mailboxes().pooled_count(
            pingers_store::MailboxPurpose::Knock,
            pingers_time::Clock::now(&world.clock),
        )?)
    };
    // A wake fills each server's knock pool.
    tokio::time::timeout(STEP, ana.client.on_wake())
        .await
        .map_err(|_elapsed| TestError::Setup("the wake did not finish"))??;
    let before = pooled(&b)?;
    assert!(before > 0, "B's knock pool has a mailbox");
    // Two fixes 31 s apart confirm Inside: the lease uploaded to B carries a real record,
    // whose knock mailbox comes out of B's pool.
    let fix = |clock: &ManualClock| Fix {
        coordinate: centre,
        accuracy: Meters::ZERO,
        taken_at: pingers_time::Clock::now(clock),
    };
    ana.client.on_location(fix(&world.clock)).await?;
    world.clock.advance(Duration::from_secs(31));
    tokio::time::timeout(STEP, ana.client.on_location(fix(&world.clock)))
        .await
        .map_err(|_elapsed| TestError::Setup("the wake did not finish"))??;
    // Each round registers one knock mailbox for the pool; the record took one, so the pool
    // is the same size as before rather than one larger.
    assert_eq!(
        pooled(&b)?,
        before,
        "a knock mailbox of B went into the lease"
    );
    Ok(())
}

/// The requests a harness server answered through its relay, by its own metrics.
async fn relayed_answers(harness: &InProcessHarness) -> Result<u64, TestError> {
    let answer = pingers_testkit::send_direct(
        harness.endpoints().server,
        &pingers_ohttp::BinaryRequest {
            method: "GET".to_owned(),
            path: "/metrics".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        },
    )
    .await?;
    let text = String::from_utf8_lossy(&answer.body).into_owned();
    Ok(text
        .lines()
        .filter(|line| line.starts_with("pingers_requests_total{channel=\"ohttp\",outcome=\"ok\"}"))
        .filter_map(|line| line.rsplit(' ').next()?.parse::<u64>().ok())
        .sum())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaving_the_party_on_b_posts_the_leave_to_b_once() -> TestResult {
    let world = World::start()?;
    let (ana, on_a, on_b) = ana_on_both(&world).await?;
    let before = relayed_answers(world.b()?).await?;
    ana.client.leave_party(on_b).await?;
    // A command on A flushes every server's waiting requests: a leave already sent is not
    // sent again.
    ana.client.set_display_name(Name::new("Ana")?).await?;
    let _invite = ana.client.create_invite(on_a).await?;
    assert_eq!(
        relayed_answers(world.b()?).await?,
        before.saturating_add(1),
        "one leave proposal reached B, through B's relay"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_pushes_go_on_for_the_servers_whose_push_relay_can_be_sealed_to() -> TestResult {
    let world = World::start()?;
    let (ana, _on_a, _on_b) = ana_on_both(&world).await?;
    ana.platform
        .set_push_registration(Some(pingers_testkit::PushRegistration {
            platform: pingers_proto::PushPlatform::UnifiedPush,
            token: pingers_proto::PushToken::from_bytes(b"https://push.example/up/ana".to_vec()),
        }));
    // This build has no pins for its push relay, so nothing is sealed for A; B's push relay
    // key was trusted with B, so B's mailboxes wake this phone. Pushes are on.
    ana.client
        .set_wake_pushes(pingers_client::snapshot::WakePushes::On)
        .await?;
    assert_eq!(
        ana.client.snapshot().settings.wake_pushes,
        pingers_client::snapshot::WakePushes::On
    );
    Ok(())
}
