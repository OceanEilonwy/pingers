// SPDX-License-Identifier: AGPL-3.0-only
//! Direct HTTPS for the identified class, over real TLS: the hybrid post-quantum key
//! exchange, certificate checking, and the router keeping classes apart.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use pingers_ohttp::{BinaryRequest, Header};
use pingers_transport::{
    DirectConfig, DirectTransport, PrivacyClass, Router, Routes, TlsRoots, Transport,
    TransportError, TransportKind,
};
use rustls::crypto::{CryptoProvider, aws_lc_rs};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{NamedGroup, ServerConfig};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;

use crate::support::{STEP, TestBody, TestError, request, world};

/// An HTTPS server for `localhost` that records the key exchange group of each handshake and
/// answers every request with 201 and `registered <path>`.
struct TlsServer {
    address: SocketAddr,
    certificate: Vec<u8>,
    groups: Arc<Mutex<Vec<Option<NamedGroup>>>>,
    _tasks: JoinSet<()>,
}

async fn tls_server() -> Result<TlsServer, TestError> {
    let issued = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let certificate = issued.cert.der().to_vec();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(issued.signing_key.serialize_der()));
    // A server that supports the hybrid group, as rustls with aws-lc-rs does by default.
    let provider = CryptoProvider {
        kx_groups: vec![
            aws_lc_rs::kx_group::X25519MLKEM768,
            aws_lc_rs::kx_group::X25519,
        ],
        ..aws_lc_rs::default_provider()
    };
    let mut config = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![issued.cert.der().clone()], key)?;
    config.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let groups = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&groups);
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let mut connections = JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let recorded = Arc::clone(&recorded);
            connections.spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let group = stream
                    .get_ref()
                    .1
                    .negotiated_key_exchange_group()
                    .map(rustls::crypto::SupportedKxGroup::name);
                if let Ok(mut groups) = recorded.lock() {
                    groups.push(group);
                }
                let service = hyper::service::service_fn(|request: Request<Incoming>| async move {
                    let (sender, body) = tokio::sync::mpsc::unbounded_channel();
                    let reply = format!("registered {}", request.uri().path());
                    let _ = sender.send(reply.into_bytes().into());
                    let mut response = Response::new(TestBody::Channel(body));
                    *response.status_mut() = hyper::StatusCode::CREATED;
                    Ok::<_, Infallible>(response)
                });
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    Ok(TlsServer {
        address,
        certificate,
        groups,
        _tasks: tasks,
    })
}

fn direct(server: &TlsServer, roots: TlsRoots) -> Result<DirectTransport, TestError> {
    let clock = pingers_time::ManualClock::new(pingers_time::UnixSeconds::EPOCH);
    Ok(DirectTransport::new(
        &DirectConfig::new(
            format!("https://localhost:{}", server.address.port()),
            roots,
        ),
        Arc::new(clock),
    )?)
}

fn registration() -> BinaryRequest {
    BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/accounts".to_owned(),
        headers: vec![Header {
            name: "content-type".to_owned(),
            value: b"application/cbor".to_vec(),
        }],
        body: vec![1, 2, 3],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_https_negotiates_the_hybrid_post_quantum_key_exchange() -> Result<(), TestError> {
    let server = tls_server().await?;
    let transport = direct(
        &server,
        TlsRoots::from_der(std::slice::from_ref(&server.certificate))?,
    )?;
    let response = tokio::time::timeout(
        STEP,
        transport.request(PrivacyClass::Identified, registration()),
    )
    .await??;
    assert_eq!(response.status.get(), 201);
    assert_eq!(response.body, b"registered /v1/accounts");
    let groups = server
        .groups
        .lock()
        .map_err(|_| TestError::Setup("lock"))?
        .clone();
    assert_eq!(groups, [Some(NamedGroup::X25519MLKEM768)]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_untrusted_certificate_is_refused() -> Result<(), TestError> {
    let server = tls_server().await?;
    let transport = direct(&server, TlsRoots::from_der(&[])?)?;
    let result = tokio::time::timeout(
        STEP,
        transport.request(PrivacyClass::Identified, registration()),
    )
    .await?;
    assert!(matches!(result, Err(TransportError::Connection)));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_router_sends_identified_direct_and_anonymous_through_the_relay()
-> Result<(), TestError> {
    let world = world().await?;
    let server = tls_server().await?;
    let identified = Arc::new(direct(
        &server,
        TlsRoots::from_der(std::slice::from_ref(&server.certificate))?,
    )?);
    let relayed = Arc::new(world.client()?);
    let router = Router::new(Routes {
        anonymous: relayed.clone(),
        identified,
        stream: relayed,
    })?;
    assert_eq!(
        router.kind_for(PrivacyClass::Identified),
        TransportKind::Direct
    );

    let anonymous = tokio::time::timeout(
        STEP,
        router.request(PrivacyClass::Anonymous, request("/v1/board/sync", b"x")),
    )
    .await??;
    assert_eq!(anonymous.body, b"seen /v1/board/sync x");
    let identified = tokio::time::timeout(
        STEP,
        router.request(PrivacyClass::Identified, registration()),
    )
    .await??;
    assert_eq!(identified.body, b"registered /v1/accounts");

    // The relay saw only the anonymous request; the TLS server only the identified one.
    assert_eq!(world.gateway.seen().inner_paths, ["/v1/board/sync"]);
    assert_eq!(
        server
            .groups
            .lock()
            .map_err(|_| TestError::Setup("lock"))?
            .len(),
        1
    );
    Ok(())
}
