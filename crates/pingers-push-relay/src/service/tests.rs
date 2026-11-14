// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use pingers_crypto::PushBind;
use pingers_proto::{
    PushClass, PushHandle, PushRelayKeyId, PushToken, PushTokenPlaintext, SealedPushBlob,
    TimeToLive, Urgency, WireEncode as _,
};
use pingers_testkit::ManualClock;
use pingers_time::UnixSeconds;

use super::*;
use crate::credentials::tests::{CREDENTIAL_A, CREDENTIAL_B, entry, hash_key};
use crate::delivery::Priority;
use crate::keys::tests::key;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;
const BIND: [u8; 32] = [0xb1; 32];
const HANDLE: [u8; 8] = [7; 8];

/// Opens blobs registered with [`FakeOpener::seal`]: a blob opens only with its own bind.
#[derive(Default)]
struct FakeOpener {
    sealed: HashMap<([u8; 32], [u8; 320]), Result<PushTokenPlaintext, OpenError>>,
}

impl FakeOpener {
    fn seal(
        &mut self,
        blob_byte: u8,
        result: Result<PushTokenPlaintext, OpenError>,
    ) -> SealedPushBlob {
        let blob = [blob_byte; 320];
        self.sealed.insert((BIND, blob), result);
        SealedPushBlob::from_bytes(blob)
    }
}

impl TokenOpener for FakeOpener {
    fn open(
        &self,
        _key: &crate::keys::RelayKey,
        bind: &PushBind,
        blob: &SealedPushBlob,
    ) -> Result<PushTokenPlaintext, OpenError> {
        self.sealed
            .get(&(*bind.as_bytes(), *blob.as_bytes()))
            .cloned()
            .unwrap_or(Err(OpenError::DoesNotOpen))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Sent {
    platform: PushPlatform,
    token: PushToken,
    message: PushMessage,
}

struct FakeSender {
    supported: Vec<PushPlatform>,
    outcome: DeliveryOutcome,
    sent: Arc<Mutex<Vec<Sent>>>,
}

impl PushSender for FakeSender {
    fn supports(&self, platform: PushPlatform) -> bool {
        self.supported.contains(&platform)
    }

    fn send(
        &self,
        platform: PushPlatform,
        token: &PushToken,
        message: &PushMessage,
    ) -> impl Future<Output = DeliveryOutcome> + Send {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Sent {
                platform,
                token: token.clone(),
                message: *message,
            });
        std::future::ready(self.outcome)
    }
}

fn plaintext(
    platform: PushPlatform,
    class: PushClass,
    token: &str,
    not_after: u64,
) -> PushTokenPlaintext {
    PushTokenPlaintext {
        platform,
        class,
        token: PushToken::from_bytes(token.as_bytes().to_vec()),
        not_after: UnixSeconds::new(not_after),
    }
}

fn per_minute(count: u32) -> RateLimit {
    RateLimit::per_minute(NonZeroU32::new(count).unwrap_or(NonZeroU32::MIN))
}

struct Harness {
    relay: Relay<FakeOpener, FakeSender, ManualClock>,
    clock: ManualClock,
    sent: Arc<Mutex<Vec<Sent>>>,
}

struct Setup {
    opener: FakeOpener,
    outcome: DeliveryOutcome,
    supported: Vec<PushPlatform>,
    server_a_per_minute: u32,
    token_per_minute: u32,
    /// When key 3 activates, if it is configured.
    key_3_activates_at: Option<u64>,
}

impl Setup {
    fn new(opener: FakeOpener) -> Self {
        Self {
            opener,
            outcome: DeliveryOutcome::Delivered,
            supported: vec![PushPlatform::UnifiedPush, PushPlatform::Fcm],
            server_a_per_minute: 1000,
            token_per_minute: 1000,
            key_3_activates_at: None,
        }
    }

