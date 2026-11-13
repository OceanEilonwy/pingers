// SPDX-License-Identifier: AGPL-3.0-only
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use axum::http::StatusCode as MockStatus;
use pingers_testkit::ManualClock;
use pingers_time::UnixSeconds;

use super::*;
use crate::mock_http::MockHttp;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const HANDLE: [u8; 8] = [0xde, 0xad, 0xbe, 0xef, 0, 1, 2, 3];

fn message(priority: Priority) -> PushMessage {
    PushMessage {
        handle: PushHandle::from_bytes(HANDLE),
        priority,
        time_to_live: Duration::from_secs(600),
    }
}

fn token(text: &str) -> PushToken {
    PushToken::from_bytes(text.as_bytes().to_vec())
}

fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

fn clock() -> ManualClock {
    ManualClock::new(UnixSeconds::new(1_800_000_000))
}

fn unifiedpush_dispatcher(timeout: Duration) -> Result<Dispatcher<ManualClock>, reqwest::Error> {
    Ok(Dispatcher::new(
        client()?,
        clock(),
        DispatcherSettings {
            timeout,
            unifiedpush: Some(EndpointPolicy::AllowInsecureLocal),
            fcm: None,
        },
    ))
}

fn fcm_dispatcher(mock: &MockHttp) -> Result<Dispatcher<ManualClock>, Box<dyn std::error::Error>> {
    let endpoint = Url::parse(&mock.url("/"))?;
    let settings = FcmSettings::new(
        "pingers-test",
        &endpoint,
        FcmAccessToken::new("ya29.test\n"),
    )
    .ok_or("FCM settings refused")?;
    Ok(Dispatcher::new(
        client()?,
        clock(),
        DispatcherSettings {
            timeout: Duration::from_secs(10),
            unifiedpush: None,
            fcm: Some(settings),
        },
    ))
}

#[test]
fn location_and_alert_wake_at_high_priority_and_quiet_at_normal() {
    let location = WakeClass::of(PushClass::Location, Urgency::Quiet);
    assert_eq!(location, WakeClass::Location);
    assert_eq!(location.priority(), Priority::High);
    let alert = WakeClass::of(PushClass::Standard, Urgency::Alert);
    assert_eq!(alert, WakeClass::Alert);
    assert_eq!(alert.priority(), Priority::High);
    let quiet = WakeClass::of(PushClass::Standard, Urgency::Quiet);
    assert_eq!(quiet, WakeClass::Quiet);
    assert_eq!(quiet.priority(), Priority::Normal);
}

#[test]
fn the_payload_carries_only_the_version_and_the_handle() {
    assert_eq!(
        message(Priority::High).payload(),
        r#"{"v":1,"h":"deadbeef00010203"}"#
    );
}

