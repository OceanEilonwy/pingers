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

    use axum::body::{Body, HttpBody as _};
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

    /// An HTTPS client that trusts only the harness's test CA.
    fn push_client(
        endpoints: &super::super::Endpoints,
    ) -> Result<reqwest::Client, Box<dyn std::error::Error + Send + Sync>> {
        let ca = reqwest::Certificate::from_pem(endpoints.push_relay_ca.as_bytes())?;
        Ok(reqwest::Client::builder().tls_certs_only([ca]).build()?)
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
            401,
            "an anonymous route without a credential"
        );

        // The push relay publishes its key, over HTTPS with the test CA's certificate.
        let config = push_client(&endpoints)?
            .get(format!("https://{}/v1/config", endpoints.push_relay))
            .send()
            .await?;
        assert_eq!(config.status().as_u16(), 200);
        // Plain HTTP is not served.
        assert!(
            exchange(endpoints.push_relay, get("/v1/config")?)
                .await
                .is_err()
        );

        harness.shutdown();
        for address in [
            endpoints.server,
            endpoints.ohttp_relay,
            endpoints.ohttp_relay_https,
            endpoints.push_relay,
        ] {
            assert!(
                tokio::net::TcpStream::connect(address).await.is_err(),
                "{address} still accepts"
            );
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_server_origin_is_https_and_checks_registration_codes() -> TestResult {
        use pingers_crypto::{AccountSigningKey, sign_registration};
        use pingers_proto::{RegisterRequest, SecretText, ServerId, WireEncode as _};

        let harness = InProcessHarness::start(&HarnessConfig {
            registration_codes: vec!["friends".to_owned()],
            ..HarnessConfig::default()
        })?;
        let endpoints = harness.endpoints().clone();
        assert_eq!(
            endpoints.server_id,
            format!("https://{}", endpoints.server_https)
        );
        let client = push_client(&endpoints)?;
        let key = AccountSigningKey::from_bytes(zeroize::Zeroizing::new([5; 32]));
        let server = ServerId::new(endpoints.server_id.clone())?;
        for (code, expected) in [
            (None, 403),
            (Some("strangers"), 403),
            (Some("friends"), 200),
        ] {
            let body = RegisterRequest {
                signature_key: *key.public_key().as_bytes(),
                proof: *sign_registration(&key, &server)?.as_bytes(),
                registration_code: code.map(|code: &str| SecretText::new(code.to_owned())),
            }
            .encode()?;
            let answer = client
                .post(format!("{}/v1/accounts", endpoints.server_id))
                .body(body)
                .send()
                .await?;
            assert_eq!(answer.status().as_u16(), expected, "code {code:?}");
        }
        harness.shutdown();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_proof_switch_reaches_the_server() -> TestResult {
        use pingers_proto::{AnonymousCredential, WireDecode as _};

        for (switch, expected) in [
            (
                crate::MembershipProofs::On,
                vec![
                    AnonymousCredential::MembershipProof,
                    AnonymousCredential::PrivacyPass,
                ],
            ),
            (
                crate::MembershipProofs::Off,
                vec![AnonymousCredential::PrivacyPass],
            ),
        ] {
            let harness = InProcessHarness::start(&HarnessConfig {
                membership_proofs: switch,
                ..HarnessConfig::default()
            })?;
            let (status, body) = exchange(harness.endpoints().server, get("/v1/params")?).await?;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                pingers_proto::Params::decode(&body)?.anonymous_credentials,
                expected
            );
            harness.shutdown();
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_parameters_advertise_the_push_relay_and_a_map_host() -> TestResult {
        use pingers_proto::WireDecode as _;

        let harness = InProcessHarness::start(&HarnessConfig::default())?;
        let endpoints = harness.endpoints().clone();
        // Public: the server answers it directly too.
        let (status, body) = exchange(endpoints.server, get("/v1/params")?).await?;
        assert_eq!(status, StatusCode::OK);
        let params = pingers_proto::Params::decode(&body)?;
        let public_key: [u8; 32] = endpoints.push_relay_key.as_slice().try_into()?;
        let hash = pingers_crypto::key_config_hash(
            &pingers_proto::PushRelayKeyConfig {
                key_id: pingers_proto::PushRelayKeyId::new(1),
                public_key,
            }
            .to_bytes(),
        );
        let [listed] = params.push_relays.as_slice() else {
            return Err("one push relay".into());
        };
        assert_eq!(listed.relay_id, super::super::HARNESS_PUSH_RELAY_ID);
        assert_eq!(
            listed.url.as_str(),
            format!("https://{}", endpoints.push_relay)
        );
        assert_eq!(listed.key_config_hash, hash);
        assert_eq!(
            params
                .map_host
                .as_ref()
                .map(pingers_proto::MapHostUrl::as_str),
            Some(super::super::HARNESS_MAP_HOST)
        );
        harness.shutdown();
        Ok(())
    }

    /// Fetches `/v1/params` through the harness's HTTPS relay front, as a client that added
    /// the server by its address does: an OHTTP-encapsulated `GET` to `relay_url`.
    async fn params_through_relay(
        endpoints: &super::super::Endpoints,
        relay_url: &str,
        seed: u64,
    ) -> Result<pingers_proto::Params, Box<dyn std::error::Error + Send + Sync>> {
        use pingers_proto::WireDecode as _;

        let key_config = KeyConfig::decode(&endpoints.ohttp_key_config)?;
        let inner = BinaryRequest {
            method: "GET".to_owned(),
            path: "/v1/params".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let encapsulated = encapsulate_request(&key_config, &inner, &mut seeded_rng(seed))?;
        let answer = push_client(endpoints)?
            .post(relay_url)
            .header(CONTENT_TYPE.as_str(), "message/ohttp-req")
            .body(encapsulated.message)
            .send()
            .await?;
        if answer.status().as_u16() != 200 {
            return Err(format!("relay answered {}", answer.status()).into());
        }
        let sealed = answer.bytes().await?;
        let opened = encapsulated.response_context.open(&sealed)?;
        if opened.status.get() != 200 {
            return Err(format!("server answered {}", opened.status.get()).into());
        }
        Ok(pingers_proto::Params::decode(&opened.body)?)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_parameters_fetched_through_the_https_relay_list_that_relay() -> TestResult {
        let harness = InProcessHarness::start(&HarnessConfig::default())?;
        let endpoints = harness.endpoints().clone();
        let relay_url = format!("https://{}/ohttp", endpoints.ohttp_relay_https);
        let params = params_through_relay(&endpoints, &relay_url, 3).await?;
        assert_eq!(
            params
                .ohttp_relays
                .iter()
                .map(pingers_proto::RelayUrl::as_str)
                .collect::<Vec<_>>(),
            vec![relay_url.as_str()]
        );
        // Without an operator the origin is published.
        assert_eq!(params.server_operator, endpoints.server_id);

        // The key config is served through the HTTPS front too.
        let config = push_client(&endpoints)?
            .get(format!(
                "https://{}/v1/ohttp/config",
                endpoints.ohttp_relay_https
            ))
            .send()
            .await?;
        assert_eq!(config.status().as_u16(), 200);
        assert_eq!(config.bytes().await?.to_vec(), endpoints.ohttp_key_config);
        harness.shutdown();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_harnesses_run_at_once_with_their_own_keys_and_names() -> TestResult {
        let start = |seed: u64, operator: &str| {
            InProcessHarness::start(&HarnessConfig {
                seed,
                operator: Some(operator.to_owned()),
                ..HarnessConfig::default()
            })
        };
        let first = start(11, "Server A")?;
        let second = start(12, "Server B")?;
        let (a, b) = (first.endpoints().clone(), second.endpoints().clone());
        assert_ne!(a.ohttp_key_config, b.ohttp_key_config);
        assert_ne!(a.server_id, b.server_id);
        assert_ne!(a.ohttp_relay_https, b.ohttp_relay_https);

        // Each answers through its own relay with its own name and relay.
        for (endpoints, operator) in [(&a, "Server A"), (&b, "Server B")] {
            let relay_url = format!("https://{}/ohttp", endpoints.ohttp_relay_https);
            let params = params_through_relay(endpoints, &relay_url, 4).await?;
            assert_eq!(params.server_operator, operator);
            assert_eq!(
                params
                    .ohttp_relays
                    .first()
                    .map(pingers_proto::RelayUrl::as_str),
                Some(relay_url.as_str())
            );
        }
        first.shutdown();
        second.shutdown();
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
        let client = push_client(&endpoints)?;
        let push = || {
            client
                .post(format!("https://{}/v1/push", endpoints.push_relay))
                .header(CONTENT_TYPE.as_str(), "application/cbor")
                .header(AUTHORIZATION.as_str(), format!("Bearer {PUSH_CREDENTIAL}"))
                .body(body.clone())
                .send()
        };
        assert_eq!(push().await?.status().as_u16(), 202);

        let pushes = harness.pushes();
        assert_eq!(pushes.len(), 1);
        let captured = pushes.first().ok_or("no push")?;
        assert_eq!(captured.platform, PushPlatform::UnifiedPush);
        assert_eq!(captured.class, PushClass::Location);
        assert_eq!(captured.token, token);
        assert_eq!(captured.sent_at, crate::TEST_START);
        assert!(harness.gone_pushes().is_empty());

        // A token marked gone is answered 410 and noted, not captured.
        harness.mark_token_gone(token.clone());
        assert_eq!(push().await?.status().as_u16(), 410);
        assert_eq!(harness.pushes().len(), 1);
        assert_eq!(
            harness
                .gone_pushes()
                .iter()
                .map(|gone| gone.token.clone())
                .collect::<Vec<_>>(),
            vec![token]
        );
        Ok(())
    }
}
