// SPDX-License-Identifier: AGPL-3.0-only
//! The client on the real crates against the in-process harness (pingers-server, an OHTTP
//! relay, pingers-push-relay): the instance's pin sets, the reachability check, wake-up
//! pushes sealed to the pinned push relay key, and the server's map host.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_client::snapshot::{PushDelivery, PushDowngrade, RelayStatus, WakePushes};
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
use pingers_wake::PushPriority;
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
async fn a_join_goes_ahead_without_a_push_token_and_every_join_needs_a_real_link()
-> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let rig = rig(&harness, &config, harness_pins(&harness)?).await?;
    let link = pingers_client::snapshot::InviteUrl::new("https://pingers.invalid/j#x")?;
    let name = pingers_client::snapshot::Name::new("Bob")?;
    // No token from the platform yet: the request would go without a subscription (the device
    // polls instead), so the link is read next, and this one holds no invite.
    assert!(matches!(
        rig.client.open_invite(link.clone(), name.clone()).await,
        Err(ClientError::Parties(_))
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
    // Nothing was started, so there is nothing to withdraw.
    assert!(matches!(
        rig.client.withdraw_join().await,
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
    events: RecordingEvents,
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
    rig_as(
        harness,
        config,
        pins,
        Who {
            seed: 3,
            account: Account::Preset,
            relay: None,
        },
    )
    .await
}

/// How a rig's account comes about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Account {
    /// Saved in the store before the client opens, as a registered device has it.
    Preset,
    /// None: the client registers one itself on its first credentialed request, trusting
    /// the harness's HTTPS front and pinning keys on first use.
    Register,
}

/// Which person a rig is: the seed of its random streams, how it gets an account, and the
/// OHTTP relay it uses (`None`: the harness's).
struct Who {
    seed: u64,
    account: Account,
    relay: Option<std::net::SocketAddr>,
}

/// A client whose random streams (and preset account key) come from `seed`, so two rigs are
/// two people.
async fn rig_as(
    harness: &InProcessHarness,
    config: &HarnessConfig,
    pins: Pins,
    who: Who,
) -> Result<Rig, TestError> {
    let Who {
        seed,
        account,
        relay,
    } = who;
    let relay = relay.unwrap_or(harness.endpoints().ohttp_relay);
    let dir = TestDir::new()?;
    let endpoints = harness.endpoints();
    let server = ServerId::new(endpoints.server_id.clone())?;
    let database_path = dir.0.join("pingers.db");
    if account == Account::Preset {
        let database =
            Database::open(&database_path, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
        let signing_key = AccountSigningKey::generate(&mut ChaCha20Rng::seed_from_u64(seed));
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
    let instance = match account {
        Account::Preset => Instance {
            server,
            relay: format!("http://{relay}"),
            push_relay: Some(format!("https://{}/", endpoints.push_relay)),
            gateway_pins: Some(pins.gateway),
            push_relay_pins: pins.push_relay,
            trust: Trust::Mozilla,
            registration_code: None,
        },
        Account::Register => Instance {
            server,
            relay: format!("http://{relay}/ohttp"),
            push_relay: Some(format!("https://{}", endpoints.push_relay)),
            gateway_pins: None,
            push_relay_pins: None,
            trust: Trust::from_pem(&endpoints.push_relay_ca)?,
            registration_code: None,
        },
    };
    let clock = config.clock.clone();
    let platform = FakePlatform::new(clock.clone());
    platform
        .secure_store_put("pingers.database-key", &KEY)
        .map_err(|_locked| TestError::Setup("secure store"))?;
    // Each person draws from their own stream, or two would make the same mailbox ids.
    let seeds = Arc::new(AtomicU64::new(
        500_u64.saturating_add(seed.saturating_mul(100_000)),
    ));
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
    let events = RecordingEvents::new();
    let crates_platform: Arc<dyn pingers_client::Platform> = Arc::new(platform.clone());
    let opened = instance.clone();
    let client = Client::open_with(
        ClientConfig {
            database: database_path.clone(),
            instance: Some(instance),
        },
        Arc::new(platform.clone()),
        Arc::new(events.clone()),
        move |database| Crates::real(database, crates_platform, outside, Some(&opened)),
    )
    .await?;
    Ok(Rig {
        client,
        platform,
        events,
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
    // With the seals gone a join request carries no subscription, so it gets as far as
    // reading the link, which holds no invite.
    let link = pingers_client::snapshot::InviteUrl::new("https://pingers.invalid/j#x")?;
    assert!(matches!(
        rig.client
            .open_invite(link, pingers_client::snapshot::Name::new("Bob")?)
            .await,
        Err(ClientError::Parties(
            pingers_parties::PartiesError::InvalidLink
        ))
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

fn delivery(rig: &Rig) -> PushDelivery {
    rig.client.snapshot().push_delivery
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_snapshot_says_why_wake_pushes_are_downgraded() -> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let rig = rig(&harness, &config, harness_pins(&harness)?).await?;
    rig.client.publish_snapshot().await?;
    // No UnifiedPush distributor or FCM registration yet.
    assert_eq!(
        delivery(&rig),
        PushDelivery::Downgraded(PushDowngrade::NoPushToken)
    );
    up_token(&rig);
    rig.client.set_wake_pushes(WakePushes::On).await?;
    assert_eq!(delivery(&rig), PushDelivery::Full);
    rig.client.set_wake_pushes(WakePushes::NoWakePushes).await?;
    assert_eq!(
        delivery(&rig),
        PushDelivery::Downgraded(PushDowngrade::WakePushesOff)
    );
    rig.client.set_wake_pushes(WakePushes::On).await?;

    // FCM delivering high-priority pushes at normal priority: three in twenty downgrades it.
    for delivered in [
        PushPriority::Normal,
        PushPriority::High,
        PushPriority::Normal,
    ] {
        rig.client
            .report_push_priority(PushPriority::High, delivered)
            .await?;
        assert_eq!(delivery(&rig), PushDelivery::Full);
    }
    rig.client
        .report_push_priority(PushPriority::High, PushPriority::Normal)
        .await?;
    assert_eq!(
        delivery(&rig),
        PushDelivery::Downgraded(PushDowngrade::PriorityLowered)
    );
    for _ in 0..18 {
        rig.client
            .report_push_priority(PushPriority::High, PushPriority::High)
            .await?;
    }
    assert_eq!(delivery(&rig), PushDelivery::Full);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_relay_key_off_the_pins_downgrades_wake_pushes() -> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let pins = harness_pins(&harness)?;
    let rig = rig(
        &harness,
        &config,
        Pins {
            gateway: pins.gateway,
            push_relay: Some(KeyPins::new([KeyConfigHash::from_bytes([1; 32])])?),
            recorded: None,
        },
    )
    .await?;
    up_token(&rig);
    assert!(rig.client.set_wake_pushes(WakePushes::On).await.is_err());
    rig.client.publish_snapshot().await?;
    assert_eq!(
        delivery(&rig),
        PushDelivery::Downgraded(PushDowngrade::RelayKeyMismatch)
    );
    Ok(())
}

/// A client that registers its own account and tokens, with wake-up pushes off: joining no
/// longer needs them, and every wake then reads the mailboxes joins wait on.
async fn person(
    harness: &InProcessHarness,
    config: &HarnessConfig,
    seed: u64,
) -> Result<Rig, TestError> {
    person_via(harness, config, seed, None).await
}

/// As [`person`], through the OHTTP relay at `relay` (`None`: the harness's).
async fn person_via(
    harness: &InProcessHarness,
    config: &HarnessConfig,
    seed: u64,
    relay: Option<std::net::SocketAddr>,
) -> Result<Rig, TestError> {
    let rig = rig_as(
        harness,
        config,
        harness_pins(harness)?,
        Who {
            seed,
            account: Account::Register,
            relay,
        },
    )
    .await?;
    rig.client.set_wake_pushes(WakePushes::NoWakePushes).await?;
    Ok(rig)
}

/// Ana's Party.
async fn climbing_crew(ana: &Rig) -> Result<pingers_proto::PartyId, TestError> {
    use pingers_client::ports::PartyDraft;
    use pingers_client::snapshot::{Name, PartyColour, PostQuantum};

    Ok(ana
        .client
        .create_party(PartyDraft {
            name: Name::new("Climbing crew")?,
            my_name: Name::new("Ana")?,
            colour: PartyColour::Teal,
            post_quantum: PostQuantum::Off,
        })
        .await?)
}

/// Invite mailboxes pooled on `rig`'s device, ready for its next link.
fn pooled_invites(rig: &Rig, config: &HarnessConfig) -> Result<u32, TestError> {
    let database = Database::open(&rig.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    let now = pingers_time::Clock::now(&config.clock);
    Ok(database
        .session()?
        .mailboxes()
        .pooled_count(pingers_store::MailboxPurpose::Invite, now)?)
}

fn asked_code(rig: &Rig) -> Option<pingers_client::snapshot::SafetyCode> {
    match rig.client.snapshot().joining.clone() {
        Some(pingers_client::snapshot::Joining::Asked { code, .. }) => Some(code),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_compare_codes_let_in_then_turn_the_next_one_away() -> Result<(), TestError> {
    use pingers_client::ports::{Foreground, JoinDecision};
    use pingers_client::snapshot::{Joining, Name};
    use pingers_testkit::{EventKind, RecordedEvent};

    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    // Ana has wake-up pushes on: her invites are read when a push names them or when she opens
    // the app, not on every wake.
    let ana = person(&harness, &config, 3).await?;
    up_token(&ana);
    ana.client.set_wake_pushes(WakePushes::On).await?;
    let ben = person(&harness, &config, 4).await?;
    let party = climbing_crew(&ana).await?;
    let link = ana.client.create_invite(party).await?;
    let shared_at = link
        .expires_at
        .checked_sub(pingers_parties::INVITE_LIFETIME)
        .ok_or(TestError::Setup("link expiry"))?;
    config.clock.advance(std::time::Duration::from_secs(600));

    // Ben asks to join; his phone shows the code at once, and nothing about the Party.
    ben.client.open_invite(link.url, Name::new("Ben")?).await?;
    let ben_code = asked_code(&ben).ok_or(TestError::Setup("Ben has no code"))?;
    assert_eq!(ben_code.as_str().len(), 14);
    assert!(ben.client.snapshot().parties.is_empty());

    // A scheduled wake does not read Ana's invites (pushes on); opening the app does: same
    // code, before anyone is let in.
    ana.client.on_wake().await?;
    assert!(ana.client.snapshot().party(party)?.join_requests.is_empty());
    ana.client.on_foreground(Foreground::Visible).await?;
    let snapshot = ana.client.snapshot();
    let requests = &snapshot.party(party)?.join_requests;
    assert_eq!(requests.len(), 1);
    let request = requests.first().ok_or(TestError::Setup("Ben's request"))?;
    assert_eq!(request.name.as_str(), "Ben");
    assert_eq!(request.code, ben_code);
    assert_eq!(request.shared_at, shared_at);
    // Being in a Party, Ana keeps an invite mailbox pooled for the next link.
    assert_eq!(pooled_invites(&ana, &config)?, 1);
    assert!(
        ana.events
            .events()
            .contains(&RecordedEvent::JoinRequested { party })
    );

    // Codes match: Ana lets Ben in. The link stops working.
    ana.client
        .decide_join(party, request.id, JoinDecision::Admit)
        .await?;
    let snapshot = ana.client.snapshot();
    let after = snapshot.party(party)?;
    assert!(after.join_requests.is_empty());
    assert!(after.invites.is_empty());

    // Ben's next wake finds the Welcome: the Party, its members and who let him in appear.
    ben.client.on_wake().await?;
    let snapshot = ben.client.snapshot();
    assert_eq!(
        snapshot.joining,
        Some(Joining::LetIn {
            party,
            inviter: Name::new("Ana")?,
            code: ben_code.clone(),
        })
    );
    let joined = snapshot.party(party)?;
    assert_eq!(joined.name.as_str(), "Climbing crew");
    let names: Vec<&str> = joined
        .members
        .iter()
        .map(|member| member.name.as_str())
        .collect();
    assert_eq!(names, ["Ana", "Ben"]);
    assert_eq!(ben.events.count(EventKind::LetIn), 1);
    ben.client.dismiss_join().await?;
    assert_eq!(ben.client.snapshot().joining, None);

    // A second invite: Cat asks, Ana turns her away, and Cat is never let in.
    let cat = person(&harness, &config, 5).await?;
    let second = ana.client.create_invite(party).await?;
    cat.client
        .open_invite(second.url, Name::new("Cat")?)
        .await?;
    let cat_code = asked_code(&cat).ok_or(TestError::Setup("Cat has no code"))?;
    assert_ne!(cat_code, ben_code);
    ana.client.on_foreground(Foreground::Visible).await?;
    let snapshot = ana.client.snapshot();
    let request = snapshot
        .party(party)?
        .join_requests
        .first()
        .ok_or(TestError::Setup("Cat's request"))?;
    assert_eq!(request.code, cat_code);
    ana.client
        .decide_join(party, request.id, JoinDecision::Refuse)
        .await?;
    let snapshot = ana.client.snapshot();
    let after = snapshot.party(party)?;
    assert!(after.join_requests.is_empty());
    // Turning someone away leaves the link open.
    assert_eq!(after.invites.len(), 1);
    cat.client.on_wake().await?;
    assert_eq!(asked_code(&cat), Some(cat_code));
    assert!(cat.client.snapshot().parties.is_empty());
    assert_eq!(cat.events.count(EventKind::LetIn), 0);
    // Cat gives up: her request is withdrawn and her phone forgets it.
    cat.client.withdraw_join().await?;
    assert_eq!(cat.client.snapshot().joining, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_name_reaches_the_other_members_screen() -> Result<(), TestError> {
    use pingers_client::snapshot::Name;

    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let ana = person(&harness, &config, 6).await?;
    let ben = person(&harness, &config, 7).await?;
    let party = climbing_crew(&ana).await?;
    let_in(&ana, &ben, party).await?;
    let names = |rig: &Rig| member_names(rig, party);
    assert_eq!(names(&ben)?, ["Ana", "Ben"]);

    // Ana renames herself: her screen shows it at once, Ben's after his next wake.
    ana.client.set_display_name(Name::new("Anna")?).await?;
    assert_eq!(names(&ana)?, ["Anna", "Ben"]);
    ben.client.on_wake().await?;
    assert_eq!(names(&ben)?, ["Anna", "Ben"]);
    // And the other way round.
    ben.client.set_display_name(Name::new("Benjamin")?).await?;
    ana.client.on_wake().await?;
    assert_eq!(names(&ana)?, ["Anna", "Benjamin"]);
    Ok(())
}

/// An OHTTP relay in front of the harness gateway that a test takes down and brings back on
/// the same address, so a client loses the server for a while.
struct SwitchedRelay {
    address: std::net::SocketAddr,
    gateway: std::net::SocketAddr,
    running: Option<RunningRelay>,
}

struct RunningRelay {
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl SwitchedRelay {
    async fn start(harness: &InProcessHarness) -> Result<Self, TestError> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let mut relay = Self {
            address: listener.local_addr()?,
            gateway: harness.endpoints().gateway,
            running: None,
        };
        relay.serve(listener);
        Ok(relay)
    }

    fn serve(&mut self, listener: tokio::net::TcpListener) {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(pingers_testkit::harness::run_ohttp_relay(
            listener,
            self.gateway,
            async {
                drop(stopped.await);
            },
        ));
        self.running = Some(RunningRelay { stop, task });
    }

    async fn down(&mut self) {
        if let Some(running) = self.running.take() {
            // An error only means the relay has stopped already.
            let _: Result<(), ()> = running.stop.send(());
            drop(running.task.await);
        }
    }

    async fn up(&mut self) -> Result<(), TestError> {
        let listener = tokio::net::TcpListener::bind(self.address).await?;
        self.serve(listener);
        Ok(())
    }
}

/// Ana lets Ben in and renames herself while her server is out of reach: the Add commit waits,
/// and so does the profile, which must follow the commit. Back online, one flush sends the
/// commit and then the profile it freed, so Ben joins and sees her new name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_that_waited_for_a_commit_follows_it_in_the_same_flush() -> Result<(), TestError> {
    use pingers_client::ports::{Foreground, JoinDecision};
    use pingers_client::snapshot::Name;

    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let mut relay = SwitchedRelay::start(&harness).await?;
    let ana = person_via(&harness, &config, 6, Some(relay.address)).await?;
    let ben = person(&harness, &config, 7).await?;
    let party = climbing_crew(&ana).await?;
    let link = ana.client.create_invite(party).await?;
    ben.client.open_invite(link.url, Name::new("Ben")?).await?;
    ana.client.on_foreground(Foreground::Visible).await?;
    let request = ana
        .client
        .snapshot()
        .party(party)?
        .join_requests
        .first()
        .ok_or(TestError::Setup("Ben's request"))?
        .id;

    relay.down().await;
    ana.client
        .decide_join(party, request, JoinDecision::Admit)
        .await?;
    ana.client.set_display_name(Name::new("Anna")?).await?;
    ben.client.on_wake().await?;
    assert!(
        ben.client.snapshot().party(party).is_err(),
        "the Add has not reached the server"
    );

    // Back online, a second rename's single flush sends the commit, then the profile.
    relay.up().await?;
    // Long enough for the relay to finish resting after its failures.
    config.clock.advance(std::time::Duration::from_secs(600));
    ana.client.set_display_name(Name::new("Anna B")?).await?;
    // Ben joins on one wake and reads the Party's log on the next.
    ben.client.on_wake().await?;
    ben.client.on_wake().await?;
    let names: Vec<String> = ben
        .client
        .snapshot()
        .party(party)?
        .members
        .iter()
        .map(|member| member.name.as_str().to_owned())
        .collect();
    assert_eq!(names, ["Anna B", "Ben"]);
    Ok(())
}

/// Ana invites Ben into `party` and lets him in; Ben's next wake joins.
async fn let_in(ana: &Rig, ben: &Rig, party: pingers_proto::PartyId) -> Result<(), TestError> {
    use pingers_client::ports::{Foreground, JoinDecision};
    use pingers_client::snapshot::Name;

    let link = ana.client.create_invite(party).await?;
    ben.client.open_invite(link.url, Name::new("Ben")?).await?;
    ana.client.on_foreground(Foreground::Visible).await?;
    let request = ana
        .client
        .snapshot()
        .party(party)?
        .join_requests
        .first()
        .ok_or(TestError::Setup("Ben's request"))?
        .id;
    ana.client
        .decide_join(party, request, JoinDecision::Admit)
        .await?;
    ben.client.on_wake().await?;
    Ok(())
}

/// The names on `rig`'s roster of `party`.
fn member_names(rig: &Rig, party: pingers_proto::PartyId) -> Result<Vec<String>, TestError> {
    Ok(rig
        .client
        .snapshot()
        .party(party)?
        .members
        .iter()
        .map(|member| member.name.as_str().to_owned())
        .collect())
}

/// Ben leaves: his leave request reaches the server, and Ana's next wake commits his removal
/// and drops him from her screen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_who_leaves_disappears_from_the_others_screen() -> Result<(), TestError> {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let ana = person(&harness, &config, 6).await?;
    let ben = person(&harness, &config, 7).await?;
    let party = climbing_crew(&ana).await?;
    let_in(&ana, &ben, party).await?;
    assert_eq!(member_names(&ana, party)?, ["Ana", "Ben"]);

    ben.client.leave_party(party).await?;
    assert!(ben.client.snapshot().party(party).is_err());
    ana.client.on_wake().await?;
    assert_eq!(member_names(&ana, party)?, ["Ana"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_join_without_a_push_token_completes_through_checks_on_screen() -> Result<(), TestError> {
    use pingers_client::ports::JoinDecision;
    use pingers_client::snapshot::{Joining, Name};
    use pingers_testkit::EventKind;

    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let ana = person(&harness, &config, 3).await?;
    up_token(&ana);
    ana.client.set_wake_pushes(WakePushes::On).await?;
    // Ben keeps the default, wake-up pushes on, but the platform has given him no token yet:
    // no push will announce his Welcome.
    let ben = rig_as(
        &harness,
        &config,
        harness_pins(&harness)?,
        Who {
            seed: 4,
            account: Account::Register,
            relay: None,
        },
    )
    .await?;
    let party = climbing_crew(&ana).await?;
    let link = ana.client.create_invite(party).await?;
    ben.client.open_invite(link.url, Name::new("Ben")?).await?;
    let ben_code = asked_code(&ben).ok_or(TestError::Setup("Ben has no code"))?;

    // Ana's screen checks while it is open: the request appears without a push or a new
    // foreground.
    assert!(ana.client.snapshot().party(party)?.join_requests.is_empty());
    ana.client.check_joins().await?;
    let snapshot = ana.client.snapshot();
    let request = snapshot
        .party(party)?
        .join_requests
        .first()
        .ok_or(TestError::Setup("Ben's request"))?;
    assert_eq!(request.code, ben_code);
    ana.client
        .decide_join(party, request.id, JoinDecision::Admit)
        .await?;

    // Ben's Asked to join checks while it is on screen: he is in.
    ben.client.check_joins().await?;
    assert!(matches!(
        ben.client.snapshot().joining,
        Some(Joining::LetIn { party: joined, .. }) if joined == party
    ));
    assert_eq!(ben.events.count(EventKind::LetIn), 1);
    Ok(())
}

/// The longest the server holds a `Quiet` push back (pingers-delivery's `MAX_QUIET_DELAY`).
const QUIET_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// The harness and the clock it shares with the phones.
struct Stack {
    config: HarnessConfig,
    harness: InProcessHarness,
}

impl Stack {
    fn start() -> Result<Self, TestError> {
        let config = HarnessConfig::default();
        let harness = InProcessHarness::start(&config)?;
        Ok(Self { config, harness })
    }

    /// Someone with wake-up pushes on (the default) and, with `token`, the platform's push
    /// token, their own so the harness can tell whose phone a push woke.
    async fn phone(&self, seed: u64, token: Token) -> Result<Phone, TestError> {
        let rig = rig_as(
            &self.harness,
            &self.config,
            harness_pins(&self.harness)?,
            Who {
                seed,
                account: Account::Register,
                relay: None,
            },
        )
        .await?;
        let phone = Phone {
            rig,
            token: PushToken::from_bytes(format!("https://push.harness.invalid/up/{seed}").into()),
        };
        if token == Token::Given {
            phone.give_token();
            phone.rig.client.set_wake_pushes(WakePushes::On).await?;
        }
        Ok(phone)
    }

    /// Lets the server's quiet delay pass until `phone` has had more than `seen` pushes;
    /// whether it did.
    async fn pushed(&self, phone: &Phone, seen: usize) -> bool {
        // The server's delay starts when its push task first runs, which may be after the
        // clock moved: move it again until the push is out, a few times at most.
        for _round in 0..4 {
            self.config.clock.advance(QUIET_DELAY);
            let waited = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while phone.pushes(&self.harness) <= seen {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            if waited.is_ok() {
                return true;
            }
        }
        false
    }

    /// Waits for the Party's push to reach `phone` (it had `seen` before), then hands the core
    /// the handle the push carried, as the push service would.
    async fn woken(
        &self,
        phone: &Phone,
        party: pingers_proto::PartyId,
        seen: usize,
    ) -> Result<(), TestError> {
        if !self.pushed(phone, seen).await {
            return Err(TestError::Setup("no push reached the phone"));
        }
        let handle = phone
            .party_push_handle(party)?
            .ok_or(TestError::Setup("no subscription"))?;
        phone.rig.client.on_push(handle).await?;
        Ok(())
    }
}

/// Whether the platform has given the phone its push token yet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Given,
    NotYet,
}

/// A phone and its UnifiedPush endpoint.
struct Phone {
    rig: Rig,
    token: PushToken,
}

impl Phone {
    fn give_token(&self) {
        self.rig
            .platform
            .set_push_registration(Some(PushRegistration {
                platform: PushPlatform::UnifiedPush,
                token: self.token.clone(),
            }));
    }

    /// How many pushes the relay has sent this phone.
    fn pushes(&self, harness: &InProcessHarness) -> usize {
        harness
            .pushes()
            .iter()
            .filter(|push| push.token == self.token)
            .count()
    }

    /// The handle in this phone's subscription to `party`'s group, if it has one.
    fn party_push_handle(
        &self,
        party: pingers_proto::PartyId,
    ) -> Result<Option<pingers_proto::PushHandle>, TestError> {
        let database = Database::open(
            &self.rig.database,
            DatabaseKey::from_bytes(Zeroizing::new(KEY)),
        )?;
        let session = database.session()?;
        let group = session
            .parties()
            .get(&party)?
            .ok_or(TestError::Setup("no such Party"))?
            .group;
        Ok(session
            .subscriptions()
            .get(&group)?
            .map(|subscription| subscription.handle))
    }

    /// The names of `party`'s Spots on this phone, sorted.
    fn spots(&self, party: pingers_proto::PartyId) -> Result<Vec<String>, TestError> {
        let mut names: Vec<String> = self
            .rig
            .client
            .snapshot()
            .party(party)?
            .spots
            .iter()
            .map(|spot| spot.name.as_str().to_owned())
            .collect();
        names.sort();
        Ok(names)
    }

    /// Saves a Spot, new (`id` `None`) or edited, around the same centre.
    async fn save_spot(
        &self,
        party: pingers_proto::PartyId,
        id: Option<pingers_proto::SpotId>,
        name: &str,
    ) -> Result<pingers_proto::SpotId, TestError> {
        use pingers_geo::{Circle, Coordinate, Fence, Meters};
        let centre = Coordinate::new(51.5007, -0.1246).map_err(|_| TestError::Setup("centre"))?;
        let radius = Meters::new(150.0).map_err(|_| TestError::Setup("radius"))?;
        let circle = Circle::new(centre, radius).map_err(|_| TestError::Setup("circle"))?;
        Ok(self
            .rig
            .client
            .save_spot(
                party,
                pingers_client::ports::SpotDraft {
                    id,
                    name: pingers_client::snapshot::Name::new(name)?,
                    fence: Fence::Circle(circle),
                },
            )
            .await?)
    }
}

/// Ana's Party with Ben let in through flow A (link, code, Admit, Welcome), as on the
/// emulators: Ana sees the request when she opens the app, Ben checks while Asked to join
/// shows.
async fn ana_lets_ben_in(ana: &Phone, ben: &Phone) -> Result<pingers_proto::PartyId, TestError> {
    use pingers_client::ports::{Foreground, JoinDecision};
    use pingers_client::snapshot::{Joining, Name};

    let party = climbing_crew(&ana.rig).await?;
    let link = ana.rig.client.create_invite(party).await?;
    ben.rig
        .client
        .open_invite(link.url, Name::new("Ben")?)
        .await?;
    ana.rig.client.on_foreground(Foreground::Visible).await?;
    let request = ana
        .rig
        .client
        .snapshot()
        .party(party)?
        .join_requests
        .first()
        .map(|request| request.id)
        .ok_or(TestError::Setup("Ben's request"))?;
    ana.rig
        .client
        .decide_join(party, request, JoinDecision::Admit)
        .await?;
    ben.rig.client.check_joins().await?;
    if !matches!(
        ben.rig.client.snapshot().joining,
        Some(Joining::LetIn { .. })
    ) {
        return Err(TestError::Setup("Ben was not let in"));
    }
    ben.rig.client.dismiss_join().await?;
    Ok(party)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spots_reach_the_other_member_through_the_party_push() -> Result<(), TestError> {
    let stack = Stack::start()?;
    let ana = stack.phone(3, Token::Given).await?;
    let ben = stack.phone(4, Token::Given).await?;
    let party = ana_lets_ben_in(&ana, &ben).await?;

    // Ana makes a Spot; the Party's push wakes Ben's phone and the Spot shows.
    let seen = ben.pushes(&stack.harness);
    let crag = ana.save_spot(party, None, "Crag").await?;
    stack.woken(&ben, party, seen).await?;
    assert_eq!(ben.spots(party)?, ["Crag"]);

    // Ana renames it: the edit reaches Ben the same way.
    let seen = ben.pushes(&stack.harness);
    ana.save_spot(party, Some(crag), "North crag").await?;
    stack.woken(&ben, party, seen).await?;
    assert_eq!(ben.spots(party)?, ["North crag"]);

    // Ben makes one of his own: Ana, who made the Party, is woken for it too.
    let seen = ana.pushes(&stack.harness);
    ben.save_spot(party, None, "Boulders").await?;
    stack.woken(&ana, party, seen).await?;
    assert_eq!(ana.spots(party)?, ["Boulders", "North crag"]);

    // Ana deletes hers: it goes from Ben's phone.
    let seen = ben.pushes(&stack.harness);
    ana.rig.client.delete_spot(party, crag).await?;
    stack.woken(&ben, party, seen).await?;
    assert_eq!(ben.spots(party)?, ["Boulders"]);
    Ok(())
}

/// The emulators' order of events: both phones made and joined the Party before ntfy gave
/// them an endpoint. The token's arrival subscribes each to the Party.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_token_that_comes_late_subscribes_to_the_parties() -> Result<(), TestError> {
    let stack = Stack::start()?;
    let ana = stack.phone(3, Token::NotYet).await?;
    let ben = stack.phone(4, Token::NotYet).await?;
    let party = ana_lets_ben_in(&ana, &ben).await?;
    assert_eq!(ana.party_push_handle(party)?, None);
    assert_eq!(ben.party_push_handle(party)?, None);

    for phone in [&ana, &ben] {
        phone.give_token();
        phone.rig.client.push_tokens_changed().await?;
        assert!(phone.party_push_handle(party)?.is_some());
    }
    let seen = ben.pushes(&stack.harness);
    ana.save_spot(party, None, "Crag").await?;
    stack.woken(&ben, party, seen).await?;
    assert_eq!(ben.spots(party)?, ["Crag"]);

    let seen = ana.pushes(&stack.harness);
    ben.save_spot(party, None, "Boulders").await?;
    stack.woken(&ana, party, seen).await?;
    assert_eq!(ana.spots(party)?, ["Boulders", "Crag"]);
    Ok(())
}

/// A push that never came (the phone was off, or the push relay dropped it): opening the app
/// reads the Party log, with wake-up pushes on and nothing to upload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_the_app_shows_a_spot_whose_push_never_came() -> Result<(), TestError> {
    use pingers_client::ports::Foreground;

    let stack = Stack::start()?;
    let ana = stack.phone(3, Token::Given).await?;
    let ben = stack.phone(4, Token::Given).await?;
    let party = ana_lets_ben_in(&ana, &ben).await?;
    ben.rig.client.on_foreground(Foreground::Visible).await?;
    ben.rig.client.on_foreground(Foreground::Hidden).await?;

    ana.save_spot(party, None, "Crag").await?;
    assert!(ben.spots(party)?.is_empty());
    ben.rig.client.on_foreground(Foreground::Visible).await?;
    assert_eq!(ben.spots(party)?, ["Crag"]);
    Ok(())
}

/// "No wake-up pushes" deletes the Party subscription: the server no longer wakes the phone
/// for the Party, and the Spot shows when the app is next opened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turning_wake_up_pushes_off_ends_the_party_s_pushes() -> Result<(), TestError> {
    use pingers_client::ports::Foreground;

    let stack = Stack::start()?;
    let ana = stack.phone(3, Token::Given).await?;
    let ben = stack.phone(4, Token::Given).await?;
    let party = ana_lets_ben_in(&ana, &ben).await?;
    ben.rig
        .client
        .set_wake_pushes(WakePushes::NoWakePushes)
        .await?;
    assert_eq!(ben.party_push_handle(party)?, None);

    let seen = ben.pushes(&stack.harness);
    ana.save_spot(party, None, "Crag").await?;
    assert!(!stack.pushed(&ben, seen).await);
    ben.rig.client.on_foreground(Foreground::Visible).await?;
    assert_eq!(ben.spots(party)?, ["Crag"]);
    Ok(())
}

/// "No wake-up pushes": nothing wakes Ben for the Party, and his next scheduled wake reads
/// the Party log and shows the Spot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_wake_up_pushes_the_next_wake_shows_the_spot() -> Result<(), TestError> {
    let stack = Stack::start()?;
    let ana = stack.phone(3, Token::Given).await?;
    let ben = stack.phone(4, Token::NotYet).await?;
    ben.rig
        .client
        .set_wake_pushes(WakePushes::NoWakePushes)
        .await?;
    let party = ana_lets_ben_in(&ana, &ben).await?;
    assert_eq!(ben.party_push_handle(party)?, None);

    ana.save_spot(party, None, "Crag").await?;
    assert!(ben.spots(party)?.is_empty());
    ben.rig.client.on_wake().await?;
    assert_eq!(ben.spots(party)?, ["Crag"]);
    Ok(())
}