#[tokio::test]
async fn unifiedpush_posts_the_payload_with_ttl_and_urgency() -> TestResult {
    let mock = MockHttp::start(MockStatus::CREATED, "").await?;
    let dispatcher = unifiedpush_dispatcher(Duration::from_secs(10))?;
    let outcome = dispatcher
        .send(
            PushPlatform::UnifiedPush,
            &token(&mock.url("/upAbC?up=1")),
            &message(Priority::Normal),
        )
        .await;
    assert_eq!(outcome, DeliveryOutcome::Delivered);
    let recorded = mock.recorded();
    let request = recorded.first().ok_or("no request")?;
    assert_eq!(request.path_and_query, "/upAbC?up=1");
    assert_eq!(request.body.as_ref(), br#"{"v":1,"h":"deadbeef00010203"}"#);
    assert_eq!(request.headers.get("ttl").ok_or("no TTL")?, "600");
    assert_eq!(
        request.headers.get("urgency").ok_or("no Urgency")?,
        "normal"
    );
    Ok(())
}

#[tokio::test]
async fn unifiedpush_high_priority_is_urgency_high() -> TestResult {
    let mock = MockHttp::start(MockStatus::OK, "").await?;
    let dispatcher = unifiedpush_dispatcher(Duration::from_secs(10))?;
    dispatcher
        .send(
            PushPlatform::UnifiedPush,
            &token(&mock.url("/up")),
            &message(Priority::High),
        )
        .await;
    let recorded = mock.recorded();
    let request = recorded.first().ok_or("no request")?;
    assert_eq!(request.headers.get("urgency").ok_or("no Urgency")?, "high");
    Ok(())
}

async fn unifiedpush_outcome_for(
    status: MockStatus,
) -> Result<DeliveryOutcome, Box<dyn std::error::Error>> {
    let mock = MockHttp::start(status, "").await?;
    let dispatcher = unifiedpush_dispatcher(Duration::from_secs(10))?;
    Ok(dispatcher
        .send(
            PushPlatform::UnifiedPush,
            &token(&mock.url("/up")),
            &message(Priority::High),
        )
        .await)
}

#[tokio::test]
async fn unifiedpush_404_and_410_mean_the_token_is_gone() -> TestResult {
    assert_eq!(
        unifiedpush_outcome_for(MockStatus::NOT_FOUND).await?,
        DeliveryOutcome::TokenGone
    );
    assert_eq!(
        unifiedpush_outcome_for(MockStatus::GONE).await?,
        DeliveryOutcome::TokenGone
    );
    Ok(())
}

#[tokio::test]
async fn unifiedpush_other_errors_and_redirects_are_failures_that_keep_the_token() -> TestResult {
    assert_eq!(
        unifiedpush_outcome_for(MockStatus::TOO_MANY_REQUESTS).await?,
        DeliveryOutcome::Failed
    );
    assert_eq!(
        unifiedpush_outcome_for(MockStatus::INTERNAL_SERVER_ERROR).await?,
        DeliveryOutcome::Failed
    );
    assert_eq!(
        unifiedpush_outcome_for(MockStatus::TEMPORARY_REDIRECT).await?,
        DeliveryOutcome::Failed
    );
    Ok(())
}

#[tokio::test]
async fn an_unreachable_endpoint_is_a_failure() -> TestResult {
    let dispatcher = unifiedpush_dispatcher(Duration::from_secs(10))?;
    // Port 9 on loopback: nothing listens there in the test environment.
    let outcome = dispatcher
        .send(
            PushPlatform::UnifiedPush,
            &token("http://127.0.0.1:9/up"),
            &message(Priority::High),
        )
        .await;
    assert_eq!(outcome, DeliveryOutcome::Failed);
    Ok(())
}

#[tokio::test]
async fn a_silent_endpoint_times_out() -> TestResult {
    let mock = MockHttp::start_hanging().await?;
    let dispatcher = unifiedpush_dispatcher(Duration::ZERO)?;
    let outcome = dispatcher
        .send(
            PushPlatform::UnifiedPush,
            &token(&mock.url("/up")),
            &message(Priority::High),
        )
        .await;
    assert_eq!(outcome, DeliveryOutcome::TimedOut);
    Ok(())
}

#[tokio::test]
async fn a_token_that_is_not_a_url_is_gone_without_a_request() -> TestResult {
    let dispatcher = unifiedpush_dispatcher(Duration::from_secs(10))?;
    for bad in [
        &b"not a url"[..],
        &[0xff, 0xfe][..],
        b"ftp://example.com/up",
    ] {
        let outcome = dispatcher
            .send(
                PushPlatform::UnifiedPush,
                &PushToken::from_bytes(bad.to_vec()),
                &message(Priority::High),
            )
            .await;
        assert_eq!(outcome, DeliveryOutcome::TokenGone);
    }
    Ok(())
}

fn public_policy_allows(endpoint: &str) -> bool {
    unifiedpush_endpoint(&token(endpoint), EndpointPolicy::PublicHttpsOnly).is_some()
}

#[test]
fn the_public_policy_allows_https_to_public_hosts() {
    assert!(public_policy_allows("https://ntfy.sh/upAbC?up=1"));
    assert!(public_policy_allows("https://203.0.114.1/up"));
    assert!(public_policy_allows("https://[2001:4860::1]/up"));
    assert!(public_policy_allows("https://100.128.0.1/up"));
}

#[test]
fn the_public_policy_refuses_http_and_local_hosts() {
    for endpoint in [
        "http://ntfy.sh/up",
        "https://localhost/up",
        "https://LOCALHOST./up",
        "https://push.localhost/up",
        "https://127.0.0.1/up",
        "https://10.1.2.3/up",
        "https://172.16.0.1/up",
        "https://192.168.1.1/up",
        "https://169.254.169.254/latest/meta-data",
        "https://0.0.0.0/up",
        "https://255.255.255.255/up",
        "https://192.0.2.1/up",
        "https://100.64.0.1/up",
        "https://100.127.255.255/up",
        "https://[::1]/up",
        "https://[::]/up",
        "https://[fd00::1]/up",
        "https://[fe80::1]/up",
        "https://[::ffff:127.0.0.1]/up",
        // Gaps the wave 1 review found.
        "https://[::127.0.0.1]/up",
        "https://[64:ff9b::7f00:1]/up",
        "https://[2002:c0a8:101::1]/up",
        "https://[ff02::1]/up",
        "https://0.1.2.3/up",
        "https://192.0.0.8/up",
        "https://198.18.0.1/up",
        "https://224.0.0.1/up",
    ] {
        assert!(
            !public_policy_allows(endpoint),
            "{endpoint} should be refused"
        );
    }
}

#[test]
fn the_insecure_policy_allows_local_http_only_for_web_schemes() {
    let allows = |endpoint: &str| {
        unifiedpush_endpoint(&token(endpoint), EndpointPolicy::AllowInsecureLocal).is_some()
    };
    assert!(allows("http://127.0.0.1:8080/up"));
    assert!(allows("https://localhost/up"));
    assert!(!allows("file:///etc/passwd"));
}

#[tokio::test]
async fn fcm_sends_a_data_message_with_the_bearer_token() -> TestResult {
    let mock = MockHttp::start(MockStatus::OK, r#"{"name":"projects/p/messages/1"}"#).await?;
    let dispatcher = fcm_dispatcher(&mock)?;
    let outcome = dispatcher
        .send(
            PushPlatform::Fcm,
            &token("fcm-token-1"),
            &message(Priority::Normal),
        )
        .await;
    assert_eq!(outcome, DeliveryOutcome::Delivered);
    let recorded = mock.recorded();
    let request = recorded.first().ok_or("no request")?;
    assert_eq!(
        request.path_and_query,
        "/v1/projects/pingers-test/messages:send"
    );
    assert_eq!(
        request
            .headers
            .get("authorization")
            .ok_or("no Authorization")?,
        "Bearer ya29.test"
    );
    let body: serde_json::Value = serde_json::from_slice(&request.body)?;
    assert_eq!(
        body,
        serde_json::json!({"message": {
            "token": "fcm-token-1",
            "data": {"v": "1", "h": "deadbeef00010203"},
            "android": {"priority": "NORMAL", "ttl": "600s"},
        }})
    );
    Ok(())
}

#[tokio::test]
async fn fcm_high_priority_is_android_high() -> TestResult {
    let mock = MockHttp::start(MockStatus::OK, "{}").await?;
    let dispatcher = fcm_dispatcher(&mock)?;
    dispatcher
        .send(PushPlatform::Fcm, &token("t"), &message(Priority::High))
        .await;
    let recorded = mock.recorded();
    let body: serde_json::Value =
        serde_json::from_slice(&recorded.first().ok_or("no request")?.body)?;
    assert_eq!(
        body.pointer("/message/android/priority"),
        Some(&serde_json::json!("HIGH"))
    );
    Ok(())
}

fn fcm_error(status: u16, code: &str) -> String {
    format!(
        r#"{{"error":{{"code":{status},"status":"X","details":[{{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"{code}"}}]}}}}"#
    )
}

async fn fcm_outcome_for(
    status: MockStatus,
    body: &str,
) -> Result<DeliveryOutcome, Box<dyn std::error::Error>> {
    let mock = MockHttp::start(status, body).await?;
    let dispatcher = fcm_dispatcher(&mock)?;
    Ok(dispatcher
        .send(PushPlatform::Fcm, &token("t"), &message(Priority::High))
        .await)
}

#[tokio::test]
async fn fcm_unregistered_and_sender_mismatch_mean_the_token_is_gone() -> TestResult {
    assert_eq!(
        fcm_outcome_for(MockStatus::NOT_FOUND, &fcm_error(404, "UNREGISTERED")).await?,
        DeliveryOutcome::TokenGone
    );
    assert_eq!(
        fcm_outcome_for(MockStatus::BAD_REQUEST, &fcm_error(400, "UNREGISTERED")).await?,
        DeliveryOutcome::TokenGone
    );
    assert_eq!(
        fcm_outcome_for(MockStatus::FORBIDDEN, &fcm_error(403, "SENDER_ID_MISMATCH")).await?,
        DeliveryOutcome::TokenGone
    );
    Ok(())
}

#[tokio::test]
async fn fcm_quota_auth_and_argument_errors_keep_the_token() -> TestResult {
    assert_eq!(
        fcm_outcome_for(
            MockStatus::TOO_MANY_REQUESTS,
            &fcm_error(429, "QUOTA_EXCEEDED")
        )
        .await?,
        DeliveryOutcome::Failed
    );
    assert_eq!(
        fcm_outcome_for(MockStatus::UNAUTHORIZED, "{}").await?,
        DeliveryOutcome::Failed
    );
    // INVALID_ARGUMENT can also mean our own payload is wrong; deleting every blob would be
    // the wrong reaction to a relay bug.
    assert_eq!(
        fcm_outcome_for(MockStatus::BAD_REQUEST, &fcm_error(400, "INVALID_ARGUMENT")).await?,
        DeliveryOutcome::Failed
    );
    assert_eq!(
        fcm_outcome_for(MockStatus::BAD_REQUEST, "not json").await?,
        DeliveryOutcome::Failed
    );
    Ok(())
}

#[tokio::test]
async fn fcm_token_that_is_not_utf8_is_gone_without_a_request() -> TestResult {
    let mock = MockHttp::start(MockStatus::OK, "{}").await?;
    let dispatcher = fcm_dispatcher(&mock)?;
    let outcome = dispatcher
        .send(
            PushPlatform::Fcm,
            &PushToken::from_bytes(vec![0xff]),
            &message(Priority::High),
        )
        .await;
    assert_eq!(outcome, DeliveryOutcome::TokenGone);
    assert!(mock.recorded().is_empty());
    Ok(())
}

#[test]
fn fcm_project_ids_that_could_alter_the_path_are_refused() -> TestResult {
    let endpoint = Url::parse("https://fcm.googleapis.com/")?;
    for bad in ["", "a/b", "a?b", "../x", "a b"] {
        assert!(
            FcmSettings::new(bad, &endpoint, FcmAccessToken::new("t")).is_none(),
            "{bad}"
        );
    }
    let settings = FcmSettings::new("my-project-1", &endpoint, FcmAccessToken::new("t"))
        .ok_or("refused a valid id")?;
    assert_eq!(
        settings.send_url.as_str(),
        "https://fcm.googleapis.com/v1/projects/my-project-1/messages:send"
    );
    Ok(())
}

#[test]
fn a_dispatcher_supports_only_its_enabled_platforms() -> TestResult {
    let dispatcher = unifiedpush_dispatcher(Duration::from_secs(1))?;
    assert!(dispatcher.supports(PushPlatform::UnifiedPush));
    assert!(!dispatcher.supports(PushPlatform::Fcm));
    Ok(())
}

#[tokio::test]
async fn an_fcm_dispatcher_supports_fcm_only() -> TestResult {
    let mock = MockHttp::start(MockStatus::OK, "{}").await?;
    let dispatcher = fcm_dispatcher(&mock)?;
    assert!(dispatcher.supports(PushPlatform::Fcm));
    assert!(!dispatcher.supports(PushPlatform::UnifiedPush));
    Ok(())
}

#[tokio::test]
async fn sending_on_a_disabled_platform_fails_without_a_request() -> TestResult {
    let mock = MockHttp::start(MockStatus::OK, "").await?;
    let dispatcher = unifiedpush_dispatcher(Duration::from_secs(1))?;
    let outcome = dispatcher
        .send(
            PushPlatform::Fcm,
            &token(&mock.url("/up")),
            &message(Priority::High),
        )
        .await;
    assert_eq!(outcome, DeliveryOutcome::Failed);
    assert!(mock.recorded().is_empty());
    Ok(())
}