    fn build(self) -> Result<Harness, Box<dyn std::error::Error>> {
        let clock = ManualClock::new(UnixSeconds::new(NOW));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let relay = Relay::new(RelayParts {
            opener: self.opener,
            sender: FakeSender {
                supported: self.supported,
                outcome: self.outcome,
                sent: Arc::clone(&sent),
            },
            clock: clock.clone(),
            // Key 2 took over a day ago; key 1 opens for the rest of the 30-day overlap.
            keys: KeyRing::new(
                [
                    Some(key(1, 0, Some(NOW + 29 * DAY))),
                    Some(key(2, NOW - DAY, None)),
                    self.key_3_activates_at.map(|at| key(3, at, None)),
                ]
                .into_iter()
                .flatten()
                .collect(),
            )?,
            servers: ServerRegistry::new(
                hash_key(),
                vec![
                    entry("a", CREDENTIAL_A, self.server_a_per_minute),
                    entry("b", CREDENTIAL_B, 1000),
                ],
            )?,
            token_hash_key: hash_key(),
            token_limit: per_minute(self.token_per_minute),
            max_ttl: TimeToLive::new(Duration::from_secs(30 * DAY))?,
            operator: "Test Relay Ltd".to_owned(),
        });
        Ok(Harness { relay, clock, sent })
    }
}

fn body(key_id: u8, blob: &SealedPushBlob, bind: [u8; 32], urgency: Urgency) -> Vec<u8> {
    PushRequest {
        key_id: PushRelayKeyId::new(key_id),
        blob: *blob,
        bind: PushBind::from_bytes(bind),
        urgency,
        handle: PushHandle::from_bytes(HANDLE),
    }
    .encode()
    .unwrap_or_default()
}

impl Harness {
    async fn push_as(&self, credential: &str, body: &[u8]) -> PushOutcome {
        self.relay.push(Some(credential.as_bytes()), body).await
    }

