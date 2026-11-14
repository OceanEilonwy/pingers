// SPDX-License-Identifier: AGPL-3.0-only
//! The harness push relay over TLS: pingers-proto's `RelayUrl` takes only `https://`, so the
//! relay gets a certificate from a test CA made with `rcgen`, and pingers-server is told to
//! trust that CA (`ServerConfig::push_relay_ca`). The https rule is not relaxed anywhere.
//!
//! The CA and certificate keys come from the TLS library's own generator, not the harness
//! seed: they are never pinned, so runs stay reproducible where it matters.

use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;

/// A test CA and the push relay's certificate from it.
pub(super) struct TestTls {
    /// The CA certificate in PEM, for the server's trust store and for test clients.
    pub(super) ca_pem: String,
    /// The relay's TLS configuration (HTTP/2 only).
    pub(super) acceptor: TlsAcceptor,
}

/// Why the TLS material could not be made.
#[derive(Debug)]
pub(super) struct TlsSetupFailed;

impl From<rcgen::Error> for TlsSetupFailed {
    fn from(_: rcgen::Error) -> Self {
        Self
    }
}

impl From<rustls::Error> for TlsSetupFailed {
    fn from(_: rustls::Error) -> Self {
        Self
    }
}

/// A CA, and a certificate from it for `address` (as an IP subject alternative name) and
/// `localhost`.
pub(super) fn test_tls(address: IpAddr) -> Result<TestTls, TlsSetupFailed> {
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "pingers-testkit harness CA");
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate()?)?;
    let leaf_key = KeyPair::generate()?;
    let leaf = CertificateParams::new(vec![address.to_string(), "localhost".to_owned()])?
        .signed_by(&leaf_key, &ca)?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(leaf.der().to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
        )?;
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(TestTls {
        ca_pem: ca.pem(),
        acceptor: TlsAcceptor::from(Arc::new(config)),
    })
}

/// Serves `router` over TLS and HTTP/2 on `listener` until `shutdown` completes, then drops
/// every connection.
pub(super) async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    router: Router,
    shutdown: impl Future<Output = ()>,
) {
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else {
                    continue;
                };
                let acceptor = acceptor.clone();
                let service = TowerToHyperService::new(router.clone());
                connections.spawn(async move {
                    // A failed handshake affects only that connection.
                    let Ok(stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
}

/// Terminates TLS on `listener` and forwards each connection's bytes to `target` (a plain
/// HTTP/2 listener) until `shutdown` completes: the server's TLS front.
pub(super) async fn forward(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    target: std::net::SocketAddr,
    shutdown: impl Future<Output = ()>,
) {
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else {
                    continue;
                };
                let acceptor = acceptor.clone();
                connections.spawn(async move {
                    // A failed handshake or connection affects only that connection.
                    let Ok(mut client) = acceptor.accept(stream).await else {
                        return;
                    };
                    let Ok(mut upstream) = tokio::net::TcpStream::connect(target).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
}
