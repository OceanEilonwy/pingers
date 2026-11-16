// SPDX-License-Identifier: AGPL-3.0-only
//! The first-phone setup link on the real crates against the in-process harness: a fresh
//! client checks a link (sending nothing), then uses it to register with the link's code.
//! Each outcome, the pin rules for builds with and without pins, the push relay's first-use
//! pins taken from the link, a refused code that is not sent again, and an account made
//! already.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_client::ports::{SetupCheck, SetupOutcome};
use pingers_client::snapshot::Host;
use pingers_client::{
    Client, ClientConfig, ClientError, Crates, Instance, KeyPins, Outside, Trust,
};
use pingers_crypto::key_config_hash;
use pingers_proto::{
    KeyConfigHash, PushRelayKeyConfig, PushRelayKeyId, SecretText, ServerId, SetupPayload,
};
use pingers_store::{AccountRegistration, Database, DatabaseKey};
use pingers_testkit::{FakePlatform, HarnessConfig, InProcessHarness, RecordingEvents};
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
    Ohttp(#[from] pingers_ohttp::OhttpError),
    #[error(transparent)]
    Snapshot(#[from] pingers_client::snapshot::SnapshotError),
    #[error(transparent)]
    Records(#[from] pingers_mls::StorageError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("setup: {0}")]
    Setup(&'static str),
}

const KEY: [u8; 32] = [8; 32];
const CODE: &str = "first-owner-0123456789";
/// Where pingers-client keeps the push relay's first-use pins.
const PIN_RECORD: &[u8] = b"pingers-client/v1/push-relay-pin";
/// How long a step may take in real time; a hang fails the test instead of stalling it.
const STEP: std::time::Duration = std::time::Duration::from_secs(60);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pingers-client-setup-{}-{unique}",
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

/// A key config hash no harness key has.
fn stranger() -> KeyConfigHash {
    KeyConfigHash::from_bytes([0xee; 32])
}

/// The keys the harness serves, as the appliance computes its pins.
struct Served {
    gateway: KeyConfigHash,
    push_relay: KeyConfigHash,
}

fn served(harness: &InProcessHarness) -> Result<Served, TestError> {
    let endpoints = harness.endpoints();
    let push_key: [u8; 32] = endpoints
        .push_relay_key
        .as_slice()
        .try_into()
        .map_err(|_| TestError::Setup("push relay key length"))?;
    Ok(Served {
        gateway: key_config_hash(&endpoints.ohttp_key_config),
        push_relay: key_config_hash(
            &PushRelayKeyConfig {
                key_id: PushRelayKeyId::new(1),
                public_key: push_key,
            }
            .to_bytes(),
        ),
    })
}

/// The link the appliance would show: its server, `code`, and its pins, the served key first
/// and a next one after it.
fn link(
    server: &str,
    code: &str,
    gateway: KeyConfigHash,
    push_relay: KeyConfigHash,
) -> Result<SetupPayload, TestError> {
    let link = SetupPayload {
        server: ServerId::new(server.to_owned())?,
        registration_code: SecretText::new(code.to_owned()),
        gateway_pins: vec![gateway, stranger()],
        push_relay_pins: vec![push_relay, stranger()],
    };
    // As the app gets it: through the link's text.
    Ok(SetupPayload::from_link(&link.to_link()?)?)
}

/// The build's pins: `None` pins on first use.
struct BuildPins {
    gateway: Option<KeyPins>,
    push_relay: Option<KeyPins>,
}

const FIRST_USE: BuildPins = BuildPins {
    gateway: None,
    push_relay: None,
};

/// A release build pointed at the harness (no registration code of its own), trusting its CA.
fn instance(harness: &InProcessHarness, pins: BuildPins) -> Result<Instance, TestError> {
    let endpoints = harness.endpoints();
    Ok(Instance {
        server: ServerId::new(endpoints.server_id.clone())?,
        relay: format!("http://{}/ohttp", endpoints.ohttp_relay),
        push_relay: Some(format!("https://{}", endpoints.push_relay)),
        gateway_pins: pins.gateway,
        push_relay_pins: pins.push_relay,
        trust: Trust::from_pem(&endpoints.push_relay_ca)?,
        registration_code: None,
    })
}

async fn open(instance: Instance, database: PathBuf) -> Result<Client, TestError> {
    let clock = HarnessConfig::default().clock;
    let platform = FakePlatform::new(clock.clone());
    platform
        .secure_store_put("pingers.database-key", &KEY)
        .map_err(|_locked| TestError::Setup("secure store"))?;
    let seeds = Arc::new(AtomicU64::new(700));
    let outside = Outside {
        prover_self_test: pingers_sync::SelfTestPlan::new(
            core::num::NonZeroU8::MIN,
            core::time::Duration::from_millis(1_500),
        ),
        clock: Arc::new(clock.clone()),
        timer: Arc::new(clock),
        rng: Arc::new(move || {
            Box::new(ChaCha20Rng::seed_from_u64(
                seeds.fetch_add(1, Ordering::Relaxed),
            ))
        }),
    };
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

/// A harness whose server takes [`CODE`].
fn harness() -> Result<InProcessHarness, TestError> {
    Ok(InProcessHarness::start(&HarnessConfig {
        registration_codes: vec![CODE.to_owned()],
        ..HarnessConfig::default()
    })?)
}

fn host(harness: &InProcessHarness) -> Result<Host, TestError> {
    let origin = harness.endpoints().server_id.as_str();
    Ok(Host::new(
        origin.strip_prefix("https://").unwrap_or(origin),
    )?)
}

async fn use_link(client: &Client, link: SetupPayload) -> Result<SetupOutcome, TestError> {
    tokio::time::timeout(STEP, client.use_setup_link(link))
        .await
        .map_err(|_elapsed| TestError::Setup("using the link did not finish"))?
        .map_err(TestError::from)
}

fn registration(path: &std::path::Path) -> Result<AccountRegistration, TestError> {
    let database = Database::open(path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    Ok(database.session()?.account().registration()?)
}

fn recorded_push_relay_pins(path: &std::path::Path) -> Result<Option<Vec<u8>>, TestError> {
    let database = Database::open(path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    Ok(pingers_mls::MlsStorage::read(&database.session()?, PIN_RECORD)?.map(|pins| pins.to_vec()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_without_pins_registers_with_the_link_and_takes_its_push_relay_pins()
-> Result<(), TestError> {
    let harness = harness()?;
    let keys = served(&harness)?;
    let dir = TestDir::new()?;
    let client = open(instance(&harness, FIRST_USE)?, dir.database()).await?;
    let server = harness.endpoints().server_id.clone();
    let link = link(&server, CODE, keys.gateway, keys.push_relay)?;

    // The check sends nothing and leaves the code unoffered.
    assert_eq!(
        client.check_setup_link(&link).await?,
        SetupCheck::Ready {
            server: host(&harness)?
        }
    );
    assert!(matches!(
        registration(&dir.database())?,
        AccountRegistration::Pending { code: None }
    ));

    assert_eq!(use_link(&client, link.clone()).await?, SetupOutcome::SetUp);
    assert!(matches!(
        registration(&dir.database())?,
        AccountRegistration::Registered { code: Some(code) } if code.expose() == CODE
    ));
    // The link's push relay pins are the first-use pins now, in its order.
    let mut expected = keys.push_relay.as_bytes().to_vec();
    expected.extend_from_slice(stranger().as_bytes());
    assert_eq!(recorded_push_relay_pins(&dir.database())?, Some(expected));

    // Opened again, the link finds the account made: nothing is offered.
    assert_eq!(
        client.check_setup_link(&link).await?,
        SetupCheck::AlreadySetUp
    );
    assert_eq!(use_link(&client, link).await?, SetupOutcome::AlreadySetUp);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn learned_push_relay_pins_are_kept() -> Result<(), TestError> {
    let harness = harness()?;
    let keys = served(&harness)?;
    let dir = TestDir::new()?;
    let learned = [0x44_u8; 32];
    {
        let database = Database::open(
            &dir.database(),
            DatabaseKey::from_bytes(Zeroizing::new(KEY)),
        )?;
        pingers_mls::MlsStorage::write(&database.session()?, PIN_RECORD, &learned)?;
    }
    let client = open(instance(&harness, FIRST_USE)?, dir.database()).await?;
    let server = harness.endpoints().server_id.clone();
    let link = link(&server, CODE, keys.gateway, keys.push_relay)?;
    assert_eq!(use_link(&client, link).await?, SetupOutcome::SetUp);
    assert_eq!(
        recorded_push_relay_pins(&dir.database())?,
        Some(learned.to_vec())
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_without_pins_sends_no_code_when_the_relay_serves_another_key()
-> Result<(), TestError> {
    let harness = harness()?;
    let keys = served(&harness)?;
    let dir = TestDir::new()?;
    let client = open(instance(&harness, FIRST_USE)?, dir.database()).await?;
    let server = harness.endpoints().server_id.clone();
    // The link names a gateway key the relay does not serve (pins of another appliance).
    let link = link(&server, CODE, stranger(), keys.push_relay)?;
    // Nothing is fetched for the check, so it cannot tell yet.
    assert!(matches!(
        client.check_setup_link(&link).await?,
        SetupCheck::Ready { .. }
    ));
    assert_eq!(
        use_link(&client, link).await?,
        SetupOutcome::ServerKeysDiffer {
            server: host(&harness)?
        }
    );
    // The code was neither offered nor sent, and no push relay pin was taken from the link.
    assert!(matches!(
        registration(&dir.database())?,
        AccountRegistration::Pending { code: None }
    ));
    assert_eq!(recorded_push_relay_pins(&dir.database())?, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_with_pins_takes_only_a_link_whose_served_keys_it_pins() -> Result<(), TestError> {
    let harness = harness()?;
    let keys = served(&harness)?;
    let dir = TestDir::new()?;
    let pins = || -> Result<BuildPins, TestError> {
        Ok(BuildPins {
            gateway: Some(KeyPins::new([keys.gateway, stranger()])?),
            push_relay: Some(KeyPins::new([stranger(), keys.push_relay])?),
        })
    };
    let client = open(instance(&harness, pins()?)?, dir.database()).await?;
    let server = harness.endpoints().server_id.clone();
    let here = host(&harness)?;

    // A link whose served gateway key the build does not pin: an older build.
    let gateway_unpinned = link(
        &server,
        CODE,
        KeyConfigHash::from_bytes([1; 32]),
        keys.push_relay,
    )?;
    assert_eq!(
        client.check_setup_link(&gateway_unpinned).await?,
        SetupCheck::BuildKeysDiffer {
            server: here.clone()
        }
    );
    assert_eq!(
        use_link(&client, gateway_unpinned).await?,
        SetupOutcome::BuildKeysDiffer {
            server: here.clone()
        }
    );
    // The same for the push relay's served key.
    let push_unpinned = link(
        &server,
        CODE,
        keys.gateway,
        KeyConfigHash::from_bytes([2; 32]),
    )?;
    assert_eq!(
        use_link(&client, push_unpinned).await?,
        SetupOutcome::BuildKeysDiffer {
            server: here.clone()
        }
    );
    assert!(matches!(
        registration(&dir.database())?,
        AccountRegistration::Pending { code: None }
    ));

    // Both served keys pinned: the account is made, and nothing is recorded as a first-use
    // pin, since the build has pins.
    let pinned = link(&server, CODE, keys.gateway, keys.push_relay)?;
    assert_eq!(
        client.check_setup_link(&pinned).await?,
        SetupCheck::Ready { server: here }
    );
    assert_eq!(use_link(&client, pinned).await?, SetupOutcome::SetUp);
    assert!(matches!(
        registration(&dir.database())?,
        AccountRegistration::Registered { code: Some(code) } if code.expose() == CODE
    ));
    assert_eq!(recorded_push_relay_pins(&dir.database())?, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_code_is_not_sent_again() -> Result<(), TestError> {
    let harness = harness()?;
    let keys = served(&harness)?;
    let dir = TestDir::new()?;
    let client = open(instance(&harness, FIRST_USE)?, dir.database()).await?;
    let server = harness.endpoints().server_id.clone();
    let wrong = link(
        &server,
        "reset-since-0123456789",
        keys.gateway,
        keys.push_relay,
    )?;
    assert_eq!(
        use_link(&client, wrong.clone()).await?,
        SetupOutcome::Refused
    );
    assert!(matches!(
        registration(&dir.database())?,
        AccountRegistration::Refused { code: Some(code) } if code.expose() == "reset-since-0123456789"
    ));
    // With the server gone, the same link still answers Refused, not Unreachable: it is known
    // from the store and nothing is sent.
    harness.shutdown();
    assert_eq!(client.check_setup_link(&wrong).await?, SetupCheck::Refused);
    assert_eq!(use_link(&client, wrong).await?, SetupOutcome::Refused);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_link_with_another_code_is_offered_after_a_refusal() -> Result<(), TestError> {
    let harness = harness()?;
    let keys = served(&harness)?;
    let dir = TestDir::new()?;
    let client = open(instance(&harness, FIRST_USE)?, dir.database()).await?;
    let server = harness.endpoints().server_id.clone();
    let wrong = link(
        &server,
        "reset-since-0123456789",
        keys.gateway,
        keys.push_relay,
    )?;
    assert_eq!(use_link(&client, wrong).await?, SetupOutcome::Refused);
    let new = link(&server, CODE, keys.gateway, keys.push_relay)?;
    assert!(matches!(
        client.check_setup_link(&new).await?,
        SetupCheck::Ready { .. }
    ));
    assert_eq!(use_link(&client, new).await?, SetupOutcome::SetUp);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_link_for_another_server_names_both_and_sends_nothing() -> Result<(), TestError> {
    let harness = harness()?;
    let keys = served(&harness)?;
    let dir = TestDir::new()?;
    let client = open(instance(&harness, FIRST_USE)?, dir.database()).await?;
    let elsewhere = link(
        "https://other.example:8443",
        CODE,
        keys.gateway,
        keys.push_relay,
    )?;
    let expected = (host(&harness)?, Host::new("other.example:8443")?);
    assert_eq!(
        client.check_setup_link(&elsewhere).await?,
        SetupCheck::OtherServer {
            app: expected.0.clone(),
            link: expected.1.clone(),
        }
    );
    assert_eq!(
        use_link(&client, elsewhere).await?,
        SetupOutcome::OtherServer {
            app: expected.0,
            link: expected.1,
        }
    );
    assert!(matches!(
        registration(&dir.database())?,
        AccountRegistration::Pending { code: None }
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_server_is_reported_and_tried_again_only_when_asked() -> Result<(), TestError>
{
    // Nothing listens on the relay or the server: a phone offline.
    let harness = harness()?;
    let keys = served(&harness)?;
    let mut offline = instance(&harness, FIRST_USE)?;
    harness.shutdown();
    offline.relay = "http://127.0.0.1:9/ohttp".to_owned();
    let server = offline.server.as_str().to_owned();
    let dir = TestDir::new()?;
    let client = open(offline, dir.database()).await?;
    let link = link(&server, CODE, keys.gateway, keys.push_relay)?;
    let here = Host::new(server.strip_prefix("https://").unwrap_or(&server))?;
    for _ in 0..2 {
        assert_eq!(
            use_link(&client, link.clone()).await?,
            SetupOutcome::Unreachable {
                server: here.clone()
            }
        );
    }
    // The gateway key could not be met, so the code was not offered.
    assert!(matches!(
        registration(&dir.database())?,
        AccountRegistration::Pending { code: None }
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pinned_build_offline_keeps_the_code_for_the_next_tap() -> Result<(), TestError> {
    let harness = harness()?;
    let keys = served(&harness)?;
    let mut offline = instance(
        &harness,
        BuildPins {
            gateway: Some(KeyPins::new([keys.gateway])?),
            push_relay: Some(KeyPins::new([keys.push_relay])?),
        },
    )?;
    harness.shutdown();
    offline.relay = "http://127.0.0.1:9/ohttp".to_owned();
    let server = offline.server.as_str().to_owned();
    let dir = TestDir::new()?;
    let client = open(offline, dir.database()).await?;
    let link = link(&server, CODE, keys.gateway, keys.push_relay)?;
    assert!(matches!(
        use_link(&client, link).await?,
        SetupOutcome::Unreachable { .. }
    ));
    // The build's pins vouched for the link, so its code waits for the next registration.
    assert!(matches!(
        registration(&dir.database())?,
        AccountRegistration::Pending { code: Some(code) } if code.expose() == CODE
    ));
    Ok(())
}
