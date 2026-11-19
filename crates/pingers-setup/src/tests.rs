// SPDX-License-Identifier: AGPL-3.0-only
//! The page served over loopback against fake services: an HTTP/2-only gateway with two keys,
//! a push relay with a next key, and a map host with a catalog. The public names are under
//! `.invalid`, which never resolves, so the public checks say they could not be tried.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use pingers_ohttp::GatewayKey;
use pingers_proto::{
    KeyConfigHash, PushPlatform, PushRelayConfig, PushRelayKeyConfig, PushRelayKeyId,
    ScheduledPushRelayConfig, SetupPayload, TimeToLive, WireEncode as _,
};
use rand_core::SeedableRng as _;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Serves `router` on a loopback port (HTTP/1.1, and HTTP/2 with prior knowledge).
async fn serve_fake(router: Router) -> Result<SocketAddr, std::io::Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(address)
}

fn relay_config(key_id: u8, public: u8) -> Result<PushRelayConfig, pingers_proto::ProtoError> {
    Ok(PushRelayConfig {
        key_config: PushRelayKeyConfig {
            key_id: PushRelayKeyId::new(key_id),
            public_key: [public; 32],
        },
        platforms: vec![PushPlatform::Fcm, PushPlatform::UnifiedPush],
        max_time_to_live: TimeToLive::new(Duration::from_hours(24))?,
        operator: "example.invalid".to_owned(),
    })
}

/// The expected pins, and the setup config pointing at the fakes.
struct Fakes {
    gateway: Vec<KeyConfigHash>,
    push: Vec<KeyConfigHash>,
    map_host: Vec<KeyConfigHash>,
    config: Config,
}

