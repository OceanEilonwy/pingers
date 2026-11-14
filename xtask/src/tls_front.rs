// SPDX-License-Identifier: AGPL-3.0-only
//! `cargo xtask tls-front <caddy.json>`: the local stack's TLS front when Caddy is not
//! installed. It serves exactly the routes of a Caddy JSON configuration of the shape of
//! `packaging/nodo/dev/caddy.json`, and refuses anything else in such a file it does not
//! understand, so the two cannot drift silently:
//!
//! - one certificate from `apps.tls.certificates.load_files` (leaf chain and key, PEM);
//! - each server in `apps.http.servers`: its first `listen` address, and its routes in order,
//!   each matching every request or a `path` list (exact, or a `/prefix/*` pattern);
//! - two handlers: `static_response` with a `status_code`, and `reverse_proxy` to one
//!   upstream `dial` address, over h2c (`transport.versions = ["h2c"]`) or HTTP/1.1.
//!
//! What matters for Pingers is kept: clients may speak HTTP/2 (ALPN `h2`) or HTTP/1.1, and
//! request and response bodies are streamed frame by frame in both directions without
//! buffering, as Caddy's `flush_interval: -1` does, so a Meetup's chunked OHTTP live channel
//! works through it. Like Caddy it adds `X-Forwarded-For`, `X-Forwarded-Proto` and
//! `X-Forwarded-Host`; the gateway ignores them (no `trusted_proxies`).

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use hyper::body::Incoming;
use hyper::header::{self, HeaderName, HeaderValue};
use hyper::{Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::Outcome;

/// How a route answers.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    /// A bodiless response with this status.
    Status(StatusCode),
    /// Forwarded to an upstream.
    Proxy(Upstream),
}

/// Where and how a route forwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Upstream {
    address: SocketAddr,
    protocol: Protocol,
}

/// The upstream's HTTP version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    /// HTTP/2 with prior knowledge, over plain TCP.
    H2c,
    /// HTTP/1.1.
    Http1,
}

/// A Caddy path matcher entry.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PathMatch {
    Exact(String),
    /// `"/ohttp/*"`: the prefix including its trailing slash.
    Prefix(String),
}

impl PathMatch {
    fn parse(pattern: &str) -> Result<Self> {
        match pattern.strip_suffix('*') {
            Some(prefix) if !prefix.contains('*') => Ok(Self::Prefix(prefix.to_owned())),
            None if !pattern.contains('*') => Ok(Self::Exact(pattern.to_owned())),
            _ => bail!("path pattern {pattern:?}: only exact paths and trailing * are supported"),
        }
    }

