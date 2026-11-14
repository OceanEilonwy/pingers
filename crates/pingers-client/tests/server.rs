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
use pingers_client::snapshot::{Name, PartyColour, PostQuantum, RelayStatus};
use pingers_client::{Client, ClientConfig, ClientError, Crates, Instance, Outside, Trust};
use pingers_ohttp::BinaryRequest;
use pingers_parties::PartyLifecycle;
use pingers_proto::{GroupMessages, InvitePayload, SecretText, ServerId, WireDecode as _};
use pingers_store::{AccountRegistration, Database, DatabaseKey, MailboxPurpose, Session};
use pingers_sync::{ApiRequest, GroupSigner};
use pingers_testkit::{FakePlatform, HarnessConfig, InProcessHarness, RecordingEvents};
use pingers_time::{Clock as _, ManualClock};
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
/// with the harness CA as its only trust root, the relay, the push relay, first-use pins and
/// `PINGERS_DEV_REGISTRATION_CODE`.
fn instance(harness: &InProcessHarness, code: Option<&str>) -> Result<Instance, TestError> {
    let endpoints = harness.endpoints();
    Ok(Instance {
        server: ServerId::new(endpoints.server_id.clone())?,
        // As deployments configure it (PINGERS_OHTTP_RELAY): the relay URL, not its origin.
        relay: format!("http://{}/ohttp", endpoints.ohttp_relay),
        push_relay: Some(format!("https://{}", endpoints.push_relay)),
        gateway_pins: None,
        push_relay_pins: None,
        trust: Trust::from_pem(&endpoints.push_relay_ca)?,
        registration_code: code.map(|code| SecretText::new(code.to_owned())),
    })
}

async fn open(
    harness: &InProcessHarness,
    clock: &ManualClock,
    database: PathBuf,
    build_code: Option<&str>,
) -> Result<Client, TestError> {
    let platform = FakePlatform::new(clock.clone());
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
    // The account was made, and registered with the build's code.
    let identity = session
        .identity()
        .load()?
        .ok_or(TestError::Setup("no account"))?;
    assert_eq!(identity.server.as_str(), harness.endpoints().server_id);
    assert!(matches!(
        session.account().registration()?,
        AccountRegistration::Registered { code: Some(code) } if code.expose() == CODE
    ));
    assert!(session.account().bearer()?.is_some());
    assert!(session.rln_identity().load()?.is_some(), "membership leaf");
    // A full batch of tokens, less the one the group creation spent.
    assert_eq!(session.privacy_pass_tokens().count()?, 299);

    // The server holds the Party's group.
    let (status, body) = group_log(&harness, &clock, &session, &party).await?;
    assert_eq!(status, 200, "the group exists on the server");
    let _log = GroupMessages::decode(&body)?;
    // A session holds the store's cross-process lock: the client needs it back.
    drop(session);

    // A wake registers a knock mailbox for the pool.
    tokio::time::timeout(STEP, client.on_wake())
        .await
        .map_err(|_elapsed| TestError::Setup("the wake did not finish"))??;
    let session = database.session()?;
    assert_eq!(
        session
            .mailboxes()
            .pooled_count(MailboxPurpose::Knock, clock.now())?,
        1
    );
    // The wake also uploaded its lease: one token for the board sync, one for the mailbox.
    assert!(session.board_upload().last()?.is_some());
    assert_eq!(session.privacy_pass_tokens().count()?, 297);

    // An invite this device makes passes its registration code on. The invite mailbox is put
    // in the pool by hand: refilling it is the invite flow's part.
    session.mailboxes().insert(&pingers_store::Mailbox {
        read_key: pingers_crypto::MailboxReadKey::from_bytes(Zeroizing::new([9; 32])),
        purpose: MailboxPurpose::Invite,
        state: pingers_store::MailboxState::Pooled,
        push: None,
        expires_at: clock
            .now()
            .checked_add(std::time::Duration::from_hours(48))
            .ok_or(TestError::Setup("time"))?,
    })?;
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
        registration_code: Some(SecretText::new(CODE.to_owned())),
    })?;
    // Opening the link registers the account with the link's code first; the join itself
    // then waits for a push token, which this device has none of.
    let opened = tokio::time::timeout(
        STEP,
        client.open_invite(
            pingers_client::snapshot::InviteUrl::new(link.as_str())?,
            Name::new("Bea")?,
        ),
    )
    .await
    .map_err(|_elapsed| TestError::Setup("opening the invite did not finish"))?;
    assert!(matches!(
        opened,
        Err(ClientError::Unavailable(
            pingers_client::Unavailable::NoPushToken
        ))
    ));
    let database = Database::open(&path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    let session = database.session()?;
    assert!(matches!(
        session.account().registration()?,
        AccountRegistration::Registered { code: Some(code) } if code.expose() == CODE
    ));
    assert_eq!(session.privacy_pass_tokens().count()?, 300);
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