    async fn push(&self, body: &[u8]) -> PushOutcome {
        self.push_as(CREDENTIAL_A, body).await
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn unifiedpush_blob(opener: &mut FakeOpener, byte: u8, class: PushClass) -> SealedPushBlob {
    opener.seal(
        byte,
        Ok(plaintext(
            PushPlatform::UnifiedPush,
            class,
            "https://ntfy.example/up1",
            NOW + DAY,
        )),
    )
}

#[tokio::test]
async fn a_valid_request_sends_one_push_with_the_handle() -> TestResult {
    let mut opener = FakeOpener::default();
    let blob = unifiedpush_blob(&mut opener, 1, PushClass::Standard);
    let harness = Setup::new(opener).build()?;
    let outcome = harness.push(&body(2, &blob, BIND, Urgency::Alert)).await;
    assert_eq!(outcome, PushOutcome::Accepted);
    assert_eq!(
        harness.sent(),
        vec![Sent {
            platform: PushPlatform::UnifiedPush,
            token: PushToken::from_bytes(b"https://ntfy.example/up1".to_vec()),
            message: PushMessage {
                handle: PushHandle::from_bytes(HANDLE),
                priority: Priority::High,
                time_to_live: MAX_PUSH_TTL,
            },
        }]
    );
    assert_eq!(harness.relay.counts().take().accepted, 1);
    Ok(())
}

#[tokio::test]
async fn quiet_urgency_sends_at_normal_priority_but_location_class_stays_high() -> TestResult {
    let mut opener = FakeOpener::default();
    let standard = unifiedpush_blob(&mut opener, 1, PushClass::Standard);
    let location = unifiedpush_blob(&mut opener, 2, PushClass::Location);
    let harness = Setup::new(opener).build()?;
    harness
        .push(&body(2, &standard, BIND, Urgency::Quiet))
        .await;
    harness
        .push(&body(2, &location, BIND, Urgency::Quiet))
        .await;
    let priorities: Vec<Priority> = harness
        .sent()
        .iter()
        .map(|sent| sent.message.priority)
        .collect();
    assert_eq!(priorities, vec![Priority::Normal, Priority::High]);
    Ok(())
}

#[tokio::test]
async fn ttl_is_capped_by_the_blob_expiry() -> TestResult {
    let mut opener = FakeOpener::default();
    let blob = opener.seal(
        1,
        Ok(plaintext(
            PushPlatform::Fcm,
            PushClass::Location,
            "fcm",
            NOW + 90,
        )),
    );
    let harness = Setup::new(opener).build()?;
    harness.push(&body(2, &blob, BIND, Urgency::Alert)).await;
    let ttl = harness.sent().first().map(|sent| sent.message.time_to_live);
    assert_eq!(ttl, Some(Duration::from_secs(90)));
    Ok(())
}

#[tokio::test]
async fn a_blob_presented_with_another_bind_does_not_open() -> TestResult {
    let mut opener = FakeOpener::default();
    let blob = unifiedpush_blob(&mut opener, 1, PushClass::Standard);
    let harness = Setup::new(opener).build()?;
    let outcome = harness
        .push(&body(2, &blob, [0xb2; 32], Urgency::Alert))
        .await;
    assert_eq!(outcome, PushOutcome::DoesNotOpen);
    assert!(harness.sent().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_blob_living_longer_than_max_ttl_is_gone() -> TestResult {
    // The harness publishes max_ttl = 30 days; one hour of clock skew is allowed on top.
    let limit = NOW + 30 * DAY + 3600;
    let mut opener = FakeOpener::default();
    let at_limit = opener.seal(
        1,
        Ok(plaintext(
            PushPlatform::UnifiedPush,
            PushClass::Standard,
            "https://a.example/one",
            limit,
        )),
    );
    let beyond = opener.seal(
        2,
        Ok(plaintext(
            PushPlatform::UnifiedPush,
            PushClass::Standard,
            "https://a.example/two",
            limit + 1,
        )),
    );
    let far_future = opener.seal(
        3,
        Ok(plaintext(
            PushPlatform::UnifiedPush,
            PushClass::Standard,
            "https://a.example/three",
            u64::MAX,
        )),
    );
    let harness = Setup::new(opener).build()?;
    assert_eq!(
        harness
            .push(&body(2, &at_limit, BIND, Urgency::Alert))
            .await,
        PushOutcome::Accepted
    );
    assert_eq!(
        harness.push(&body(2, &beyond, BIND, Urgency::Alert)).await,
        PushOutcome::TokenGone
    );
    assert_eq!(
        harness
            .push(&body(2, &far_future, BIND, Urgency::Alert))
            .await,
        PushOutcome::TokenGone
    );
    assert_eq!(harness.sent().len(), 1);
    Ok(())
}

#[tokio::test]
async fn an_expired_blob_is_gone_but_one_expiring_now_is_sent() -> TestResult {
    let mut opener = FakeOpener::default();
    let expired = opener.seal(
        1,
        Ok(plaintext(
            PushPlatform::UnifiedPush,
            PushClass::Standard,
            "https://a.example/",
            NOW - 1,
        )),
    );
    let last_second = opener.seal(
        2,
        Ok(plaintext(
            PushPlatform::UnifiedPush,
            PushClass::Standard,
            "https://a.example/",
            NOW,
        )),
    );
    let harness = Setup::new(opener).build()?;
    assert_eq!(
        harness.push(&body(2, &expired, BIND, Urgency::Alert)).await,
        PushOutcome::TokenGone
    );
    assert_eq!(
        harness
            .push(&body(2, &last_second, BIND, Urgency::Alert))
            .await,
        PushOutcome::Accepted
    );
    assert_eq!(harness.sent().len(), 1);
    Ok(())
}

#[tokio::test]
async fn the_old_key_opens_during_the_overlap_and_is_gone_after() -> TestResult {
    let mut opener = FakeOpener::default();
    let blob = opener.seal(
        1,
        Ok(plaintext(
            PushPlatform::UnifiedPush,
            PushClass::Standard,
            "https://a.example/",
            NOW + 30 * DAY,
        )),
    );
    let harness = Setup::new(opener).build()?;
    let request = body(1, &blob, BIND, Urgency::Alert);
    assert_eq!(harness.push(&request).await, PushOutcome::Accepted);
    harness.clock.advance(Duration::from_secs(29 * DAY));
    assert_eq!(harness.push(&request).await, PushOutcome::TokenGone);
    Ok(())
}

#[tokio::test]
async fn an_unknown_key_id_is_gone() -> TestResult {
    let mut opener = FakeOpener::default();
    let blob = unifiedpush_blob(&mut opener, 1, PushClass::Standard);
    let harness = Setup::new(opener).build()?;
    assert_eq!(
        harness.push(&body(9, &blob, BIND, Urgency::Alert)).await,
        PushOutcome::TokenGone
    );
    Ok(())
}

#[tokio::test]
async fn a_malformed_plaintext_is_gone_and_a_crypto_failure_is_internal() -> TestResult {
    let mut opener = FakeOpener::default();
    let malformed = opener.seal(1, Err(OpenError::Malformed));
    let broken = opener.seal(2, Err(OpenError::Unavailable));
    let harness = Setup::new(opener).build()?;
    assert_eq!(
        harness
            .push(&body(2, &malformed, BIND, Urgency::Alert))
            .await,
        PushOutcome::TokenGone
    );
    assert_eq!(
        harness.push(&body(2, &broken, BIND, Urgency::Alert)).await,
        PushOutcome::Internal
    );
    Ok(())
}

#[tokio::test]
async fn a_platform_this_relay_does_not_serve_is_unsupported_and_sends_nothing() -> TestResult {
    let mut opener = FakeOpener::default();
    let blob = opener.seal(
        1,
        Ok(plaintext(
            PushPlatform::Fcm,
            PushClass::Standard,
            "fcm",
            NOW + DAY,
        )),
    );
    let mut setup = Setup::new(opener);
    setup.supported = vec![PushPlatform::UnifiedPush];
    let harness = setup.build()?;
    assert_eq!(
        harness.push(&body(2, &blob, BIND, Urgency::Alert)).await,
        PushOutcome::PlatformUnsupported
    );
    assert!(harness.sent().is_empty());
    Ok(())
}

#[tokio::test]
async fn missing_or_wrong_credentials_are_unauthorized() -> TestResult {
    let mut opener = FakeOpener::default();
    let blob = unifiedpush_blob(&mut opener, 1, PushClass::Standard);
    let harness = Setup::new(opener).build()?;
    let request = body(2, &blob, BIND, Urgency::Alert);
    assert_eq!(
        harness.relay.push(None, &request).await,
        PushOutcome::Unauthorized
    );
    assert_eq!(
        harness.push_as("not-a-credential", &request).await,
        PushOutcome::Unauthorized
    );
    assert!(harness.sent().is_empty());
    assert_eq!(harness.relay.counts().take().unauthorized, 2);
    Ok(())
}

#[tokio::test]
async fn a_malformed_body_is_a_bad_request() -> TestResult {
    let harness = Setup::new(FakeOpener::default()).build()?;
    assert_eq!(harness.push(b"{}").await, PushOutcome::BadRequest);
    Ok(())
}

#[tokio::test]
async fn each_server_has_its_own_rate_limit() -> TestResult {
    let mut setup = Setup::new(FakeOpener::default());
    setup.server_a_per_minute = 2;
    let harness = setup.build()?;
    // Bad requests still count against the server: the limit is on asking, not on success.
    assert_eq!(harness.push(b"{}").await, PushOutcome::BadRequest);
    assert_eq!(harness.push(b"{}").await, PushOutcome::BadRequest);
    assert_eq!(harness.push(b"{}").await, PushOutcome::RateLimited);
    assert_eq!(
        harness.push_as(CREDENTIAL_B, b"{}").await,
        PushOutcome::BadRequest
    );
    harness.clock.advance(Duration::from_secs(30));
    assert_eq!(harness.push(b"{}").await, PushOutcome::BadRequest);
    Ok(())
}

#[tokio::test]
async fn a_replayed_token_is_rate_limited_across_blobs_and_servers() -> TestResult {
    let mut opener = FakeOpener::default();
    // Two different blobs (two mailboxes) sealing the same device token.
    let first = unifiedpush_blob(&mut opener, 1, PushClass::Standard);
    let second = unifiedpush_blob(&mut opener, 2, PushClass::Standard);
    let other_device = opener.seal(
        3,
        Ok(plaintext(
            PushPlatform::UnifiedPush,
            PushClass::Standard,
            "https://ntfy.example/up2",
            NOW + DAY,
        )),
    );
    let mut setup = Setup::new(opener);
    setup.token_per_minute = 1;
    let harness = setup.build()?;
    assert_eq!(
        harness.push(&body(2, &first, BIND, Urgency::Alert)).await,
        PushOutcome::Accepted
    );
    assert_eq!(
        harness
            .push_as(CREDENTIAL_B, &body(2, &second, BIND, Urgency::Alert))
            .await,
        PushOutcome::RateLimited
    );
    assert_eq!(
        harness
            .push(&body(2, &other_device, BIND, Urgency::Alert))
            .await,
        PushOutcome::Accepted
    );
    assert_eq!(harness.sent().len(), 2);
    harness.clock.advance(Duration::from_secs(60));
    assert_eq!(
        harness.push(&body(2, &second, BIND, Urgency::Alert)).await,
        PushOutcome::Accepted
    );
    Ok(())
}

async fn outcome_when_sender_says(
    delivery: DeliveryOutcome,
) -> Result<PushOutcome, Box<dyn std::error::Error>> {
    let mut opener = FakeOpener::default();
    let blob = unifiedpush_blob(&mut opener, 1, PushClass::Standard);
    let mut setup = Setup::new(opener);
    setup.outcome = delivery;
    let harness = setup.build()?;
    Ok(harness.push(&body(2, &blob, BIND, Urgency::Alert)).await)
}

#[tokio::test]
async fn push_service_answers_map_to_outcomes() -> TestResult {
    assert_eq!(
        outcome_when_sender_says(DeliveryOutcome::TokenGone).await?,
        PushOutcome::TokenGone
    );
    assert_eq!(
        outcome_when_sender_says(DeliveryOutcome::Failed).await?,
        PushOutcome::UpstreamFailed
    );
    assert_eq!(
        outcome_when_sender_says(DeliveryOutcome::TimedOut).await?,
        PushOutcome::UpstreamTimedOut
    );
    Ok(())
}

#[test]
fn the_published_config_follows_rotation_and_lists_served_platforms() -> TestResult {
    let mut setup = Setup::new(FakeOpener::default());
    setup.supported = vec![PushPlatform::Fcm];
    let harness = setup.build()?;
    let config = harness
        .relay
        .published_config()
        .ok_or("nothing published")?;
    assert_eq!(config.key_config.key_id, PushRelayKeyId::new(2));
    assert_eq!(config.platforms, vec![PushPlatform::Fcm]);
    assert_eq!(
        config.max_time_to_live.duration(),
        Duration::from_secs(30 * DAY)
    );
    assert_eq!(config.operator, "Test Relay Ltd");
    Ok(())
}

#[test]
fn the_next_key_is_announced_before_it_is_published() -> TestResult {
    let mut setup = Setup::new(FakeOpener::default());
    setup.key_3_activates_at = Some(NOW + DAY);
    let harness = setup.build()?;
    let next = harness.relay.next_config().ok_or("no next key")?;
    assert_eq!(next.key_config.key_id, PushRelayKeyId::new(3));
    assert_eq!(next.operator, "Test Relay Ltd");
    let published = harness
        .relay
        .published_config()
        .ok_or("nothing published")?;
    assert_eq!(published.key_config.key_id, PushRelayKeyId::new(2));
    harness.clock.advance(Duration::from_secs(DAY));
    let published = harness
        .relay
        .published_config()
        .ok_or("nothing published")?;
    assert_eq!(published.key_config, next.key_config);
    assert_eq!(harness.relay.next_config(), None);
    Ok(())
}

#[test]
fn the_status_reports_the_rotation_and_platforms() -> TestResult {
    let mut setup = Setup::new(FakeOpener::default());
    setup.supported = vec![PushPlatform::Fcm];
    setup.key_3_activates_at = Some(NOW + DAY);
    let harness = setup.build()?;
    assert_eq!(
        harness.relay.status(),
        RelayStatus {
            operator: "Test Relay Ltd".to_owned(),
            platforms: vec![PushPlatform::Fcm],
            published_key: Some(PushRelayKeyId::new(2)),
            next_key: Some(PushRelayKeyId::new(3)),
            next_key_activates_at: Some(UnixSeconds::new(NOW + DAY)),
        }
    );
    Ok(())
}
