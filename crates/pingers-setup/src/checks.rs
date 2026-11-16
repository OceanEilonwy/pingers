// SPDX-License-Identifier: AGPL-3.0-only
//! What the setup page knows of the running services, gathered fresh for each request:
//!
//! - On loopback, through the references nodo granted: the gateway's two key configs
//!   (`GET /v1/ohttp/config` and `/v1/ohttp/config/next`, HTTP/2 prior knowledge, as the
//!   gateway listener speaks), the push relay's published and next configs (`GET /v1/config`,
//!   `/v1/config/next`) with the platforms it serves, and the map host's catalog.
//! - Through the public names, from the appliance itself: each site's TLS (a handshake that
//!   offers only X25519MLKEM768, so it succeeds only with post-quantum key exchange, verified
//!   against Mozilla's roots; HTTP/2 offered by the server and the relay) and what each answers
//!   (`/v1/params`, the relay's key config, the push relay's config, the map catalog, and `404`
//!   for the paths that must never be public). Many routers do not loop connections from the
//!   inside back to the public address; then these say they could not be tried from here.
//!
//! Every request has a short timeout and they all run at once, so a page is ready in seconds
//! whatever is down.

use std::sync::Arc;
use std::time::Duration;

use pingers_ohttp::KeyConfig;
use pingers_proto::{KeyConfigHash, PushPlatform, PushRelayConfig, WireDecode as _};
use reqwest::{Client, StatusCode};

use crate::config::{Config, Site};

/// Per request, connection included.
const TIMEOUT: Duration = Duration::from_secs(6);

/// Larger than any key config, push relay config or catalog this page reads.
pub(crate) const MAX_BODY_BYTES: usize = 256 * 1024;

/// A check's outcome, as the owner page shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Passed.
    Ok,
    /// Failed: something to fix.
    Failed,
    /// Not done yet, such as a certificate still on its way.
    Waiting,
    /// Could not be tried from the appliance.
    Unknown,
}

/// One check: what was tried, how it went, and a sentence saying more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// What was checked, in words.
    pub label: String,
    /// The outcome.
    pub state: State,
    /// More about it.
    pub detail: Option<String>,
}

/// The longest detail nodo's owner page takes, in characters.
pub const MAX_DETAIL_CHARS: usize = 512;

impl Check {
    /// A check; its detail is cut to [`MAX_DETAIL_CHARS`] (an error message can be long).
    #[must_use]
    pub fn new(label: impl Into<String>, state: State, detail: impl Into<String>) -> Self {
        let detail: String = detail.into();
        let detail = if detail.chars().count() > MAX_DETAIL_CHARS {
            let mut cut: String = detail
                .chars()
                .take(MAX_DETAIL_CHARS.saturating_sub(1))
                .collect();
            cut.push('\u{2026}');
            cut
        } else {
            detail
        };
        Self {
            label: label.into(),
            state,
            detail: Some(detail).filter(|detail| !detail.is_empty()),
        }
    }
}

/// The push relay as it answers on loopback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRelayFacts {
    /// The published key's pin, then the next key's when one is scheduled.
    pub pins: Vec<KeyConfigHash>,
    /// Whether it serves FCM, which it does once the service account is pasted.
    pub fcm: bool,
}

/// Everything the page is built from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// The gateway's served key's pin, then the other slot's; or why they could not be read.
    pub gateway_pins: Result<Vec<KeyConfigHash>, String>,
    /// The push relay; or why it could not be read.
    pub push_relay: Result<PushRelayFacts, String>,
    /// How many regions the map host serves; or why it could not be read.
    pub map_regions: Result<usize, String>,
    /// The checks through the public names, in the order of [`Config::origins`].
    pub public: Vec<Check>,
}

/// The clients the checks use.
pub struct Clients {
    /// HTTP/1.1, and HTTPS with the system's roots.
    plain: Client,
    /// HTTP/2 with prior knowledge, for the gateway's listener.
    h2c: Client,
    /// TLS that offers only X25519MLKEM768, with Mozilla's roots.
    post_quantum: Arc<rustls::ClientConfig>,
}

impl Clients {
    /// Builds the clients.
    ///
    /// # Errors
    ///
    /// The HTTP client could not be built, or the TLS provider lacks X25519MLKEM768.
    pub fn new() -> Result<Self, String> {
        Self::trusting(&[])
    }

