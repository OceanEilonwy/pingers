// SPDX-License-Identifier: AGPL-3.0-only
//! The main app's RLN path against the real server: the account, its leaf, the prover
//! self-test, the membership tree kept in step with the hourly roots (new and zeroed leaves),
//! real proofs the server verifies, a reused nullifier refused, a removed leaf replaced, and
//! the fallbacks to Privacy Pass the design names.

use std::time::Duration;

use pingers_anon::RlnSecret;
use pingers_ohttp::{BinaryRequest, Header};
use pingers_proto::{
    MembershipLeafRequest, MembershipLeafResponse, Redacted, RlnAuth, RlnScope, WireDecode as _,
    WireEncode as _,
};
use pingers_store::{ProverVerdict, SelfTestRecord};
use pingers_sync::{
    CredentialChoice, Delivered, ProcessRole, Refill, RlnReadiness, Route, SelfTestStanding,
    SelfTestStart, SyncError, WakeBudget,
};
use pingers_testkit::{HarnessConfig, seeded_rng};
use pingers_time::{Clock as _, UnixSeconds};

use crate::mailbox::knock_mailbox;
use crate::support::{Device, TestError, TestResult, World, real_pause};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Decodes padded base64url, as the server will.
fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0_u32;
    let mut count = 0_u32;
    let mut bytes = Vec::new();
    for symbol in text.trim_end_matches('=').bytes() {
        let value = ALPHABET.iter().position(|candidate| *candidate == symbol)?;
        bits = (bits << 6) | u32::try_from(value).ok()?;
        count = count.saturating_add(6);
        if count >= 8 {
            count = count.saturating_sub(8);
            bytes.push(u8::try_from((bits >> count) & 0xff).ok()?);
        }
    }
    Some(bytes)
}

/// Moves the clock to the start of the next hourly root epoch.
fn next_hour(world: &World) {
    let now = world.clock.now().get();
    let next = now
        .checked_div(3_600)
        .and_then(|hour| hour.checked_add(1))
        .and_then(|hour| hour.checked_mul(3_600))
        .unwrap_or(now);
    world
        .clock
        .advance(Duration::from_secs(next.saturating_sub(now)));
}

/// How long the background self-test may take in real time: its two proofs share the CPU with
/// every other harness test proving at the same time.
const SELF_TEST_WAIT: Duration = Duration::from_secs(180);

/// Waits (in real time) for the background prover self-test to record its result.
async fn tested(device: &Device) -> Result<SelfTestStanding, TestError> {
    tokio::time::timeout(SELF_TEST_WAIT, async {
        loop {
            let standing = device.engine.prover_standing().await?;
            if standing != SelfTestStanding::Due {
                return Ok::<_, TestError>(standing);
            }
            real_pause().await;
        }
    })
    .await?
}

/// An account device that registered, took a leaf, passed its self-test and, an hour later,
/// refreshed its tree into the root holding its leaf: ready to prove.
async fn proving_device(world: &World) -> Result<Device, TestError> {
    let device = world.account_device()?;
    let first = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?;
    assert!(matches!(first.tokens, Refill::Fetched { .. }));
    assert!(matches!(
        tested(&device).await?,
        SelfTestStanding::Passed { .. }
    ));
    assert_eq!(
        device.engine.rln_readiness().await?,
        RlnReadiness::NotLiveYet
    );
    assert_eq!(
        device.engine.credential_choice().await?,
        CredentialChoice::PrivacyPass,
        "a leaf not yet in a root uses tokens (design B.4.8 trigger 1)"
    );
    next_hour(world);
    assert_eq!(
        device.engine.rln_readiness().await?,
        RlnReadiness::TreeStale
    );
    let second = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?;
    let refresh = second
        .membership
        .ok_or(TestError::Unexpected("no tree refresh in the new hour"))?;
    assert_eq!(refresh.added, 1);
    assert_eq!(device.engine.rln_readiness().await?, RlnReadiness::Ready);
    assert_eq!(
        device.engine.credential_choice().await?,
        CredentialChoice::MembershipProof
    );
    Ok(device)
}

/// The RLN header of the last request to `path` this device sent.
fn last_proof(device: &Device, path: &str) -> Result<RlnAuth, TestError> {
    let seen = device.link.seen();
    let header = seen
        .iter()
        .rev()
        .find(|seen| seen.path == path)
        .and_then(|seen| seen.authorization.as_deref())
        .and_then(|value| value.strip_prefix("PingersRln "))
        .ok_or(TestError::Unexpected("no RLN header"))?;
    Ok(RlnAuth::decode(
        &base64url_decode(header).ok_or(TestError::Unexpected("not base64url"))?,
    )?)
}