async fn fakes() -> Result<Fakes, Box<dyn std::error::Error>> {
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(7);
    let served = GatewayKey::generate(1, &mut rng)?.config();
    let next = GatewayKey::generate(2, &mut rng)?.config();
    let gateway_pins = vec![served.hash(), next.hash()];
    let (served, next) = (served.encoded().to_vec(), next.encoded().to_vec());
    let gateway = serve_fake(
        Router::new()
            .route(
                "/v1/ohttp/config",
                get(move || std::future::ready(served.clone())),
            )
            .route(
                "/v1/ohttp/config/next",
                get(move || std::future::ready(next.clone())),
            ),
    )
    .await?;
    let published = relay_config(1, 5)?;
    let upcoming = relay_config(2, 6)?;
    let push_pins = [&published, &upcoming]
        .map(|config| pingers_crypto::key_config_hash(&config.key_config.to_bytes()))
        .to_vec();
    let published = published.encode()?;
    let upcoming = ScheduledPushRelayConfig {
        config: upcoming,
        activates_at: pingers_time::UnixSeconds::new(4_102_444_800),
    }
    .encode()?;
    let push = serve_fake(
        Router::new()
            .route(
                "/v1/config",
                get(move || std::future::ready(published.clone())),
            )
            .route(
                "/v1/config/next",
                get(move || std::future::ready(upcoming.clone())),
            ),
    )
    .await?;
    let map_key = GatewayKey::generate(1, &mut rng)?.config();
    let map_host = vec![map_key.hash()];
    let map_key = map_key.encoded().to_vec();
    let maps = serve_fake(
        Router::new()
            .route(
                "/catalog.json",
                get(|| {
                    std::future::ready(
                        r#"{"format":1,"regions":[{"id":"a"},{"id":"b","status":"on_demand"}]}"#,
                    )
                }),
            )
            .route(
                "/ohttp-keys",
                get(move || std::future::ready(map_key.clone())),
            ),
    )
    .await?;
    let text = crate::config::tests::file()
        .replace("example.org", "example.invalid")
        .replace("http://127.0.0.1:9471", &format!("http://{gateway}"))
        .replace("http://127.0.0.1:9472", &format!("http://{push}"))
        .replace("http://127.0.0.1:9474", &format!("http://{maps}"));
    Ok(Fakes {
        gateway: gateway_pins,
        push: push_pins,
        map_host,
        config: Config::parse(&text)?,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_facts_come_from_the_services_on_loopback() -> TestResult {
    let Fakes {
        gateway,
        push,
        map_host,
        config,
    } = fakes().await?;
    let facts = checks::gather(&config, &Clients::new()?).await;
    assert_eq!(facts.gateway_pins, Ok(gateway));
    assert_eq!(facts.map_host_pins, Ok(map_host));
    let relay = facts.push_relay.map_err(|why| why.clone())?;
    assert_eq!(relay.pins, push);
    assert!(relay.fcm);
    assert_eq!(
        facts.map_regions,
        Ok(checks::MapCatalog {
            regions: 2,
            on_demand: 1
        })
    );
    // Nothing under .invalid resolves: every public check says it could not be tried, none
    // claims a pass or a failure it did not see.
    assert_eq!(facts.public.len(), 10);
    assert!(
        facts
            .public
            .iter()
            .all(|check| check.state == checks::State::Unknown),
        "{:?}",
        facts.public
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_page_is_served_on_loopback_with_a_working_link() -> TestResult {
    let Fakes {
        gateway,
        push,
        map_host: _,
        config,
    } = fakes().await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = Arc::new(App::new(config)?);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve(listener, app, async move {
        let _ = stopped.await;
    }));
    let client = reqwest::Client::new();
    let response = client.get(format!("http://{address}/owner")).send().await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let page: serde_json::Value = response.json().await?;
    let link = page
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("items")?.as_array())
        .flatten()
        .find(|item| item.get("kind").and_then(serde_json::Value::as_str) == Some("qr"))
        .and_then(|item| item.get("data")?.as_str())
        .ok_or("no QR code")?
        .to_owned();
    let payload = SetupPayload::from_link(&link)?;
    assert_eq!(payload.gateway_pins, gateway);
    assert_eq!(payload.push_relay_pins, push);
    assert_eq!(payload.server.as_str(), "https://pingers.example.invalid");
    // Only the page is served.
    let other = client.get(format!("http://{address}/")).send().await?;
    assert_eq!(other.status(), reqwest::StatusCode::NOT_FOUND);
    let _ = stop.send(());
    server.await??;
    Ok(())
}

/// A TLS server on loopback for `localhost`, with a self-signed certificate (returned, to
/// trust), offering `groups` and HTTP/2 when `h2`, serving `router`.
async fn tls_fake(
    router: Router,
    groups: &[rustls::NamedGroup],
    h2: bool,
) -> Result<(SocketAddr, Vec<u8>), Box<dyn std::error::Error>> {
    let key = rcgen::KeyPair::generate()?;
    let certificate =
        rcgen::CertificateParams::new(vec!["localhost".to_owned()])?.self_signed(&key)?;
    let der = certificate.der().to_vec();
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    provider
        .kx_groups
        .retain(|group| groups.contains(&group.name()));
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(der.clone())],
            rustls::pki_types::PrivateKeyDer::try_from(key.serialize_der())?,
        )?;
    config.alpn_protocols = if h2 {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (acceptor, router) = (acceptor.clone(), router.clone());
            tokio::spawn(async move {
                if let Ok(stream) = acceptor.accept(stream).await {
                    let service = hyper_util::service::TowerToHyperService::new(router);
                    let _ = hyper_util::server::conn::auto::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    )
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
                }
            });
        }
    });
    Ok((address, der))
}

/// The public routes the checks ask for, answering as a healthy appliance does, with the keys
/// given.
fn public_routes(gateway: Vec<u8>, push: Vec<u8>) -> Router {
    Router::new()
        .route("/v1/params", get(|| std::future::ready("params")))
        .route(
            "/catalog.json",
            get(|| std::future::ready("{\"regions\":[]}")),
        )
        .route(
            "/v1/ohttp/config",
            get(move || std::future::ready(gateway.clone())),
        )
        .route("/v1/config", get(move || std::future::ready(push.clone())))
}

/// The fakes' setup config with every public origin at `https://localhost:<port>`.
fn at_localhost(config: &str, port: u16) -> Result<Config, crate::ConfigError> {
    let origin = format!("https://localhost:{port}");
    Config::parse(
        &config
            .replace("https://pingers.example.invalid", &origin)
            .replace("https://ohttp.example.invalid", &origin)
            .replace("https://push.example.invalid", &origin)
            .replace("https://maps.example.invalid", &origin),
    )
}

/// The key configs the fakes serve, for the public side.
fn served_keys() -> Result<(Vec<u8>, Vec<u8>), Box<dyn std::error::Error>> {
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(7);
    let served = GatewayKey::generate(1, &mut rng)?
        .config()
        .encoded()
        .to_vec();
    Ok((served, relay_config(1, 5)?.encode()?))
}