    /// The clients, also trusting `extra` (DER certificates) besides the usual roots: for
    /// tests against a local TLS server.
    ///
    /// # Errors
    ///
    /// As [`Clients::new`], or a certificate is not DER.
    pub fn trusting(extra: &[Vec<u8>]) -> Result<Self, String> {
        let certificates = extra
            .iter()
            .map(|der| reqwest::Certificate::from_der(der).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let plain = Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .tls_certs_merge(certificates)
            .build()
            .map_err(|error| error.to_string())?;
        let h2c = Client::builder()
            .timeout(TIMEOUT)
            .http2_prior_knowledge()
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            plain,
            h2c,
            post_quantum: Arc::new(post_quantum_tls(extra)?),
        })
    }
}

/// A TLS client that offers only X25519MLKEM768 and HTTP/2 then HTTP/1.1, verifying against
/// Mozilla's roots (bundled, so nothing depends on the sandbox's files).
fn post_quantum_tls(extra: &[Vec<u8>]) -> Result<rustls::ClientConfig, String> {
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    provider
        .kx_groups
        .retain(|group| group.name() == rustls::NamedGroup::X25519MLKEM768);
    if provider.kx_groups.is_empty() {
        return Err("the TLS library has no X25519MLKEM768".to_owned());
    }
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    for der in extra {
        roots
            .add(rustls::pki_types::CertificateDer::from(der.clone()))
            .map_err(|error| error.to_string())?;
    }
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| error.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// Gathers every fact at once.
pub async fn gather(config: &Config, clients: &Clients) -> Facts {
    let gateway = gateway_pins(clients, &config.gateway_key_config);
    let push = push_relay(clients, &config.push_relay);
    let catalog = format!("{}/catalog.json", config.map_host_local);
    let maps = map_regions(clients, &catalog);
    let public = public_checks(config, clients);
    let (gateway_pins, push_relay, map_regions, public) = tokio::join!(gateway, push, maps, public);
    let known = Known {
        gateway: gateway_pins.as_ref().ok().and_then(|pins| pins.first()),
        push: push_relay
            .as_ref()
            .ok()
            .and_then(|facts| facts.pins.first()),
    };
    let public = with_pin_checks(public, config, clients, known).await;
    Facts {
        gateway_pins,
        push_relay,
        map_regions,
        public,
    }
}

/// The body of a `200`, `None` for a `404`, an error otherwise.
async fn get(client: &Client, url: &str) -> Result<Option<Vec<u8>>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "not answering".to_owned())?;
    match response.status() {
        StatusCode::OK => {}
        StatusCode::NOT_FOUND => return Ok(None),
        status => return Err(format!("answered {status}")),
    }
    let body = response
        .bytes()
        .await
        .map_err(|_| "stopped answering".to_owned())?;
    if body.len() > MAX_BODY_BYTES {
        return Err("answered too much".to_owned());
    }
    Ok(Some(body.to_vec()))
}

/// The gateway's served key's pin, then the other slot's.
async fn gateway_pins(clients: &Clients, served_url: &str) -> Result<Vec<KeyConfigHash>, String> {
    let served = get(&clients.h2c, served_url)
        .await
        .map_err(|why| format!("pingers-server's gateway is {why}"))?
        .ok_or("pingers-server's gateway has no key config")?;
    let decode = |body: &[u8]| {
        KeyConfig::decode(body)
            .map(|config| config.hash())
            .map_err(|_| "pingers-server's gateway sent a bad key config".to_owned())
    };
    let mut pins = vec![decode(&served)?];
    let next_url = format!("{served_url}/next");
    if let Some(next) = get(&clients.h2c, &next_url)
        .await
        .map_err(|why| format!("pingers-server's gateway is {why}"))?
    {
        pins.push(decode(&next)?);
    }
    Ok(pins)
}

/// The pin of a push relay config, and the config.
fn push_config(body: &[u8]) -> Result<(KeyConfigHash, PushRelayConfig), String> {
    let config = PushRelayConfig::decode(body)
        .map_err(|_| "pingers-push-relay sent a bad config".to_owned())?;
    Ok((
        pingers_crypto::key_config_hash(&config.key_config.to_bytes()),
        config,
    ))
}

