// SPDX-License-Identifier: AGPL-3.0-only
//! The harness's configuration, its behaviour without the feature, and with it: three services
//! that answer, pinned keys that follow the seed, captured pushes, and a clean stop.

use std::net::{IpAddr, Ipv4Addr};

use super::{HarnessConfig, InProcessHarness};

#[test]
fn default_config_binds_loopback_at_the_test_start() {
    let config = HarnessConfig::default();
    assert_eq!(config.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(pingers_time::Clock::now(&config.clock), crate::TEST_START);
    assert_eq!(config.seed, 0);
}

#[cfg(not(feature = "harness"))]
#[test]
fn without_the_feature_start_reports_the_server_unwired() {
    assert!(matches!(
        InProcessHarness::start(&HarnessConfig::default()),
        Err(super::HarnessError::NotWired {
            service: super::Service::Server
        })
    ));
}

#[cfg(feature = "harness")]
mod running {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::net::SocketAddr;
    use std::time::Duration;

    use axum::body::{Body, Bytes, HttpBody as _};
    use hyper::client::conn::http2::SendRequest;
    use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
    use hyper::{Method, Request, StatusCode};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use pingers_crypto::{PushRelayPublicKey, PushTokenToSeal, push_bind_for_mailbox};
    use pingers_ohttp::{BinaryRequest, KeyConfig, encapsulate_request};
    use pingers_proto::{MailboxId, PushClass, PushPlatform, PushToken, PushTokenPlaintext};
    use pingers_time::UnixSeconds;

    use super::super::wiring::PUSH_CREDENTIAL;
    use super::{HarnessConfig, InProcessHarness};
    use crate::seeded_rng;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    const STEP: Duration = Duration::from_secs(10);

    async fn connect(
        address: SocketAddr,
    ) -> Result<SendRequest<Body>, Box<dyn std::error::Error + Send + Sync>> {
        let stream = tokio::net::TcpStream::connect(address).await?;
        let (sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                .await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(sender)
    }

    /// Sends a request and returns the status and the whole body.
    async fn exchange(
        address: SocketAddr,
        request: Request<Body>,
    ) -> Result<(StatusCode, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
        let mut client = connect(address).await?;
        let response = tokio::time::timeout(STEP, client.send_request(request)).await??;
        let status = response.status();
        let mut body = response.into_body();
        let mut all = Vec::new();
        while let Some(frame) = tokio::time::timeout(
            STEP,
            std::future::poll_fn(|context| std::pin::Pin::new(&mut body).poll_frame(context)),
        )
        .await?
        {
            if let Ok(data) = frame?.into_data() {
                all.extend_from_slice(&data);
            }
        }
        Ok((status, all))
    }

    fn get(path: &str) -> Result<Request<Body>, hyper::http::Error> {
        Request::builder()
            .uri(format!("http://harness{path}"))
            .body(Body::empty())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_three_services_answer_and_stop_cleanly() -> TestResult {
        let harness = InProcessHarness::start(&HarnessConfig::default())?;
        let endpoints = harness.endpoints().clone();

        // The key config, fetched through the relay as clients do, is the one to pin.
        let (status, config) = exchange(endpoints.ohttp_relay, get("/v1/ohttp/config")?).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(config, endpoints.ohttp_key_config);

        // An anonymous request through the relay reaches the server's gateway and comes back.
        let key_config = KeyConfig::decode(&endpoints.ohttp_key_config)?;
        let inner = BinaryRequest {
            method: "POST".to_owned(),
            path: "/v1/groups".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let encapsulated = encapsulate_request(&key_config, &inner, &mut seeded_rng(1))?;
        let request = Request::builder()
            .method(Method::POST)
            .uri("http://harness/ohttp")
            .header(CONTENT_TYPE, "message/ohttp-req")
            .body(Body::from(encapsulated.message))?;
        let (status, sealed) = exchange(endpoints.ohttp_relay, request).await?;
        assert_eq!(status, StatusCode::OK);
        let opened = encapsulated.response_context.open(&sealed)?;
        assert_eq!(
            opened.status.get(),
            501,
            "pingers-delivery is not wired yet"
        );

        // The push relay publishes its key.
        let (status, _) = exchange(endpoints.push_relay, get("/v1/config")?).await?;
        assert_eq!(status, StatusCode::OK);

        harness.shutdown();
        for address in [
            endpoints.server,
            endpoints.ohttp_relay,
            endpoints.push_relay,
        ] {
            assert!(
                tokio::net::TcpStream::connect(address).await.is_err(),
                "{address} still accepts"
            );
        }
        Ok(())
    }

    #[test]
    fn keys_follow_the_seed() -> TestResult {
        let start = |seed| {
            InProcessHarness::start(&HarnessConfig {
                seed,
                ..HarnessConfig::default()
            })
        };
        let first = start(7)?;
        let again = start(7)?;
        let other = start(8)?;
        let keys = |harness: &InProcessHarness| {
            (
                harness.endpoints().ohttp_key_config.clone(),
                harness.endpoints().push_relay_key.clone(),
            )
        };
        assert_eq!(keys(&first), keys(&again));
        let (other_config, other_push) = keys(&other);
        let (first_config, first_push) = keys(&first);
        assert_ne!(first_config, other_config);
        assert_ne!(first_push, other_push);
        assert_eq!(first_push.len(), 32);
        assert_ne!(first.endpoints().server, again.endpoints().server);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pushes_are_captured_with_their_class_and_time() -> TestResult {
        let config = HarnessConfig::default();
        let harness = InProcessHarness::start(&config)?;
        let endpoints = harness.endpoints().clone();
        assert!(harness.pushes().is_empty());

        let relay_key = PushRelayPublicKey::new(1, endpoints.push_relay_key.as_slice().try_into()?);
        let bind = push_bind_for_mailbox(&MailboxId::from_bytes([5; 16]));
        let token = PushToken::from_bytes(b"https://push.example/endpoint".to_vec());
        let plaintext = PushTokenPlaintext {
            platform: PushPlatform::UnifiedPush,
            class: PushClass::Location,
            token: token.clone(),
            // Within the relay's max_ttl of 30 days.
            not_after: UnixSeconds::new(crate::TEST_START.get().saturating_add(86_400)),
        };
        let blob = pingers_crypto::seal_push_token(
            &relay_key,
            &PushTokenToSeal {
                bind,
                plaintext: &plaintext,
            },
            &mut seeded_rng(2),
        )?;
        let body = pingers_proto::WireEncode::encode(&pingers_proto::PushRequest {
            key_id: pingers_proto::PushRelayKeyId::new(1),
            blob,
            bind,
            urgency: pingers_proto::Urgency::Quiet,
            handle: pingers_proto::PushHandle::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
        })?;
        let request = Request::builder()
            .method(Method::POST)
            .uri("http://harness/v1/push")
            .header(CONTENT_TYPE, "application/cbor")
            .header(
                AUTHORIZATION,
                format!("Bearer {}", String::from_utf8(PUSH_CREDENTIAL.to_vec())?),
            )
            .body(Body::from(Bytes::from(body)))?;
        let (status, _) = exchange(endpoints.push_relay, request).await?;
        assert_eq!(status, StatusCode::ACCEPTED);

        let pushes = harness.pushes();
        assert_eq!(pushes.len(), 1);
        let push = pushes.first().ok_or("no push")?;
        assert_eq!(push.platform, PushPlatform::UnifiedPush);
        assert_eq!(push.class, PushClass::Location);
        assert_eq!(push.token, token);
        assert_eq!(push.sent_at, crate::TEST_START);
        Ok(())
    }
}
