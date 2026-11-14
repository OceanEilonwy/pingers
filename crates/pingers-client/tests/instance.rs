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
async fn a_join_with_pushes_on_needs_a_token_and_every_join_a_real_link() -> Result<(), TestError> {
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
    rig_as(harness, config, pins, 3).await
}

/// A client whose account key comes from `seed`, so two rigs are two people.
async fn rig_as(
    harness: &InProcessHarness,
    config: &HarnessConfig,
    pins: Pins,
    seed: u64,
) -> Result<Rig, TestError> {
    let dir = TestDir::new()?;
    let endpoints = harness.endpoints();
    let server = ServerId::new(endpoints.server_id.clone())?;
    let database_path = dir.0.join("pingers.db");
    {
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
    let instance = Instance {
        server,
        relay: format!("http://{}", endpoints.ohttp_relay),
        push_relay: Some(format!("https://{}/", endpoints.push_relay)),
        gateway_pins: Some(pins.gateway),
        push_relay_pins: pins.push_relay,
        trust: Trust::Mozilla,
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
    // Nothing has asked for the parameters yet, and none were recorded.
    assert_eq!(rig.client.snapshot().map_host, None);

    rig.platform.set_push_registration(Some(PushRegistration {
        platform: PushPlatform::UnifiedPush,
        token: PushToken::from_bytes(b"https://push.harness.invalid/up/abc".to_vec()),
    }));
    rig.client.set_wake_pushes(WakePushes::On).await?;
    assert_eq!(rig.client.snapshot().settings.wake_pushes, WakePushes::On);
    // The key matched the build's pins: nothing is recorded as a first-use pin.
    assert_eq!(recorded_pin(&rig)?, None);
    rig.client.push_tokens_changed().await?;
    // Fetching the push relay's id read /v1/params, which carries the map host.
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

/// Tokens fetched for each test client.
const TOKENS: u16 = 120;

/// The credential bypass until the client registers its own account (agent r-accounts): an
/// account for the rig's key is registered over the identified API, a batch of Privacy Pass
/// tokens is issued to it, and the tokens are put straight into the client's store, where
/// pingers-sync takes one per credentialed request.
async fn stock_tokens(
    harness: &InProcessHarness,
    config: &HarnessConfig,
    rig: &Rig,
    seed: u64,
) -> Result<(), TestError> {
    use pingers_proto::{
        AuthRequest, AuthResponse, PrivacyPassIssueRequest, PrivacyPassIssueResponse,
        PrivacyPassKeys, RegisterRequest, RegisterResponse, WireDecode as _, WireEncode as _,
    };

    let endpoints = harness.endpoints();
    let server = ServerId::new(endpoints.server_id.clone())?;
    let key = AccountSigningKey::generate(&mut ChaCha20Rng::seed_from_u64(seed));
    let call = |method: &str, path: &str, authorization: Option<String>, body: Vec<u8>| {
        let mut headers = vec![pingers_ohttp::Header {
            name: "content-type".to_owned(),
            value: b"application/cbor".to_vec(),
        }];
        if let Some(value) = authorization {
            headers.push(pingers_ohttp::Header {
                name: "authorization".to_owned(),
                value: value.into_bytes(),
            });
        }
        let request = pingers_ohttp::BinaryRequest {
            method: method.to_owned(),
            path: path.to_owned(),
            headers,
            body,
        };
        async move {
            let response = pingers_testkit::send_direct(endpoints.server, &request).await?;
            if response.status.get() == 200 {
                Ok(response.body)
            } else {
                Err(TestError::Setup("an identified request was refused"))
            }
        }
    };
    let registration = RegisterRequest {
        signature_key: *key.public_key().as_bytes(),
        proof: *pingers_crypto::sign_registration(&key, &server)
            .map_err(|_| TestError::Setup("registration proof"))?
            .as_bytes(),
        registration_code: None,
    };
    let registered = call("POST", "/v1/accounts", None, registration.encode()?).await?;
    let challenge = pingers_crypto::sign_auth_challenge(
        &key,
        &server,
        pingers_time::Clock::now(&config.clock).bucket(),
    )
    .map_err(|_| TestError::Setup("auth challenge"))?;
    let auth = AuthRequest {
        account_id: RegisterResponse::decode(&registered)?.account_id,
        nonce_signature: *challenge.as_bytes(),
    };
    let authed = call("POST", "/v1/auth", None, auth.encode()?).await?;
    let bearer = format!("Bearer {}", AuthResponse::decode(&authed)?.bearer.expose());
    let keys = PrivacyPassKeys::decode(&call("GET", "/v1/pp/keys", None, Vec::new()).await?)?;
    let issuer = keys
        .keys
        .last()
        .ok_or(TestError::Setup("the server has no issuer key"))?;
    let mut rng = ChaCha20Rng::seed_from_u64(seed.wrapping_add(1_000));
    let pending = pingers_anon::begin_token_issuance(
        &issuer.public_key,
        pingers_anon::TokenCount::new(TOKENS).map_err(|_| TestError::Setup("token count"))?,
        &mut rng,
    )
    .map_err(|_| TestError::Setup("token request"))?;
    let request = PrivacyPassIssueRequest::new(pending.request().to_vec())?;
    let batch = call("POST", "/v1/pp/issue", Some(bearer), request.encode()?).await?;
    let tokens: Vec<pingers_store::UnspentToken> = pending
        .finish(PrivacyPassIssueResponse::decode(&batch)?.token_response())
        .map_err(|_| TestError::Setup("token response"))?
        .iter()
        .map(|token| pingers_store::UnspentToken::from_bytes(token.as_bytes().to_vec()))
        .collect();
    let database = Database::open(&rig.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
    database.session()?.privacy_pass_tokens().add(
        &tokens,
        &pingers_store::TokenKeyId::from_bytes(issuer.key_id),
    )?;
    Ok(())
}

/// A client with its own account and a stock of tokens, with wake-up pushes off: joining no
/// longer needs them, and every wake then reads the mailboxes joins wait on.
async fn person(
    harness: &InProcessHarness,
    config: &HarnessConfig,
    seed: u64,
) -> Result<Rig, TestError> {
    let rig = rig_as(harness, config, harness_pins(harness)?, seed).await?;
    stock_tokens(harness, config, &rig, seed).await?;
    rig.client.set_wake_pushes(WakePushes::NoWakePushes).await?;
    Ok(rig)
}

fn asked_code(rig: &Rig) -> Option<pingers_client::snapshot::SafetyCode> {
    match rig.client.snapshot().joining.clone() {
        Some(pingers_client::snapshot::Joining::Asked { code, .. }) => Some(code),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_compare_codes_let_in_then_turn_the_next_one_away() -> Result<(), TestError> {
    use pingers_client::ports::{JoinDecision, PartyDraft};
    use pingers_client::snapshot::{Joining, Name, PartyColour, PostQuantum};
    use pingers_testkit::{EventKind, RecordedEvent};

    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let ana = person(&harness, &config, 3).await?;
    let ben = person(&harness, &config, 4).await?;
    let party = ana
        .client
        .create_party(PartyDraft {
            name: Name::new("Climbing crew")?,
            my_name: Name::new("Ana")?,
            colour: PartyColour::Teal,
            post_quantum: PostQuantum::Off,
        })
        .await?;
    let link = ana.client.create_invite(party).await?;

    // Ben asks to join; his phone shows the code at once, and nothing about the Party.
    ben.client.open_invite(link.url, Name::new("Ben")?).await?;
    let ben_code = asked_code(&ben).ok_or(TestError::Setup("Ben has no code"))?;
    assert_eq!(ben_code.as_str().len(), 14);
    assert!(ben.client.snapshot().parties.is_empty());

    // Ana's next wake reads the request: same code, before anyone is let in.
    ana.client.on_wake().await?;
    let snapshot = ana.client.snapshot();
    let requests = &snapshot.party(party)?.join_requests;
    assert_eq!(requests.len(), 1);
    let request = requests.first().ok_or(TestError::Setup("Ben's request"))?;
    assert_eq!(request.name.as_str(), "Ben");
    assert_eq!(request.code, ben_code);
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
    ana.client.on_wake().await?;
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