/// The push relay's published and next keys and whether it serves FCM.
async fn push_relay(clients: &Clients, origin: &str) -> Result<PushRelayFacts, String> {
    let published = get(&clients.plain, &format!("{origin}/v1/config"))
        .await
        .map_err(|why| format!("pingers-push-relay is {why}"))?
        .ok_or("pingers-push-relay has no published key")?;
    let (pin, config) = push_config(&published)?;
    let mut pins = vec![pin];
    if let Some(next) = get(&clients.plain, &format!("{origin}/v1/config/next"))
        .await
        .map_err(|why| format!("pingers-push-relay is {why}"))?
    {
        // The next config is the published one with an activation time: the same key fields.
        let scheduled = pingers_proto::ScheduledPushRelayConfig::decode(&next)
            .map_err(|_| "pingers-push-relay sent a bad next config".to_owned())?;
        pins.push(pingers_crypto::key_config_hash(
            &scheduled.config.key_config.to_bytes(),
        ));
    }
    Ok(PushRelayFacts {
        pins,
        fcm: config.platforms.contains(&PushPlatform::Fcm),
    })
}

/// How many regions the map host's catalog lists.
async fn map_regions(clients: &Clients, url: &str) -> Result<usize, String> {
    let body = get(&clients.plain, url)
        .await
        .map_err(|why| format!("pingers-map-host is {why}"))?
        .ok_or("pingers-map-host has no catalog")?;
    let catalog: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| "pingers-map-host sent a bad catalog".to_owned())?;
    catalog
        .get("regions")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| "pingers-map-host sent a bad catalog".to_owned())
}

/// The sentence for a public check that could not be tried from here.
const NO_LOOPBACK: &str = "Could not connect from the appliance. If the site is online and the \
     DNS check passes, your router may not loop connections from inside back to your public \
     address: check from a phone on mobile data instead.";

/// TLS and the public answers of every site.
async fn public_checks(config: &Config, clients: &Clients) -> Vec<Check> {
    let [server, relay, push, maps] = config.origins();
    let tls = futures_join4(
        tls_check(clients, server, true),
        tls_check(clients, relay, true),
        tls_check(clients, push, false),
        tls_check(clients, maps, false),
    );
    let answers = futures_join4(
        status_check(
            clients,
            Probe::get(format!("{}/v1/params", server.origin), StatusCode::OK),
            "The server answers",
        ),
        status_check(
            clients,
            Probe::post(format!("{}/ohttp", server.origin), StatusCode::NOT_FOUND),
            "The gateway is not reachable directly",
        ),
        status_check(
            clients,
            Probe::get(format!("{}/catalog.json", maps.origin), StatusCode::OK),
            "The map host answers",
        ),
        status_check(
            clients,
            Probe::post(format!("{}/v1/push", push.origin), StatusCode::NOT_FOUND),
            "Pushes are not accepted from outside",
        ),
    );
    let (tls, answers) = tokio::join!(tls, answers);
    let mut checks: Vec<Check> = tls.into_iter().collect();
    checks.extend(answers);
    checks
}

/// Four futures at once, in order.
async fn futures_join4<T>(
    first: impl Future<Output = T>,
    second: impl Future<Output = T>,
    third: impl Future<Output = T>,
    fourth: impl Future<Output = T>,
) -> [T; 4] {
    let (first, second, third, fourth) = tokio::join!(first, second, third, fourth);
    [first, second, third, fourth]
}

/// The keys the services hold, as read on loopback.
#[derive(Clone, Copy)]
struct Known<'facts> {
    /// The gateway's served key.
    gateway: Option<&'facts KeyConfigHash>,
    /// The push relay's published key.
    push: Option<&'facts KeyConfigHash>,
}