/// The authorization scheme of the last request this device sent.
fn last_scheme(device: &Device) -> Option<String> {
    device
        .link
        .seen()
        .iter()
        .rev()
        .find_map(|seen| seen.authorization.clone())
        .and_then(|value| value.split(' ').next().map(str::to_owned))
}

/// `prepare_credentials` in a new hour: the tree refresh it made.
async fn refresh(device: &Device) -> Result<pingers_sync::MembershipRefresh, TestError> {
    device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?
        .membership
        .ok_or(TestError::Unexpected("no refresh"))
}

/// Registers a leaf for an account by hand: `POST /v1/rln/leaf` with `bearer`.
async fn register_leaf(
    world: &World,
    bearer: String,
    seed: u64,
) -> Result<MembershipLeafResponse, TestError> {
    let secret = RlnSecret::generate(&mut seeded_rng(seed))?;
    let answer = world
        .account
        .send(&BinaryRequest {
            method: "POST".to_owned(),
            path: "/v1/rln/leaf".to_owned(),
            headers: vec![Header {
                name: "authorization".to_owned(),
                value: bearer.into_bytes(),
            }],
            body: MembershipLeafRequest {
                id_commitment: Redacted::new(*secret.id_commitment()?.as_bytes()),
            }
            .encode()?,
        })
        .await?;
    assert_eq!(answer.status.get(), 200);
    Ok(MembershipLeafResponse::decode(&answer.body)?)
}