    fn matches(&self, path: &str) -> bool {
        match self {
            Self::Exact(exact) => path == exact,
            Self::Prefix(prefix) => path.starts_with(prefix.as_str()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Route {
    /// `None` matches every request.
    paths: Option<Vec<PathMatch>>,
    action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Site {
    name: String,
    listen: SocketAddr,
    routes: Vec<Route>,
}

impl Site {
    fn route(&self, path: &str) -> Option<&Action> {
        self.routes
            .iter()
            .find(|route| {
                route
                    .paths
                    .as_ref()
                    .is_none_or(|paths| paths.iter().any(|pattern| pattern.matches(path)))
            })
            .map(|route| &route.action)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Front {
    certificate: String,
    key: String,
    sites: Vec<Site>,
}

/// `cargo xtask tls-front <caddy.json>`: serves until killed.
///
/// # Errors
///
/// When the file is not of the supported shape, the certificate cannot be loaded, or a port
/// cannot be bound.
pub(crate) fn run(config: &Path) -> Result<Outcome> {
    let text = std::fs::read_to_string(config)
        .with_context(|| format!("cannot read {}", config.display()))?;
    let front = parse(&serde_json::from_str(&text)?)?;
    let certificates = CertificateDer::pem_file_iter(&front.certificate)
        .with_context(|| front.certificate.clone())?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| front.certificate.clone())?;
    let key = PrivateKeyDer::from_pem_file(&front.key).with_context(|| front.key.clone())?;
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_no_client_auth()
    .with_single_cert(certificates, key)?;
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let mut serving = tokio::task::JoinSet::new();
        for site in front.sites {
            let listener = TcpListener::bind(site.listen)
                .await
                .with_context(|| format!("{}: cannot bind {}", site.name, site.listen))?;
            serving.spawn(serve_site(listener, Arc::new(site), acceptor.clone()));
        }
        // Each site serves until the process is killed; one that fails stops the front.
        match serving.join_next().await {
            Some(Ok(result)) => result,
            Some(Err(error)) => Err(anyhow!("a site task failed: {error}")),
            None => Ok(()),
        }
    })?;
    Ok(Outcome::Passed)
}

async fn serve_site(listener: TcpListener, site: Arc<Site>, acceptor: TlsAcceptor) -> Result<()> {
    let mut h2c = HttpConnector::new();
    h2c.enforce_http(true);
    let clients = Clients {
        h2c: Client::builder(TokioExecutor::new())
            .http2_only(true)
            .build(h2c.clone()),
        http1: Client::builder(TokioExecutor::new()).build(h2c),
    };
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let (stream, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let site = Arc::clone(&site);
        let clients = clients.clone();
        // Finished connections are reaped as new ones arrive, so the set stays small.
        while connections.try_join_next().is_some() {}
        connections.spawn(async move {
            let Ok(tls) = acceptor.accept(stream).await else {
                return;
            };
            let service = hyper::service::service_fn(move |request| {
                forward(request, Arc::clone(&site), clients.clone(), peer)
            });
            let _closed = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(tls), service)
                .await;
        });
    }
}

#[derive(Clone)]
struct Clients {
    h2c: Client<HttpConnector, Incoming>,
    http1: Client<HttpConnector, Incoming>,
}

/// The front's own answers: an empty body.
type FrontBody = http_body_util::Either<Incoming, http_body_util::Empty<hyper::body::Bytes>>;

async fn forward(
    request: Request<Incoming>,
    site: Arc<Site>,
    clients: Clients,
    peer: SocketAddr,
) -> Result<Response<FrontBody>, std::convert::Infallible> {
    let action = site.route(request.uri().path()).cloned();
    let response = match action {
        Some(Action::Status(status)) => status_response(status),
        Some(Action::Proxy(upstream)) => match proxy(request, upstream, &clients, peer).await {
            Ok(response) => response.map(http_body_util::Either::Left),
            Err(_error) => status_response(StatusCode::BAD_GATEWAY),
        },
        None => status_response(StatusCode::NOT_FOUND),
    };
    Ok(response)
}

fn status_response(status: StatusCode) -> Response<FrontBody> {
    let mut response = Response::new(http_body_util::Either::Right(http_body_util::Empty::new()));
    *response.status_mut() = status;
    response
}

/// Headers that belong to one hop (RFC 9110 §7.6.1) and are not forwarded.
const HOP_BY_HOP: [&str; 7] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "proxy-authenticate",
    "proxy-authorization",
];

async fn proxy(
    request: Request<Incoming>,
    upstream: Upstream,
    clients: &Clients,
    peer: SocketAddr,
) -> Result<Response<Incoming>> {
    let (mut parts, body) = request.into_parts();
    let authority = parts
        .uri
        .authority()
        .map(ToString::to_string)
        .or_else(|| {
            parts
                .headers
                .get(header::HOST)
                .and_then(|host| host.to_str().ok())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let path = parts
        .uri
        .path_and_query()
        .map_or("/", hyper::http::uri::PathAndQuery::as_str);
    parts.uri = Uri::try_from(format!("http://{}{path}", upstream.address))?;
    for name in HOP_BY_HOP {
        parts.headers.remove(name);
    }
    if parts
        .headers
        .get(header::TE)
        .is_some_and(|te| te.as_bytes() != b"trailers")
    {
        parts.headers.remove(header::TE);
    }
    // HTTP/2 carries the authority in the URI; HTTP/1.1 upstreams get it as Host.
    parts.headers.remove(header::HOST);
    if upstream.protocol == Protocol::Http1 {
        parts.version = hyper::Version::HTTP_11;
        if let Ok(host) = HeaderValue::from_str(&authority) {
            parts.headers.insert(header::HOST, host);
        }
    } else {
        parts.version = hyper::Version::HTTP_2;
    }
    for (name, value) in [
        ("x-forwarded-for", peer.ip().to_string()),
        ("x-forwarded-proto", "https".to_owned()),
        ("x-forwarded-host", authority),
    ] {
        if let Ok(value) = HeaderValue::from_str(&value) {
            parts.headers.insert(HeaderName::from_static(name), value);
        }
    }
    let request = Request::from_parts(parts, body);
    let client = match upstream.protocol {
        Protocol::H2c => &clients.h2c,
        Protocol::Http1 => &clients.http1,
    };
    let mut response = client.request(request).await?;
    for name in HOP_BY_HOP {
        response.headers_mut().remove(name);
    }
    Ok(response)
}

fn parse(config: &Value) -> Result<Front> {
    let files = config
        .pointer("/apps/tls/certificates/load_files")
        .and_then(Value::as_array)
        .context("apps.tls.certificates.load_files is missing")?;
    let [file] = files.as_slice() else {
        bail!("exactly one certificate in load_files is supported");
    };
    let certificate = string(file, "certificate")?;
    let key = string(file, "key")?;
    let servers = config
        .pointer("/apps/http/servers")
        .and_then(Value::as_object)
        .context("apps.http.servers is missing")?;
    let mut sites = Vec::with_capacity(servers.len());
    for (name, server) in servers {
        let listen = server
            .get("listen")
            .and_then(Value::as_array)
            .and_then(|listen| listen.first())
            .and_then(Value::as_str)
            .with_context(|| format!("{name}: no listen address"))?
            .parse()
            .with_context(|| format!("{name}: listen is not an address and port"))?;
        let routes = server
            .get("routes")
            .and_then(Value::as_array)
            .with_context(|| format!("{name}: no routes"))?
            .iter()
            .map(route)
            .collect::<Result<Vec<_>>>()
            .with_context(|| name.clone())?;
        sites.push(Site {
            name: name.clone(),
            listen,
            routes,
        });
    }
    Ok(Front {
        certificate,
        key,
        sites,
    })
}

fn route(route: &Value) -> Result<Route> {
    let paths = match route.get("match") {
        None => None,
        Some(matchers) => {
            let matchers = matchers.as_array().context("match is not a list")?;
            let mut paths = Vec::new();
            for matcher in matchers {
                let object = matcher.as_object().context("a matcher is not an object")?;
                for (kind, list) in object {
                    if kind != "path" {
                        bail!("matcher {kind:?} is not supported");
                    }
                    for pattern in list.as_array().context("path is not a list")? {
                        paths.push(PathMatch::parse(
                            pattern.as_str().context("a path is not a string")?,
                        )?);
                    }
                }
            }
            Some(paths)
        }
    };
    let handlers = route
        .get("handle")
        .and_then(Value::as_array)
        .context("a route has no handle list")?;
    let [handler] = handlers.as_slice() else {
        bail!("exactly one handler per route is supported");
    };
    let action = match string(handler, "handler")?.as_str() {
        "static_response" => {
            let code = handler
                .get("status_code")
                .and_then(|code| match code {
                    Value::String(text) => text.parse::<u16>().ok(),
                    Value::Number(number) => number.as_u64().and_then(|n| u16::try_from(n).ok()),
                    Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
                })
                .context("static_response needs a status_code")?;
            Action::Status(StatusCode::from_u16(code)?)
        }
        "reverse_proxy" => {
            let upstreams = handler
                .get("upstreams")
                .and_then(Value::as_array)
                .context("reverse_proxy needs upstreams")?;
            let [upstream] = upstreams.as_slice() else {
                bail!("exactly one upstream is supported");
            };
            let address = string(upstream, "dial")?
                .parse()
                .context("dial is not an address and port")?;
            let h2c = handler
                .pointer("/transport/versions")
                .and_then(Value::as_array)
                .is_some_and(|versions| versions.iter().any(|version| version == "h2c"));
            Action::Proxy(Upstream {
                address,
                protocol: if h2c { Protocol::H2c } else { Protocol::Http1 },
            })
        }
        other => bail!("handler {other:?} is not supported"),
    };
    Ok(Route { paths, action })
}

fn string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{field} is missing or not a string"))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn dev_front() -> Result<Front> {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../packaging/nodo/dev/caddy.json"),
        )?;
        parse(&serde_json::from_str(&text)?)
    }

