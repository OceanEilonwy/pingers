// SPDX-License-Identifier: AGPL-3.0-only
//! A fresh client on the real crates against the in-process harness (pingers-server behind its
//! HTTPS front, an OHTTP relay, pingers-push-relay): it makes its account, registers it with the
//! build's registration code, fetches Privacy Pass tokens, takes a membership leaf, creates a
//! Party whose group the server then holds, and registers a mailbox, with nothing set up in its
//! store beforehand.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_client::ports::PartyDraft;
use pingers_client::snapshot::{Name, PartyColour, PostQuantum, RelayStatus, WakePushes};
use pingers_client::{
    Client, ClientConfig, ClientError, Crates, Instance, KeyPins, Outside, Trust,
};
use pingers_crypto::{PushRelayPublicKey, PushSealFingerprint, push_seal_fingerprint};
use pingers_ohttp::BinaryRequest;
use pingers_parties::PartyLifecycle;
use pingers_proto::{
    GroupMessages, InvitePayload, KeyConfigHash, PushPlatform, PushToken, SecretText, ServerId,
    WireDecode as _,
};
use pingers_store::{AccountRegistration, Database, DatabaseKey, MailboxPurpose, Session};
use pingers_sync::{ApiRequest, GroupSigner};
use pingers_testkit::{
    FakePlatform, HarnessConfig, InProcessHarness, PushRegistration, RecordingEvents, TEST_START,
};
use pingers_time::{Clock as _, ManualClock, UnixSeconds};
use pingers_transport::{
    KeyPin, OhttpConfig, OhttpTransport, PrivacyClass, Relay, TlsRoots, Transport as _,
};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error(transparent)]
    Store(#[from] pingers_store::StoreError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error(transparent)]
    Harness(#[from] pingers_testkit::HarnessError),
    #[error(transparent)]
    Parties(#[from] pingers_parties::PartiesError),
    #[error(transparent)]
    Sync(#[from] pingers_sync::SyncError),
    #[error(transparent)]
    Transport(#[from] pingers_transport::TransportError),
    #[error(transparent)]
    Snapshot(#[from] pingers_client::snapshot::SnapshotError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Crypto(#[from] pingers_crypto::CryptoError),
    #[error(transparent)]
    Records(#[from] pingers_mls::StorageError),
    #[error("setup: {0}")]
    Setup(&'static str),
}

const KEY: [u8; 32] = [6; 32];
const CODE: &str = "first-owner";
/// How long a step may take in real time; a hang fails the test instead of stalling it.
const STEP: std::time::Duration = std::time::Duration::from_secs(60);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pingers-client-server-{}-{unique}",
            std::process::id()
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

/// The instance a debug build pointed at the harness would carry: the server's HTTPS origin
/// with the harness CA as its only trust root, the relay, the push relay, the build's pins and
/// `PINGERS_DEV_REGISTRATION_CODE`.
fn instance(harness: &InProcessHarness, code: Option<&str>) -> Result<Instance, TestError> {
    let endpoints = harness.endpoints();
    Ok(Instance {
        server: ServerId::new(endpoints.server_id.clone())?,
        // As deployments configure it (PINGERS_OHTTP_RELAY): the relay URL, not its origin.
        relay: format!("http://{}/ohttp", endpoints.ohttp_relay),
        push_relay: Some(format!("https://{}", endpoints.push_relay)),
        gateway_pins: Some(KeyPins::from(pingers_crypto::key_config_hash(
            &endpoints.ohttp_key_config,
        ))),
        push_relay_pins: Some(push_relay_pins(endpoints)?),
        trust: Trust::from_pem(&endpoints.push_relay_ca)?,
        registration_code: code.map(|code| SecretText::new(code.to_owned())),
    })
}

/// The build's pins for the harness push relay: its published key and, when one is scheduled,
/// the next, as an official build pins both keys a relay announces.
fn push_relay_pins(endpoints: &pingers_testkit::Endpoints) -> Result<KeyPins, TestError> {
    let mut hashes = vec![push_relay_pin(&endpoints.push_relay_key, 1)?];
    if let Some(next) = &endpoints.push_relay_next_key {
        hashes.push(push_relay_pin(next, 2)?);
    }
    KeyPins::new(hashes).map_err(|_bad| TestError::Setup("push relay pins"))
}

/// The hash of a harness push relay key config.
fn push_relay_pin(key: &[u8], key_id: u8) -> Result<KeyConfigHash, TestError> {
    let public_key: [u8; 32] = key
        .try_into()
        .map_err(|_length| TestError::Setup("push relay key length"))?;
    Ok(pingers_crypto::key_config_hash(
        &pingers_proto::PushRelayKeyConfig {
            key_id: pingers_proto::PushRelayKeyId::new(key_id),
            public_key,
        }
        .to_bytes(),
    ))
}

async fn open(
    harness: &InProcessHarness,
    clock: &ManualClock,
    database: PathBuf,
    build_code: Option<&str>,
) -> Result<Client, TestError> {
    let platform = FakePlatform::new(clock.clone());
    let build = Build {
        code: build_code,
        platform,
    };
    open_on(harness, clock, database, build).await
}

/// The build's registration code and the platform (whose push registration a test sets).
struct Build<'code> {
    code: Option<&'code str>,
    platform: FakePlatform,
}

/// As [`open`], with `build`.
async fn open_on(
    harness: &InProcessHarness,
    clock: &ManualClock,
    database: PathBuf,
    build: Build<'_>,
) -> Result<Client, TestError> {
    let Build {
        code: build_code,
        platform,
    } = build;
    platform
        .secure_store_put("pingers.database-key", &KEY)
        .map_err(|_locked| TestError::Setup("secure store"))?;
    let seeds = Arc::new(AtomicU64::new(900));
    let outside = Outside {
        prover_self_test: pingers_sync::SelfTestPlan::new(
            core::num::NonZeroU8::MIN,
            core::time::Duration::from_millis(1_500),
        ),
        clock: Arc::new(clock.clone()),
        timer: Arc::new(clock.clone()),
        rng: Arc::new(move || {
            Box::new(ChaCha20Rng::seed_from_u64(
                seeds.fetch_add(1, Ordering::Relaxed),
            ))
        }),
    };
    let instance = instance(harness, build_code)?;
    let crates_platform: Arc<dyn pingers_client::Platform> = Arc::new(platform.clone());
    let opened = instance.clone();
    Ok(Client::open_with(
        ClientConfig {
            database,
            instance: Some(instance),
        },
        Arc::new(platform),
        Arc::new(RecordingEvents::new()),
        move |database| Crates::real(database, crates_platform, outside, Some(&opened)),
    )
    .await?)
}

/// The Party group's log, read from the server through the relay with the Party's log-auth
/// key, as any member would.
async fn group_log(
    harness: &InProcessHarness,
    clock: &ManualClock,
    session: &Session,
    party: &pingers_proto::PartyId,
) -> Result<(u16, Vec<u8>), TestError> {
    let group = session
        .parties()
        .get(party)?
        .ok_or(TestError::Setup("no Party row"))?
        .group;
    let key = PartyLifecycle::new(session, clock).log_auth_key(party)?;
    let request = ApiRequest::read_group_messages(
        &group,
        None,
        &GroupSigner {
            key: &key,
            now: clock.now(),
        },
    )?;
    let relay = harness.endpoints().ohttp_relay;
    let ohttp = OhttpTransport::new(
        OhttpConfig::new(
            vec![Relay::new(
                &format!("http://{relay}/ohttp"),
                &format!("http://{relay}/v1/ohttp/config"),
            )?],
            KeyPin::FirstUse,
            TlsRoots::from_der(&[])?,
        ),
        Arc::new(clock.clone()),
        Box::new(ChaCha20Rng::seed_from_u64(77)),
    )?;
    let answer = ohttp
        .request(
            PrivacyClass::Anonymous,
            BinaryRequest {
                method: request.method().to_owned(),
                path: request.path().to_owned(),
                headers: request.headers().to_vec(),
                body: request.body().to_vec(),
            },
        )
        .await?;
    Ok((answer.status.get(), answer.body))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_client_registers_gets_tokens_creates_a_party_and_a_mailbox()
-> Result<(), TestError> {
    let config = HarnessConfig {
        registration_codes: vec![CODE.to_owned()],
        ..HarnessConfig::default()
    };
    let clock = config.clock.clone();
    let harness = InProcessHarness::start(&config)?;
    let dir = TestDir::new()?;
    let path = dir.0.join("pingers.db");
    let client = open(&harness, &clock, path.clone(), Some(CODE)).await?;
    // New party's checks: the relay answers with the pinned gateway key, and the server's
    // parameters name who runs it and the push relay.
    assert_eq!(client.check_reachability().await?, RelayStatus::Reachable);
    let operators = client
        .snapshot()
        .operators
        .clone()
        .ok_or(TestError::Setup("no operators after the check"))?;
    assert!(!operators.server.as_str().is_empty());
    assert!(
        operators.push_relay.is_some(),
        "the server lists the instance's push relay"
    );

    let party = tokio::time::timeout(
        STEP,
        client.create_party(PartyDraft {
            server: None,
            name: Name::new("Climbing")?,
            my_name: Name::new("Ana")?,
            colour: PartyColour::Blue,
            post_quantum: PostQuantum::Off,
        }),
    )
    .await
    .map_err(|_elapsed| TestError::Setup("creating the Party did not finish"))??;

    let database = Database::open(&path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    let session = database.session()?;
    let scoped = session.server(&ServerId::new(harness.endpoints().server_id.clone())?)?;
    // The account was made, and registered with the build's code.
    assert!(session.identity().load()?.is_some(), "the account");
    assert_eq!(
        session
            .servers()
            .server_pin(&ServerId::new(harness.endpoints().server_id.clone())?)?,
        Some(pingers_crypto::key_config_hash(
            &harness.endpoints().ohttp_key_config
        ))
    );
    assert!(matches!(
        scoped.account().registration()?,
        AccountRegistration::Registered { code: Some(code) } if code.expose() == CODE
    ));
    assert!(scoped.account().bearer()?.is_some());
    assert!(scoped.rln_identity().load()?.is_some(), "membership leaf");
    // A full batch of tokens, less the one the group creation spent.
    assert_eq!(scoped.privacy_pass_tokens().count()?, 299);

    // The server holds the Party's group.
    let (status, body) = group_log(&harness, &clock, &session, &party).await?;
    assert_eq!(status, 200, "the group exists on the server");
    let _log = GroupMessages::decode(&body)?;
    // A session holds the store's cross-process lock: the client needs it back.
    drop(scoped);
    drop(session);

    // A wake registers a knock mailbox for the pool.
    tokio::time::timeout(STEP, client.on_wake())
        .await
        .map_err(|_elapsed| TestError::Setup("the wake did not finish"))??;
    let session = database.session()?;
    let scoped = session.server(&ServerId::new(harness.endpoints().server_id.clone())?)?;
    assert_eq!(
        scoped
            .mailboxes()
            .pooled_count(MailboxPurpose::Knock, clock.now())?,
        1
    );
    // The wake also uploaded its lease: one token for the board sync, one for the mailbox.
    assert!(scoped.board_upload().last()?.is_some());
    assert_eq!(scoped.privacy_pass_tokens().count()?, 297);

    // An invite this device makes passes its registration code on. The invite mailbox is put
    // in the pool by hand: refilling it is the invite flow's part.
    scoped.mailboxes().insert(&pingers_store::Mailbox {
        read_key: pingers_crypto::MailboxReadKey::from_bytes(Zeroizing::new([9; 32])),
        purpose: MailboxPurpose::Invite,
        state: pingers_store::MailboxState::Pooled,
        push: None,
        seal: None,
        expires_at: clock
            .now()
            .checked_add(std::time::Duration::from_hours(48))
            .ok_or(TestError::Setup("time"))?,
    })?;
    drop(scoped);
    drop(session);
    let link = client.create_invite(party).await?;
    let payload = pingers_parties::InviteLink::from_text(link.url.as_str().to_owned()).payload()?;
    assert_eq!(
        payload.registration_code.as_ref().map(SecretText::expose),
        Some(CODE)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invite_links_registration_code_registers_the_joiner() -> Result<(), TestError> {
    let config = HarnessConfig {
        registration_codes: vec![CODE.to_owned()],
        ..HarnessConfig::default()
    };
    let clock = config.clock.clone();
    let harness = InProcessHarness::start(&config)?;
    let dir = TestDir::new()?;
    let path = dir.0.join("pingers.db");
    // A friend's device: no code in its build.
    let client = open(&harness, &clock, path.clone(), None).await?;
    let link = pingers_parties::InviteLink::new(&InvitePayload {
        server: ServerId::new(harness.endpoints().server_id.clone())?,
        invite_mailbox: pingers_proto::MailboxId::from_bytes([3; 16]),
        invite_secret: pingers_proto::SecretBytes::new([4; 32]),
        inviter_fingerprint: pingers_proto::InviterFingerprint::from_bytes([5; 8]),
        expires: clock
            .now()
            .checked_add(std::time::Duration::from_hours(1))
            .ok_or(TestError::Setup("time"))?,
        gateway_pins: vec![pingers_crypto::key_config_hash(
            &harness.endpoints().ohttp_key_config,
        )],
        push_relay_pins: Vec::new(),
        registration_code: Some(SecretText::new(CODE.to_owned())),
    })?;
    // Opening the link registers the account with the link's code first; the join then goes
    // out without a subscription, as this device has no push token yet.
    let opened = tokio::time::timeout(
        STEP,
        client.open_invite(
            pingers_client::snapshot::InviteUrl::new(link.as_str())?,
            Name::new("Bea")?,
        ),
    )
    .await
    .map_err(|_elapsed| TestError::Setup("opening the invite did not finish"))?;
    assert!(opened.is_ok(), "{opened:?}");
    let database = Database::open(&path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    let session = database.session()?;
    let scoped = session.server(&ServerId::new(harness.endpoints().server_id.clone())?)?;
    assert!(matches!(
        scoped.account().registration()?,
        AccountRegistration::Registered { code: Some(code) } if code.expose() == CODE
    ));
    // The day's 300 tokens, less the reply mailbox's registration and the join request.
    assert_eq!(scoped.privacy_pass_tokens().count()?, 298);
    Ok(())
}

fn harness_key(bytes: Option<&Vec<u8>>, key_id: u8) -> Result<PushRelayPublicKey, TestError> {
    let public: [u8; 32] = bytes
        .ok_or(TestError::Setup("no relay key"))?
        .as_slice()
        .try_into()
        .map_err(|_| TestError::Setup("relay key length"))?;
    Ok(PushRelayPublicKey::new(key_id, public))
}

fn token(text: &str) -> PushRegistration {
    PushRegistration {
        platform: PushPlatform::UnifiedPush,
        token: PushToken::from_bytes(text.as_bytes().to_vec()),
    }
}

fn fingerprint(key: &PushRelayPublicKey, text: &str) -> Result<PushSealFingerprint, TestError> {
    Ok(push_seal_fingerprint(
        key,
        PushPlatform::UnifiedPush,
        &PushToken::from_bytes(text.as_bytes().to_vec()),
    )?)
}

/// What the Party subscription and every live mailbox were last sealed with.
fn seals(path: &std::path::Path, now: UnixSeconds) -> Result<Vec<PushSealFingerprint>, TestError> {
    let database = Database::open(path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    let session = database.session()?;
    let mut seals = Vec::new();
    for record in session.servers().list()? {
        let server = session.server(&record.server)?;
        for subscription in server.subscriptions().all()? {
            seals.push(
                subscription
                    .seal
                    .ok_or(TestError::Setup("unsealed"))?
                    .fingerprint,
            );
        }
        for mailbox in server.mailboxes().live(now)? {
            seals.push(
                mailbox
                    .seal
                    .ok_or(TestError::Setup("unsealed"))?
                    .fingerprint,
            );
        }
    }
    Ok(seals)
}

/// One maintenance step per call: the platform reports its registration again.
async fn steps(client: &Client, count: usize) -> Result<(), TestError> {
    for _ in 0..count {
        tokio::time::timeout(STEP, client.push_tokens_changed())
            .await
            .map_err(|_elapsed| TestError::Setup("the step did not finish"))??;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_token_and_a_relay_rotation_reach_every_registration() -> Result<(), TestError> {
    let rotation = TEST_START
        .checked_add(std::time::Duration::from_hours(48))
        .ok_or(TestError::Setup("time"))?;
    let config = HarnessConfig {
        registration_codes: vec![CODE.to_owned()],
        push_relay_next_key_at: Some(rotation),
        ..HarnessConfig::default()
    };
    let clock = config.clock.clone();
    let harness = InProcessHarness::start(&config)?;
    let old_key = harness_key(Some(&harness.endpoints().push_relay_key), 1)?;
    let new_key = harness_key(harness.endpoints().push_relay_next_key.as_ref(), 2)?;
    let dir = TestDir::new()?;
    let path = dir.0.join("pingers.db");
    let platform = FakePlatform::new(clock.clone());
    platform.set_push_registration(Some(token("https://up.invalid/a")));
    let build = Build {
        code: Some(CODE),
        platform: platform.clone(),
    };
    let client = open_on(&harness, &clock, path.clone(), build).await?;
    client.set_wake_pushes(WakePushes::On).await?;
    tokio::time::timeout(
        STEP,
        client.create_party(PartyDraft {
            name: Name::new("Climbing")?,
            my_name: Name::new("Ana")?,
            colour: PartyColour::Blue,
            post_quantum: PostQuantum::Off,
            server: None,
        }),
    )
    .await
    .map_err(|_elapsed| TestError::Setup("creating the Party did not finish"))??;
    // The creator's own subscription went in with the group; one more step fills the pool.
    steps(&client, 1).await?;
    let first = fingerprint(&old_key, "https://up.invalid/a")?;
    assert_eq!(seals(&path, clock.now())?, vec![first, first]);

    // A new token reaches the subscription and the pooled mailbox, one step each.
    platform.set_push_registration(Some(token("https://up.invalid/b")));
    steps(&client, 1).await?;
    let renewed = fingerprint(&old_key, "https://up.invalid/b")?;
    assert_eq!(
        seals(&path, clock.now())?
            .iter()
            .filter(|seal| **seal == renewed)
            .count(),
        1,
        "one registration per step"
    );
    steps(&client, 1).await?;
    assert_eq!(seals(&path, clock.now())?, vec![renewed, renewed]);

    // After the rotation the same token is sealed to the new key, again one step each.
    clock.advance(std::time::Duration::from_hours(49));
    steps(&client, 3).await?;
    let rotated = fingerprint(&new_key, "https://up.invalid/b")?;
    let after = seals(&path, clock.now())?;
    assert!(!after.is_empty());
    assert!(after.iter().all(|seal| *seal == rotated), "{after:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_wake_pushes_off_new_mailboxes_carry_no_push() -> Result<(), TestError> {
    let config = HarnessConfig {
        registration_codes: vec![CODE.to_owned()],
        ..HarnessConfig::default()
    };
    let clock = config.clock.clone();
    let harness = InProcessHarness::start(&config)?;
    let dir = TestDir::new()?;
    let path = dir.0.join("pingers.db");
    let platform = FakePlatform::new(clock.clone());
    platform.set_push_registration(Some(token("https://up.invalid/a")));
    let build = Build {
        code: Some(CODE),
        platform,
    };
    let client = open_on(&harness, &clock, path.clone(), build).await?;
    client.set_wake_pushes(WakePushes::On).await?;
    tokio::time::timeout(
        STEP,
        client.create_party(PartyDraft {
            name: Name::new("Climbing")?,
            my_name: Name::new("Ana")?,
            colour: PartyColour::Blue,
            post_quantum: PostQuantum::Off,
            server: None,
        }),
    )
    .await
    .map_err(|_elapsed| TestError::Setup("creating the Party did not finish"))??;
    // Turning them off makes one step: the Party's subscription goes first. The next wake's
    // step fills the empty pool, with no push.
    client.set_wake_pushes(WakePushes::NoWakePushes).await?;
    client.on_wake().await?;
    let database = Database::open(&path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    let session = database.session()?;
    let mailboxes = session
        .server(&ServerId::new(harness.endpoints().server_id.clone())?)?
        .mailboxes()
        .live(clock.now())?;
    assert!(!mailboxes.is_empty());
    assert!(mailboxes.iter().all(|mailbox| mailbox.push.is_none()));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_display_name_is_published_to_existing_parties() -> Result<(), TestError> {
    let config = HarnessConfig {
        registration_codes: vec![CODE.to_owned()],
        ..HarnessConfig::default()
    };
    let clock = config.clock.clone();
    let harness = InProcessHarness::start(&config)?;
    let dir = TestDir::new()?;
    let path = dir.0.join("pingers.db");
    let client = open(&harness, &clock, path.clone(), Some(CODE)).await?;
    let party = tokio::time::timeout(
        STEP,
        client.create_party(PartyDraft {
            name: Name::new("Climbing")?,
            my_name: Name::new("Ana")?,
            colour: PartyColour::Blue,
            post_quantum: PostQuantum::Off,
            server: None,
        }),
    )
    .await
    .map_err(|_elapsed| TestError::Setup("creating the Party did not finish"))??;

    tokio::time::timeout(STEP, client.set_display_name(Name::new("Anna")?))
        .await
        .map_err(|_elapsed| TestError::Setup("renaming did not finish"))??;

    // This device's roster shows the new name at once.
    let shown: Vec<String> = client
        .snapshot()
        .parties
        .iter()
        .filter(|shown| shown.id == party)
        .flat_map(|shown| shown.members.iter())
        .map(|member| member.name.as_str().to_owned())
        .collect();
    assert_eq!(shown, ["Anna"]);
    // The Party's log on the server holds the profile operation for the other members.
    let database = Database::open(&path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    let session = database.session()?;
    let (status, body) = group_log(&harness, &clock, &session, &party).await?;
    assert_eq!(status, 200);
    let log = GroupMessages::decode(&body)?;
    let applications = log
        .messages
        .iter()
        .filter(|entry| entry.content_type == pingers_proto::ContentType::Application)
        .count();
    assert_eq!(applications, 1, "one profile operation");
    assert!(
        !PartyLifecycle::new(&session, &clock).profile_waiting(&party)?,
        "nothing left waiting"
    );
    Ok(())
}
