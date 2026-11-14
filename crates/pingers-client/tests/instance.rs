// SPDX-License-Identifier: AGPL-3.0-only
//! The client on the real crates against the in-process harness (pingers-server, an OHTTP
//! relay, pingers-push-relay): the instance's pin sets, the reachability check, wake-up
//! pushes sealed to the pinned push relay key, and the server's map host.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_client::snapshot::{RelayStatus, WakePushes};
use pingers_client::{
    Client, ClientConfig, ClientError, Crates, Instance, KeyPins, Outside, Trust, Unavailable,
};
use pingers_crypto::{AccountSigningKey, account_id, key_config_hash};
use pingers_proto::{
    KeyConfigHash, ProtoError, PushPlatform, PushRelayKeyConfig, PushRelayKeyId, PushToken,
    ServerId,
};
use pingers_store::{Database, DatabaseKey, Identity, StoreError};
use pingers_testkit::{
    FakePlatform, HARNESS_MAP_HOST, HarnessConfig, InProcessHarness, PushRegistration,
    RecordingEvents,
};
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
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Harness(#[from] pingers_testkit::HarnessError),
    #[error(transparent)]
    Ohttp(#[from] pingers_ohttp::OhttpError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Snapshot(#[from] pingers_client::snapshot::SnapshotError),
    #[error(transparent)]
    Records(#[from] pingers_mls::StorageError),
    #[error("setup: {0}")]
    Setup(&'static str),
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_join_needs_a_sealed_subscription_and_a_real_link() -> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let rig = rig(&harness, &config, harness_pins(&harness)?).await?;
    let link = pingers_client::snapshot::InviteUrl::new("https://pingers.invalid/j#x")?;
    let name = pingers_client::snapshot::Name::new("Bob")?;
    // No token from the platform: no subscription can be sealed for the join request.
    assert!(matches!(
        rig.client.open_invite(link.clone(), name.clone()).await,
        Err(ClientError::Unavailable(Unavailable::NoPushToken))
    ));
    rig.platform.set_push_registration(Some(PushRegistration {
        platform: PushPlatform::UnifiedPush,
        token: PushToken::from_bytes(b"https://push.harness.invalid/up/abc".to_vec()),
    }));
    // With a token the subscription is sealed to the pinned push relay key; the link is read
    // next, and this one holds no invite.
    assert!(matches!(
        rig.client.open_invite(link, name).await,
        Err(ClientError::Parties(_))
    ));
    // Nothing was started, so there is nothing to accept.
    assert!(matches!(
        rig.client.accept_invite().await,
        Err(ClientError::Parties(
            pingers_parties::PartiesError::UnknownJoin
        ))
    ));
    Ok(())
}

const KEY: [u8; 32] = [4; 32];

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pingers-client-instance-{}-{unique}",
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

struct Rig {
    client: Client,
    platform: FakePlatform,
    database: PathBuf,
    _dir: TestDir,
}

/// The pins an official build would carry for the harness: the served key and a next one.
struct Pins {
    gateway: KeyPins,
    /// `None`: a build without push relay pins, which pins on first use.
    push_relay: Option<KeyPins>,
    /// A first-use pin already recorded by an earlier run.
    recorded: Option<[u8; 32]>,
}

fn harness_pins(harness: &InProcessHarness) -> Result<Pins, TestError> {
    let endpoints = harness.endpoints();
    let next = KeyConfigHash::from_bytes([0xee; 32]);
    let push_key: [u8; 32] = endpoints
        .push_relay_key
        .as_slice()
        .try_into()
        .map_err(|_| TestError::Setup("push relay key length"))?;
    let push_hash = key_config_hash(
        &PushRelayKeyConfig {
            key_id: PushRelayKeyId::new(1),
            public_key: push_key,
        }
        .to_bytes(),
    );
    Ok(Pins {
        gateway: KeyPins::new([key_config_hash(&endpoints.ohttp_key_config), next])?,
        push_relay: Some(KeyPins::new([next, push_hash])?),
        recorded: None,
    })
}

async fn rig(
    harness: &InProcessHarness,
    config: &HarnessConfig,
    pins: Pins,
) -> Result<Rig, TestError> {
    let dir = TestDir::new()?;
    let endpoints = harness.endpoints();
    let server = ServerId::new(endpoints.server_id.clone())?;
    let database_path = dir.0.join("pingers.db");
    {
        let database =
            Database::open(&database_path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
        let signing_key = AccountSigningKey::generate(&mut ChaCha20Rng::seed_from_u64(3));
        database.session()?.identity().save(&Identity {
            account: account_id(&signing_key.public_key()),
            signing_key,
            server: server.clone(),
            server_pin: KeyConfigHash::from_bytes([3; 32]),
        })?;
        if let Some(pin) = pins.recorded {
            pingers_mls::MlsStorage::write(&database.session()?, PIN_RECORD, &pin)?;
        }
    }
    let instance = Instance {
        server,
        relay: format!("http://{}", endpoints.ohttp_relay),
        push_relay: Some(format!("https://{}/", endpoints.push_relay)),
        gateway_pins: Some(pins.gateway),
        push_relay_pins: pins.push_relay,
        trust: Trust::Mozilla,
        registration_code: None,
    };
    let clock = config.clock.clone();
    let platform = FakePlatform::new(clock.clone());
    platform
        .secure_store_put("pingers.database-key", &KEY)
        .map_err(|_locked| TestError::Setup("secure store"))?;
    let seeds = Arc::new(AtomicU64::new(500));
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
    let client = Client::open_with(
        ClientConfig {
            database: database_path.clone(),
            instance: Some(instance),
        },
        Arc::new(platform.clone()),
        Arc::new(RecordingEvents::new()),
        move |database| Crates::real(database, crates_platform, outside, Some(&opened)),
    )
    .await?;
    Ok(Rig {
        client,
        platform,
        database: database_path,
        _dir: dir,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pinned_instance_is_reachable_and_seals_wake_pushes_to_the_pinned_key()
-> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let rig = rig(&harness, &config, harness_pins(&harness)?).await?;

    assert_eq!(
        rig.client.check_reachability().await?,
        RelayStatus::Reachable
    );
    assert_eq!(rig.client.snapshot().relay, RelayStatus::Reachable);
    // The check read the parameters too, for New party's operators: the map host came with them.
    assert_eq!(
        rig.client
            .snapshot()
            .map_host
            .as_ref()
            .map(|url| url.as_str().to_owned()),
        Some(HARNESS_MAP_HOST.to_owned())
    );

    rig.platform.set_push_registration(Some(PushRegistration {
        platform: PushPlatform::UnifiedPush,
        token: PushToken::from_bytes(b"https://push.harness.invalid/up/abc".to_vec()),
    }));
    rig.client.set_wake_pushes(WakePushes::On).await?;
    assert_eq!(rig.client.snapshot().settings.wake_pushes, WakePushes::On);
    // The key matched the build's pins: nothing is recorded as a first-use pin.
    assert_eq!(recorded_pin(&rig)?, None);
    rig.client.push_tokens_changed().await?;
    // The map host stays known.
    assert_eq!(
        rig.client
            .snapshot()
            .map_host
            .as_ref()
            .map(|url| url.as_str().to_owned()),
        Some(HARNESS_MAP_HOST.to_owned())
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpinned_keys_are_refused_with_first_use_off() -> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let stranger = KeyPins::new([KeyConfigHash::from_bytes([1; 32])])?;
    let pins = harness_pins(&harness)?;
    let rig = rig(
        &harness,
        &config,
        Pins {
            gateway: pins.gateway,
            push_relay: Some(stranger.clone()),
            recorded: None,
        },
    )
    .await?;
    rig.platform.set_push_registration(Some(PushRegistration {
        platform: PushPlatform::UnifiedPush,
        token: PushToken::from_bytes(b"https://push.harness.invalid/up/abc".to_vec()),
    }));
    // The gateway matches; the push relay's key does not, so no token is sealed to it, whether
    // pushes are turned on or the platform hands over a new token.
    assert!(matches!(
        rig.client.set_wake_pushes(WakePushes::On).await,
        Err(ClientError::Sync(
            pingers_sync::SyncError::PushRelayKeyMismatch
        ))
    ));
    assert!(matches!(
        rig.client.push_tokens_changed().await,
        Err(ClientError::Sync(
            pingers_sync::SyncError::PushRelayKeyMismatch
        ))
    ));
    // A pinned build never records a first-use pin.
    assert_eq!(recorded_pin(&rig)?, None);

    let gateway_wrong = rig_with_gateway(&harness, &config, stranger).await?;
    assert_eq!(
        gateway_wrong.client.check_reachability().await?,
        RelayStatus::KeyMismatch
    );
    assert_eq!(
        gateway_wrong.client.snapshot().relay,
        RelayStatus::KeyMismatch
    );
    Ok(())
}

async fn rig_with_gateway(
    harness: &InProcessHarness,
    config: &HarnessConfig,
    gateway: KeyPins,
) -> Result<Rig, TestError> {
    let pins = harness_pins(harness)?;
    rig(
        harness,
        config,
        Pins {
            gateway,
            push_relay: pins.push_relay,
            recorded: None,
        },
    )
    .await
}

/// Where the client keeps a first-use push relay pin.
const PIN_RECORD: &[u8] = b"pingers-client/v1/push-relay-pin";

fn recorded_pin(rig: &Rig) -> Result<Option<Vec<u8>>, TestError> {
    let database = Database::open(&rig.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    Ok(pingers_mls::MlsStorage::read(&database.session()?, PIN_RECORD)?.map(|pin| pin.to_vec()))
}

fn up_token(rig: &Rig) {
    rig.platform.set_push_registration(Some(PushRegistration {
        platform: PushPlatform::UnifiedPush,
        token: PushToken::from_bytes(b"https://push.harness.invalid/up/abc".to_vec()),
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_push_relay_pins_the_first_key_is_recorded_and_then_enforced()
-> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let pins = harness_pins(&harness)?;
    let expected = pins
        .push_relay
        .as_ref()
        .and_then(|pins| pins.hashes().get(1).copied())
        .ok_or(TestError::Setup("harness pin"))?;
    let first = rig(
        &harness,
        &config,
        Pins {
            gateway: pins.gateway.clone(),
            push_relay: None,
            recorded: None,
        },
    )
    .await?;
    up_token(&first);
    first.client.set_wake_pushes(WakePushes::On).await?;
    assert_eq!(recorded_pin(&first)?, Some(expected.as_bytes().to_vec()));

    // A recorded pin the relay's key does not match: the key is refused, the pin kept.
    let tampered = rig(
        &harness,
        &config,
        Pins {
            gateway: pins.gateway,
            push_relay: None,
            recorded: Some([7; 32]),
        },
    )
    .await?;
    up_token(&tampered);
    assert!(matches!(
        tampered.client.set_wake_pushes(WakePushes::On).await,
        Err(ClientError::Sync(
            pingers_sync::SyncError::PushRelayKeyMismatch
        ))
    ));
    assert_eq!(recorded_pin(&tampered)?, Some(vec![7; 32]));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turning_wake_pushes_off_drops_the_seals() -> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let rig = rig(&harness, &config, harness_pins(&harness)?).await?;
    up_token(&rig);
    rig.client.set_wake_pushes(WakePushes::On).await?;
    rig.client.set_wake_pushes(WakePushes::NoWakePushes).await?;
    // With the seals gone a join request has no subscription to carry.
    let link = pingers_client::snapshot::InviteUrl::new("https://pingers.invalid/j#x")?;
    assert!(matches!(
        rig.client
            .open_invite(link, pingers_client::snapshot::Name::new("Bob")?)
            .await,
        Err(ClientError::Unavailable(Unavailable::WakePushesOff))
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_pushes_need_the_platform_s_push_token() -> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let rig = rig(&harness, &config, harness_pins(&harness)?).await?;
    // No token from the platform yet.
    assert!(matches!(
        rig.client.set_wake_pushes(WakePushes::On).await,
        Err(ClientError::Unavailable(Unavailable::NoPushToken))
    ));
    Ok(())
}