/// Adds the checks that the relay and the push relay publish the keys this appliance holds.
async fn with_pin_checks(
    mut checks: Vec<Check>,
    config: &Config,
    clients: &Clients,
    known: Known<'_>,
) -> Vec<Check> {
    let [_, relay, push_site, _] = config.origins();
    let relay_url = format!("{}/v1/ohttp/config", relay.origin);
    let push_url = format!("{}/v1/config", push_site.origin);
    let (relay_body, push_body) = tokio::join!(
        get(&clients.plain, &relay_url),
        get(&clients.plain, &push_url)
    );
    checks.push(match (relay_body, known.gateway) {
        (Ok(Some(body)), Some(served)) => match KeyConfig::decode(&body) {
            Ok(config) if config.hash() == *served => Check::new(
                "The relay hands out this appliance's gateway key",
                State::Ok,
                "",
            ),
            _ => Check::new(
                "The relay hands out this appliance's gateway key",
                State::Failed,
                format!("{} hands out a different key", relay.host()),
            ),
        },
        (Ok(_), None) => Check::new(
            "The relay hands out this appliance's gateway key",
            State::Waiting,
            "pingers-server's gateway is not answering on the appliance yet",
        ),
        (Ok(None), Some(_)) => Check::new(
            "The relay hands out this appliance's gateway key",
            State::Failed,
            format!("{} has no key config", relay.host()),
        ),
        (Err(_), _) => Check::new(
            "The relay hands out this appliance's gateway key",
            State::Unknown,
            NO_LOOPBACK,
        ),
    });
    checks.push(match (push_body, known.push) {
        (Ok(Some(body)), Some(published)) => match push_config(&body) {
            Ok((pin, _)) if pin == *published => Check::new(
                "The push relay publishes this appliance's key",
                State::Ok,
                "",
            ),
            _ => Check::new(
                "The push relay publishes this appliance's key",
                State::Failed,
                format!("{} publishes a different key", push_site.host()),
            ),
        },
        (Ok(_), None) => Check::new(
            "The push relay publishes this appliance's key",
            State::Waiting,
            "pingers-push-relay is not answering on the appliance yet",
        ),
        (Ok(None), Some(_)) => Check::new(
            "The push relay publishes this appliance's key",
            State::Failed,
            format!("{} has no published key", push_site.host()),
        ),
        (Err(_), _) => Check::new(
            "The push relay publishes this appliance's key",
            State::Unknown,
            NO_LOOPBACK,
        ),
    });
    checks
}

/// A request to make through a public name, and the status it must get.
struct Probe {
    post: bool,
    url: String,
    expected: StatusCode,
}

impl Probe {
    const fn get(url: String, expected: StatusCode) -> Self {
        Self {
            post: false,
            url,
            expected,
        }
    }

    const fn post(url: String, expected: StatusCode) -> Self {
        Self {
            post: true,
            url,
            expected,
        }
    }
}

/// A request through the public name and whether it answers as expected.
async fn status_check(clients: &Clients, probe: Probe, label: &str) -> Check {
    let Probe {
        post,
        url,
        expected,
    } = probe;
    let request = if post {
        clients.plain.post(&url)
    } else {
        clients.plain.get(&url)
    };
    match request.send().await {
        Ok(response) if response.status() == expected => Check::new(label, State::Ok, ""),
        Ok(response) => Check::new(
            label,
            State::Failed,
            format!("{url} answered {}, not {expected}", response.status()),
        ),
        Err(error) if error.is_connect() || error.is_timeout() => {
            Check::new(label, State::Unknown, NO_LOOPBACK)
        }
        Err(_) => Check::new(
            label,
            State::Failed,
            format!(
                "{url}: the connection failed, such as with a certificate that is not valid yet"
            ),
        ),
    }
}

/// A TLS handshake offering only X25519MLKEM768: the site's certificate is valid for its name,
/// the key exchange is post-quantum and, where asked, HTTP/2 is offered.
async fn tls_check(clients: &Clients, site: Site<'_>, needs_h2: bool) -> Check {
    let host = site.host().to_owned();
    let label = format!("{host}: valid certificate, post-quantum TLS");
    let Ok(name) = rustls::pki_types::ServerName::try_from(host.clone()) else {
        return Check::new(label, State::Failed, "not a host name");
    };
    let connector = tokio_rustls::TlsConnector::from(Arc::clone(&clients.post_quantum));
    let attempt = async {
        let stream = tokio::net::TcpStream::connect((host.as_str(), site.port()))
            .await
            .map_err(|_| None)?;
        connector
            .connect(name, stream)
            .await
            .map_err(|error| Some(error.to_string()))
    };
    match tokio::time::timeout(TIMEOUT, attempt).await {
        Err(_) | Ok(Err(None)) => Check::new(label, State::Unknown, NO_LOOPBACK),
        Ok(Err(Some(why))) => Check::new(
            label,
            State::Failed,
            format!(
                "The handshake failed ({why}). Either the certificate is not issued yet or not \
                 valid, or whatever answers on 443 does not offer X25519MLKEM768: if your \
                 router terminates TLS, set it to pass TLS through to the appliance."
            ),
        ),
        Ok(Ok(stream)) => {
            let (_, connection) = stream.get_ref();
            let h2 = connection.alpn_protocol() == Some(b"h2".as_slice());
            if needs_h2 && !h2 {
                Check::new(
                    label,
                    State::Failed,
                    "HTTP/2 is not offered: if your router terminates TLS, it must speak HTTP/2 to the appliance, or pass TLS through.",
                )
            } else {
                Check::new(label, State::Ok, "")
            }
        }
    }
}