    fn site<'front>(front: &'front Front, name: &str) -> Result<&'front Site> {
        front
            .sites
            .iter()
            .find(|site| site.name == name)
            .context("no such site")
    }

    #[test]
    fn the_dev_caddy_file_is_understood_whole() -> Result<()> {
        let front = dev_front()?;
        assert_eq!(front.certificate, "target/dev-ca/leaf.pem");
        assert_eq!(front.sites.len(), 4);
        Ok(())
    }

    #[test]
    fn the_gateway_and_metrics_are_hidden_behind_the_public_server() -> Result<()> {
        let front = dev_front()?;
        let server = site(&front, "server")?;
        for hidden in ["/ohttp", "/ohttp/x", "/metrics"] {
            assert_eq!(
                server.route(hidden),
                Some(&Action::Status(StatusCode::NOT_FOUND)),
                "{hidden}"
            );
        }
        assert_eq!(
            server.route("/v1/params"),
            Some(&Action::Proxy(Upstream {
                address: SocketAddr::from(([127, 0, 0, 1], 9470)),
                protocol: Protocol::H2c,
            }))
        );
        // "/ohttpx" is not "/ohttp" nor under "/ohttp/".
        assert!(matches!(server.route("/ohttpx"), Some(Action::Proxy(_))));
        Ok(())
    }

    #[test]
    fn the_push_relay_hides_push_and_speaks_http1() -> Result<()> {
        let front = dev_front()?;
        let push = site(&front, "push")?;
        assert_eq!(
            push.route("/v1/push"),
            Some(&Action::Status(StatusCode::NOT_FOUND))
        );
        assert!(matches!(
            push.route("/v1/config"),
            Some(Action::Proxy(Upstream {
                protocol: Protocol::Http1,
                ..
            }))
        ));
        Ok(())
    }

    #[test]
    fn unsupported_caddy_features_are_refused() -> Result<()> {
        let mut config: Value = serde_json::from_str(&std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../packaging/nodo/dev/caddy.json"),
        )?)?;
        let handler = config
            .pointer_mut("/apps/http/servers/maps/routes/0/handle/0/handler")
            .context("maps handler")?;
        *handler = Value::String("file_server".to_owned());
        assert!(parse(&config).is_err());
        Ok(())
    }
}
