// SPDX-License-Identifier: AGPL-3.0-only
//! Shared fixtures for the relay's integration tests: a fake opener, the RFC 7748 key pair,
//! and an in-process relay on loopback.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use pingers_crypto::{PushBind, PushRelayPrivateKey, PushRelayPublicKey};
use pingers_proto::{PushTokenPlaintext, SealedPushBlob};
use pingers_push_relay::credentials::{ServerCredential, ServerEntry, ServerRegistry};
use pingers_push_relay::delivery::PushSender;
use pingers_push_relay::keys::{KeyLifetime, KeyRing, RelayKey};
use pingers_push_relay::opener::{OpenError, TokenOpener};
use pingers_push_relay::rate::RateLimit;
use pingers_push_relay::service::{Relay, RelayParts};
use pingers_time::{Clock, UnixSeconds};
use zeroize::Zeroizing;

pub(crate) type TestResult = Result<(), Box<dyn std::error::Error>>;

/// RFC 7748 §6.1, Alice's X25519 private key.
pub(crate) const RELAY_PRIVATE: [u8; 32] = [
    0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d, 0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2, 0x66, 0x45,
    0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a, 0xb1, 0x77, 0xfb, 0xa5, 0x1d, 0xb9, 0x2c, 0x2a,
];
/// RFC 7748 §6.1, Alice's public key.
pub(crate) const RELAY_PUBLIC: [u8; 32] = [
    0x85, 0x20, 0xf0, 0x09, 0x89, 0x30, 0xa7, 0x54, 0x74, 0x8b, 0x7d, 0xdc, 0xb4, 0x3e, 0xf7, 0x5c,
    0x0d, 0xbf, 0x3a, 0x0d, 0x26, 0x38, 0x1a, 0xf4, 0xeb, 0xa4, 0xa9, 0x8e, 0xaa, 0x9b, 0x4e, 0x6a,
];
pub(crate) const KEY_ID: u8 = 1;
pub(crate) const CREDENTIAL: &str = "integration-test-credential-0123456789";

/// Opens only the blobs it was given, each under its own bind.
#[derive(Default)]
pub(crate) struct FakeOpener {
    sealed: HashMap<([u8; 32], [u8; 320]), PushTokenPlaintext>,
}

impl FakeOpener {
    pub(crate) fn seal(
        &mut self,
        bind: [u8; 32],
        blob_byte: u8,
        plaintext: PushTokenPlaintext,
    ) -> SealedPushBlob {
        let blob = [blob_byte; 320];
        self.sealed.insert((bind, blob), plaintext);
        SealedPushBlob::from_bytes(blob)
    }
}

impl TokenOpener for FakeOpener {
    fn open(
        &self,
        _key: &PushRelayPrivateKey,
        bind: &PushBind,
        blob: &SealedPushBlob,
    ) -> Result<PushTokenPlaintext, OpenError> {
        self.sealed
            .get(&(*bind.as_bytes(), *blob.as_bytes()))
            .cloned()
            .ok_or(OpenError::DoesNotOpen)
    }
}

pub(crate) fn key_ring() -> Result<KeyRing, Box<dyn std::error::Error>> {
    Ok(KeyRing::new(vec![RelayKey::new(
        PushRelayPrivateKey::from_bytes(Zeroizing::new(RELAY_PRIVATE)),
        PushRelayPublicKey::new(KEY_ID, RELAY_PUBLIC),
        KeyLifetime {
            activated_at: UnixSeconds::EPOCH,
            retired_at: None,
        },
    )])?)
}

pub(crate) fn per_minute(count: u32) -> RateLimit {
    RateLimit::per_minute(NonZeroU32::new(count).unwrap_or(NonZeroU32::MIN))
}

pub(crate) fn servers(per_minute_limit: u32) -> Result<ServerRegistry, Box<dyn std::error::Error>> {
    Ok(ServerRegistry::new(vec![ServerEntry {
        server_id: "pingers.test".to_owned(),
        credential: ServerCredential::new(Zeroizing::new(CREDENTIAL.as_bytes().to_vec())),
        limit: per_minute(per_minute_limit),
    }])?)
}

/// A relay served on loopback; the server task is aborted when this is dropped.
pub(crate) struct RunningRelay {
    pub(crate) address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl RunningRelay {
    pub(crate) async fn start<O: TokenOpener, S: PushSender, C: Clock + 'static>(
        opener: O,
        sender: S,
        clock: C,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let relay = Arc::new(Relay::new(RelayParts {
            opener,
            sender,
            clock,
            keys: key_ring()?,
            servers: servers(1000)?,
            token_limit: per_minute(2),
            max_ttl: Duration::from_hours(24 * 30),
            operator: "Integration Test Relay".to_owned(),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let router = pingers_push_relay::http::router(relay);
        let task = tokio::spawn(async move {
            drop(axum::serve(listener, router).await);
        });
        Ok(Self { address, task })
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

impl Drop for RunningRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A UnifiedPush distributor stand-in: records bodies, answers with a fixed status.
pub(crate) struct Distributor {
    pub(crate) address: SocketAddr,
    received: Arc<std::sync::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Distributor {
    pub(crate) async fn start(
        status: axum::http::StatusCode,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_received = Arc::clone(&received);
        let router = axum::Router::new().fallback(move |body: String| {
            let received = Arc::clone(&handler_received);
            async move {
                received
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(body);
                status
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            drop(axum::serve(listener, router).await);
        });
        Ok(Self {
            address,
            received,
            task,
        })
    }

    pub(crate) fn endpoint(&self, topic: &str) -> String {
        format!("http://{}/{topic}?up=1", self.address)
    }

    pub(crate) fn received(&self) -> Vec<String> {
        self.received
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Distributor {
    fn drop(&mut self) {
        self.task.abort();
    }
}