async fn register_mailbox(device: &Device) -> Result<(), TestError> {
    let registered = device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await?;
    assert!(matches!(registered, Delivered::Done(_)));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_main_app_proves_and_the_real_server_accepts_it() -> TestResult {
    let world = World::start()?;
    let device = proving_device(&world).await?;
    let tokens = device.engine.token_count().await?;
    register_mailbox(&device).await?;
    assert_eq!(device.engine.token_count().await?, tokens, "no token spent");
    let auth = last_proof(&device, "/v1/mailboxes")?;
    assert_eq!(auth.scope, RlnScope::Mailbox);
    assert_eq!(auth.bucket, world.clock.now().bucket());
    // The proof binds exactly the request that went out.
    let sent = device
        .link
        .seen()
        .into_iter()
        .rev()
        .find(|seen| seen.path == "/v1/mailboxes")
        .ok_or(TestError::Unexpected("no registration seen"))?;
    assert_eq!(
        auth.x,
        *pingers_anon::request_digest(&sent.method, &sent.path, &sent.body)?.as_bytes()
    );
    // The message id was reserved before sending: the bucket has seven left.
    let mut left = 0;
    while device
        .scoped()?
        .server()?
        .rln_counters()
        .reserve(RlnScope::Mailbox, world.clock.now().bucket())?
        .is_some()
    {
        left += 1;
    }
    assert_eq!(left, pingers_anon::MESSAGE_LIMIT.saturating_sub(1));
    // With the bucket's ids spent, a token is still accepted.
    register_mailbox(&device).await?;
    assert_eq!(device.engine.token_count().await?, tokens.saturating_sub(1));
    assert_eq!(last_scheme(&device).as_deref(), Some("PrivateToken"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reused_nullifier_is_refused_with_403() -> TestResult {
    let world = World::start()?;
    let device = proving_device(&world).await?;
    register_mailbox(&device).await?;
    let first = last_proof(&device, "/v1/mailboxes")?;
    // A restored backup forgets the spent message id: the next request proves with the same
    // slot for another body, so the same nullifier with a different `x`.
    let bucket = world.clock.now().bucket();
    let after = pingers_time::Bucket::containing(UnixSeconds::new(
        world.clock.now().get().saturating_add(900),
    ));
    assert!(after > bucket);
    device
        .scoped()?
        .server()?
        .rln_counters()
        .delete_before(after)?;
    let reused = device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await;
    assert!(
        matches!(
            reused,
            Err(SyncError::Status {
                route: Route::CreateMailbox,
                status: 403
            })
        ),
        "{reused:?}"
    );
    let second = last_proof(&device, "/v1/mailboxes")?;
    assert_eq!(second.nullifier, first.nullifier);
    assert_ne!(second.x, first.x);
    // The next slot is a new nullifier and is accepted.
    register_mailbox(&device).await?;
    assert_ne!(
        last_proof(&device, "/v1/mailboxes")?.nullifier,
        first.nullifier
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tree_follows_new_and_zeroed_leaves_hour_by_hour() -> TestResult {
    let world = World::start()?;
    let device = proving_device(&world).await?;
    // Another account takes a leaf: in the next root, the copy grows and still proves.
    let _other = register_leaf(&world, world.account.bearer().await?, 40).await?;
    register_mailbox(&device).await?;
    next_hour(&world);
    let grown = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?
        .membership
        .ok_or(TestError::Unexpected("no refresh"))?;
    assert_eq!((grown.added, grown.zeroed), (1, 0));
    register_mailbox(&device).await?;
    assert_eq!(last_scheme(&device).as_deref(), Some("PingersRln"));
    // That account replaces its leaf: the old one is zeroed in the next root.
    let _replacement = register_leaf(&world, world.account.bearer().await?, 41).await?;
    next_hour(&world);
    let replaced = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?
        .membership
        .ok_or(TestError::Unexpected("no refresh"))?;
    assert_eq!((replaced.added, replaced.zeroed), (1, 1));
    // The server verifies against its root: the copy's root (and this device's path in it)
    // matches it after the zeroing.
    register_mailbox(&device).await?;
    assert_eq!(last_scheme(&device).as_deref(), Some("PingersRln"));
    // Within the hour nothing is fetched again.
    let quiet = device.link.seen().len();
    assert!(
        device
            .engine
            .prepare_credentials(WakeBudget::UNLIMITED)
            .await?
            .membership
            .is_none()
    );
    assert_eq!(device.link.seen().len(), quiet);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_process_rebuilds_the_tree_from_its_stored_leaves() -> TestResult {
    let world = World::start()?;
    let device = proving_device(&world).await?;
    // Another account's leaf arrives and is zeroed a root later.
    let _other = register_leaf(&world, world.account.bearer().await?, 42).await?;
    next_hour(&world);
    let grown = refresh(&device).await?;
    assert_eq!((grown.added, grown.zeroed), (1, 0));
    let _replacement = register_leaf(&world, world.account.bearer().await?, 43).await?;
    // A new process: the in-memory tree is gone, the stored leaves and path remain, and a
    // proof needs only those.
    let device = world.restart(device)?;
    register_mailbox(&device).await?;
    assert_eq!(last_scheme(&device).as_deref(), Some("PingersRln"));
    next_hour(&world);
    let rebuilt = refresh(&device).await?;
    assert_eq!(
        (rebuilt.added, rebuilt.zeroed),
        (1, 1),
        "rebuilt from the store, only the new zeroing counts"
    );
    register_mailbox(&device).await?;
    assert_eq!(last_scheme(&device).as_deref(), Some("PingersRln"));
    next_hour(&world);
    let quiet = refresh(&device).await?;
    assert_eq!((quiet.added, quiet.zeroed), (0, 0));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tree_two_hours_old_is_refreshed_before_proving_again() -> TestResult {
    let world = World::start()?;
    let device = proving_device(&world).await?;
    next_hour(&world);
    // The previous root is still accepted.
    assert_eq!(device.engine.rln_readiness().await?, RlnReadiness::Ready);
    next_hour(&world);
    assert_eq!(
        device.engine.rln_readiness().await?,
        RlnReadiness::TreeStale
    );
    assert_eq!(
        device.engine.credential_choice().await?,
        CredentialChoice::PrivacyPass
    );
    assert!(
        device
            .engine
            .ensure_membership_fresh(WakeBudget::UNLIMITED)
            .await?
            .is_some()
    );
    register_mailbox(&device).await?;
    assert_eq!(last_scheme(&device).as_deref(), Some("PingersRln"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_leaf_falls_back_to_tokens_and_is_replaced() -> TestResult {
    let world = World::start()?;
    let device = proving_device(&world).await?;
    // The account's leaf is replaced behind this device's back (as after a reinstall that
    // kept the account): the server zeroes the device's leaf at the next root.
    let stored = device
        .scoped()?
        .server()?
        .account()
        .bearer()?
        .ok_or(TestError::Unexpected("no bearer"))?;
    let _foreign = register_leaf(&world, format!("Bearer {}", stored.bearer.expose()), 50).await?;
    next_hour(&world);
    let prepared = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?;
    let refresh = prepared
        .membership
        .ok_or(TestError::Unexpected("no refresh"))?;
    assert_eq!(refresh.zeroed, 1);
    assert_eq!(
        prepared.leaf,
        pingers_sync::LeafStanding::Registered,
        "a zeroed leaf is replaced with a fresh secret"
    );
    assert_eq!(
        device.engine.rln_readiness().await?,
        RlnReadiness::NotLiveYet
    );
    register_mailbox(&device).await?;
    assert_eq!(last_scheme(&device).as_deref(), Some("PrivateToken"));
    next_hour(&world);
    let _ = device
        .engine
        .prepare_credentials(WakeBudget::UNLIMITED)
        .await?;
    register_mailbox(&device).await?;
    assert_eq!(last_scheme(&device).as_deref(), Some("PingersRln"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wake_without_time_for_a_proof_spends_a_token() -> TestResult {
    let world = World::start()?;
    let device = proving_device(&world).await?;
    // This device's proofs take up to 3 s (its self-test's 95th percentile).
    device.session()?.prover_self_test().save(&SelfTestRecord {
        parameters: pingers_anon::PARAMETERS_ID.to_owned(),
        verdict: ProverVerdict::Passed,
        first: Duration::from_secs(4),
        median: Duration::from_secs(2),
        p95: Duration::from_secs(3),
        timed: 20,
        tested_at: world.clock.now(),
    })?;
    let tokens = device.engine.token_count().await?;
    // 4 s left: a proof (3 s) and the request (2 s) do not fit.
    let short = WakeBudget::from_now(&world.clock, Duration::from_secs(4));
    let registered = device
        .engine
        .register_mailbox(&knock_mailbox()?, short)
        .await?;
    assert!(matches!(registered, Delivered::Done(_)));
    assert_eq!(last_scheme(&device).as_deref(), Some("PrivateToken"));
    assert_eq!(device.engine.token_count().await?, tokens.saturating_sub(1));
    // 5 s left: they do.
    let enough = WakeBudget::from_now(&world.clock, Duration::from_secs(5));
    let registered = device
        .engine
        .register_mailbox(&knock_mailbox()?, enough)
        .await?;
    assert!(matches!(registered, Delivered::Done(_)));
    assert_eq!(last_scheme(&device).as_deref(), Some("PingersRln"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_self_test_keeps_the_main_app_on_tokens() -> TestResult {
    let world = World::start()?;
    let device = proving_device(&world).await?;
    device.session()?.prover_self_test().save(&SelfTestRecord {
        parameters: pingers_anon::PARAMETERS_ID.to_owned(),
        verdict: ProverVerdict::TooSlow,
        first: Duration::from_secs(4),
        median: Duration::from_secs(2),
        p95: Duration::from_secs(2),
        timed: 20,
        tested_at: world.clock.now(),
    })?;
    assert_eq!(device.engine.rln_readiness().await?, RlnReadiness::Ready);
    assert_eq!(
        device.engine.credential_choice().await?,
        CredentialChoice::PrivacyPass
    );
    // A result stands: no test starts again within the week.
    assert_eq!(
        device.engine.start_prover_self_test().await?,
        SelfTestStart::NotDue
    );
    assert_eq!(
        device.engine.prover_standing().await?,
        SelfTestStanding::Failed
    );
    register_mailbox(&device).await?;
    assert_eq!(last_scheme(&device).as_deref(), Some("PrivateToken"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_who_turns_proofs_off_gets_tokens() -> TestResult {
    let world = World::start_with(&HarnessConfig {
        membership_proofs: pingers_testkit::MembershipProofs::Off,
        ..HarnessConfig::default()
    })?;
    let device = proving_device(&world).await?;
    // Reading the parameters switches the device to tokens before anything is refused.
    let params = device.engine.params(WakeBudget::UNLIMITED).await?;
    assert_eq!(
        params.anonymous_credentials,
        [pingers_proto::AnonymousCredential::PrivacyPass]
    );
    assert_eq!(
        device.engine.credential_choice().await?,
        CredentialChoice::PrivacyPass
    );
    register_mailbox(&device).await?;
    assert_eq!(last_scheme(&device).as_deref(), Some("PrivateToken"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_self_test_runs_at_a_time_and_only_in_the_main_app() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    assert_eq!(
        device.engine.start_prover_self_test().await?,
        SelfTestStart::Started
    );
    // Two proofs take far longer than this call: the first test is still running.
    assert_eq!(
        device.engine.start_prover_self_test().await?,
        SelfTestStart::AlreadyRunning
    );
    assert!(matches!(
        tested(&device).await?,
        SelfTestStanding::Passed { .. }
    ));
    assert_eq!(
        device.engine.start_prover_self_test().await?,
        SelfTestStart::NotDue
    );
    let address = world.harness.endpoints().ohttp_relay.to_string();
    let extension = world.device_with(&address, ProcessRole::Extension)?;
    assert_eq!(
        extension.engine.start_prover_self_test().await?,
        SelfTestStart::NotDue
    );
    Ok(())
}