/// The setup config text of `fakes()`, before parsing.
async fn fake_text() -> Result<String, Box<dyn std::error::Error>> {
    let config = fakes().await?.config;
    Ok(format!(
        "listen = \"127.0.0.1:9479\"\ndomain = \"example.invalid\"\n\
         server_id = \"https://pingers.example.invalid\"\n\
         ohttp_relay_url = \"https://ohttp.example.invalid/ohttp\"\n\
         push_relay_url = \"https://push.example.invalid\"\nmap_host = \"https://maps.example.invalid\"\n\
         registration_code = \"0123456789abcdef0123456789abcdef\"\n\
         gateway_key_config = \"{}\"\npush_relay = \"{}\"\nmap_host_local = \"{}\"\n\
         github_repository = \"OceanEilonwy/pingers\"\nrelease_tag = \"v1.0.0-rc.1\"\n",
        config.gateway_key_config, config.push_relay, config.map_host_local
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_healthy_appliance_passes_every_public_check() -> TestResult {
    let (gateway, push) = served_keys()?;
    let (address, der) = tls_fake(
        public_routes(gateway, push),
        &[
            rustls::NamedGroup::X25519MLKEM768,
            rustls::NamedGroup::X25519,
        ],
        true,
    )
    .await?;
    let config = at_localhost(&fake_text().await?, address.port())?;
    let facts = checks::gather(&config, &Clients::trusting(&[der])?).await;
    assert_eq!(facts.public.len(), 10);
    for check in &facts.public {
        assert_eq!(check.state, checks::State::Ok, "{check:?}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classical_tls_missing_http2_and_other_keys_fail() -> TestResult {
    // Someone else's keys, X25519 only, and no HTTP/2: what a terminating router in front of
    // another server would look like.
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(99);
    let other_gateway = GatewayKey::generate(1, &mut rng)?
        .config()
        .encoded()
        .to_vec();
    let other_push = relay_config(1, 9)?.encode()?;
    let (address, der) = tls_fake(
        public_routes(other_gateway, other_push),
        &[rustls::NamedGroup::X25519],
        false,
    )
    .await?;
    let config = at_localhost(&fake_text().await?, address.port())?;
    let facts = checks::gather(&config, &Clients::trusting(std::slice::from_ref(&der))?).await;
    let states: Vec<checks::State> = facts.public.iter().map(|check| check.state).collect();
    // The four TLS checks fail (no post-quantum key exchange); the answers pass; the two key
    // checks fail.
    assert_eq!(
        states,
        [
            checks::State::Failed,
            checks::State::Failed,
            checks::State::Failed,
            checks::State::Failed,
            checks::State::Ok,
            checks::State::Ok,
            checks::State::Ok,
            checks::State::Ok,
            checks::State::Failed,
            checks::State::Failed,
        ]
    );
    // Post-quantum but without HTTP/2: only the server's and the relay's TLS checks fail.
    let (gateway, push) = served_keys()?;
    let (address, der) = tls_fake(
        public_routes(gateway, push),
        &[rustls::NamedGroup::X25519MLKEM768],
        false,
    )
    .await?;
    let config = at_localhost(&fake_text().await?, address.port())?;
    let facts = checks::gather(&config, &Clients::trusting(&[der])?).await;
    let tls: Vec<checks::State> = facts
        .public
        .iter()
        .take(4)
        .map(|check| check.state)
        .collect();
    assert_eq!(
        tls,
        [
            checks::State::Failed,
            checks::State::Failed,
            checks::State::Ok,
            checks::State::Ok
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_untrusted_certificate_fails_rather_than_passing() -> TestResult {
    let (gateway, push) = served_keys()?;
    let (address, _der) = tls_fake(
        public_routes(gateway, push),
        &[rustls::NamedGroup::X25519MLKEM768],
        true,
    )
    .await?;
    let config = at_localhost(&fake_text().await?, address.port())?;
    let facts = checks::gather(&config, &Clients::new()?).await;
    for check in &facts.public {
        assert_ne!(check.state, checks::State::Ok, "{check:?}");
    }
    let first = facts.public.first().ok_or("no check")?;
    assert_eq!(first.state, checks::State::Failed);
    Ok(())
}

/// The fakes' config with the map host's loopback origin at `maps`.
async fn with_map_host(maps: SocketAddr) -> Result<Config, Box<dyn std::error::Error>> {
    let text = fake_text().await?;
    let start = text
        .find("map_host_local = \"")
        .ok_or("no map_host_local")?;
    let line_end = text
        .get(start..)
        .and_then(|rest| rest.find('\n'))
        .ok_or("no line end")?;
    let mut changed = text.clone();
    changed.replace_range(
        start..start.saturating_add(line_end),
        &format!("map_host_local = \"http://{maps}\""),
    );
    Ok(Config::parse(&changed)?)
}

/// A catalog of exactly `length` bytes with no regions.
fn catalog_of(length: usize) -> Result<String, &'static str> {
    let empty = r#"{"regions":[],"pad":""}"#;
    let padding = length.checked_sub(empty.len()).ok_or("too short")?;
    Ok(format!(
        r#"{{"regions":[],"pad":"{}"}}"#,
        "p".repeat(padding)
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_up_to_the_limit_is_read_and_one_byte_more_is_refused() -> TestResult {
    for (length, read) in [
        (checks::MAX_BODY_BYTES, true),
        (checks::MAX_BODY_BYTES.saturating_add(1), false),
    ] {
        let catalog = catalog_of(length)?;
        let maps = serve_fake(Router::new().route(
            "/catalog.json",
            get(move || std::future::ready(catalog.clone())),
        ))
        .await?;
        let config = with_map_host(maps).await?;
        let facts = checks::gather(&config, &Clients::new()?).await;
        assert_eq!(
            facts.map_regions.is_ok(),
            read,
            "{length}: {:?}",
            facts.map_regions
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_public_path_that_answers_when_it_must_not_fails() -> TestResult {
    let (gateway, push) = served_keys()?;
    let routes = public_routes(gateway, push).route(
        "/ohttp",
        axum::routing::post(|| std::future::ready("gateway")),
    );
    let (address, der) = tls_fake(routes, &[rustls::NamedGroup::X25519MLKEM768], true).await?;
    let config = at_localhost(&fake_text().await?, address.port())?;
    let facts = checks::gather(&config, &Clients::trusting(&[der])?).await;
    let gateway_hidden = facts.public.get(5).ok_or("no check")?;
    assert_eq!(
        gateway_hidden.label,
        "The gateway is not reachable directly"
    );
    assert_eq!(gateway_hidden.state, checks::State::Failed);
    assert!(
        gateway_hidden
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("200"))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_address_that_is_no_url_fails_rather_than_being_untried() -> TestResult {
    let text = fake_text().await?.replace(
        "https://maps.example.invalid",
        "https://maps example.invalid",
    );
    let facts = checks::gather(&Config::parse(&text)?, &Clients::new()?).await;
    // The map host's TLS check (the fourth) and its answer (the seventh).
    for index in [3, 6] {
        let check = facts.public.get(index).ok_or("no check")?;
        assert_eq!(check.state, checks::State::Failed, "{check:?}");
    }
    Ok(())
}

/// Set in the child process the next test starts, which runs the test's body.
const NO_SYSTEM_ROOTS: &str = "PINGERS_SETUP_TEST_NO_SYSTEM_ROOTS";

/// The clients are built where the system has no CA certificates, as in a sandbox without
/// `/etc/ssl`. The system's store is read from `SSL_CERT_FILE` and `SSL_CERT_DIR` alone when
/// either is set (rustls-native-certs), so the child gets an empty file and an empty
/// directory: a child process, because the environment is the whole process's and other tests
/// run alongside.
#[test]
fn the_clients_build_without_a_system_ca_store() -> TestResult {
    if std::env::var_os(NO_SYSTEM_ROOTS).is_some() {
        Clients::new()?;
        return Ok(());
    }
    let empty = std::env::temp_dir().join(format!("pingers-setup-no-roots-{}", std::process::id()));
    std::fs::create_dir_all(&empty)?;
    let file = empty.join("none.pem");
    std::fs::write(&file, "")?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "tests::the_clients_build_without_a_system_ca_store",
            "--exact",
            "--nocapture",
        ])
        .env(NO_SYSTEM_ROOTS, "1")
        .env("SSL_CERT_FILE", &file)
        .env("SSL_CERT_DIR", &empty)
        .output()?;
    std::fs::remove_dir_all(&empty)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a client did not build without a system CA store:\n{stdout}\n{stderr}"
    );
    // The child ran this very test, not nothing.
    assert!(stdout.contains("1 passed"), "{stdout}");
    Ok(())
}
