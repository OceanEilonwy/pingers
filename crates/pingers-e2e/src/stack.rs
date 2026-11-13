// SPDX-License-Identifier: AGPL-3.0-only
//! The server, the relay and the tap between them, on the caller's tokio runtime.

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::body::{Body, Bytes, Frame, Incoming};
use hyper_util::rt::{TokioExecutor, TokioIo};
use pingers_crypto::{PushRelayPrivateKey, PushRelayPublicKey};
use pingers_delivery::{Delivery, SqliteDeliveryStore};
use pingers_mailbox::{ListedRelays, RelayCredential, RelayTarget};
use pingers_ohttp::{GatewayKey, KeyConfig};
use pingers_proto::{RelayId, RelayUrl, ServerId};
use pingers_server::{
    AddressList, Database, DeliveryLive, OhttpKeys, Outside, RelayAllowList, Required, RngSeed,
    ServerConfig,
};
use pingers_testkit::{random_array, seeded_rng, test_clock};
use pingers_time::{Clock, ManualClock, Timer as _, UnixSeconds};
use pingers_transport::{KeyPin, OhttpConfig, OhttpTransport, Relay, TlsRoots, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::E2eError;
use crate::tap::{self, Tap};

/// Where simulated devices connect from.
pub const DEVICE_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// Where the relay's connections to the server come from (through the [`Tap`]): the only
/// address the server's gateway accepts.
pub const RELAY_EGRESS: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
/// The push relay the server lists. Its URL does not resolve, so wakes go nowhere.
const PUSH_RELAY_ID: RelayId = RelayId::new(1);
const PUSH_RELAY_URL: &str = "https://push-relay.invalid";

/// The delivery service the devices' group logs go to (pingers-server's group routes are
/// wired, but the devices do not use them yet).
pub type GroupDelivery = Delivery<SqliteDeliveryStore, ()>;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// pingers-server behind the testkit's OHTTP relay, with a [`Tap`] in between.
pub struct Stack {
    clock: ManualClock,
    server: SocketAddr,
    relay: SocketAddr,
    key_config: KeyConfig,
    push_relay_key: PushRelayPublicKey,
    push_relay_private: PushRelayPrivateKey,
    directory: PathBuf,
    tap: Arc<Tap>,
    delivery: GroupDelivery,
    credentials: Arc<crate::TokenPool>,
    stop: watch::Sender<()>,
    tasks: Vec<JoinHandle<()>>,
}

impl Stack {
    /// Starts the server, the tap and the relay on ephemeral loopback ports of the current
    /// tokio runtime, with every key derived from `seed` and the testkit's clock.
    ///
    /// # Errors
    ///
    /// [`E2eError::Io`] if a port cannot be bound (the tap needs [`RELAY_EGRESS`], which Linux
    /// routes to loopback), or a key or configuration error.
    pub async fn start(seed: u64) -> Result<Self, E2eError> {
        let clock = test_clock();
        let directory = std::env::temp_dir().join(format!(
            "pingers-e2e-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory)?;

        let mut rng = seeded_rng(seed);
        let gateway_key = GatewayKey::generate(1, &mut rng)?;
        let key_config = gateway_key.config();
        let push_relay_private = PushRelayPrivateKey::generate(&mut rng);
        let push_relay_key = push_relay_private.public_key(1)?;
        let server_seed = RngSeed::from_bytes(Zeroizing::new(random_array(&mut rng)));

        let server_listener = TcpListener::bind(SocketAddr::new(DEVICE_ADDRESS, 0)).await?;
        let server = server_listener.local_addr()?;
        let server_id = ServerId::new(format!("https://{server}"))?;
        let tap_listener = TcpListener::bind(SocketAddr::new(DEVICE_ADDRESS, 0)).await?;
        let tap_address = tap_listener.local_addr()?;
        let relay_listener = TcpListener::bind(SocketAddr::new(DEVICE_ADDRESS, 0)).await?;
        let relay = relay_listener.local_addr()?;

        let push_relays = ListedRelays::new([(
            PUSH_RELAY_ID,
            RelayTarget {
                url: RelayUrl::new(PUSH_RELAY_URL.to_owned())?,
                credential: RelayCredential::new("pingers-e2e".to_owned()),
            },
        )]);
        let config = ServerConfig {
            metrics: AddressList::new(vec![DEVICE_ADDRESS]),
            mailbox_store: Database::File(directory.join("mailboxes.sqlite")),
            push_relays,
            push_timeout: Duration::from_secs(1),
            expire_interval: Duration::from_secs(60),
            ..ServerConfig::new(Required {
                listen: server,
                server_id: server_id.clone(),
                ohttp_keys: OhttpKeys::new(vec![gateway_key])?,
                relays: RelayAllowList::new(vec![RELAY_EGRESS])?,
            })
        };
        let outside = Outside {
            clock: clock.clone(),
            timer: clock.clone(),
            seed: server_seed,
            live: DeliveryLive,
        };
        let delivery = Delivery::new(
            SqliteDeliveryStore::open(&directory.join("delivery.sqlite"))?,
            Arc::new(clock.clone()),
        );

        let (stop, stopped) = watch::channel(());
        let until_stopped = |mut stopped: watch::Receiver<()>| async move {
            // A stop signal, or the stack being dropped.
            let _ = stopped.changed().await;
        };
        let tap = Arc::new(Tap::default());
        let server_done = until_stopped(stopped.clone());
        let tap_done = until_stopped(stopped.clone());
        let relay_done = until_stopped(stopped);
        let tasks = vec![
            tokio::spawn(async move {
                // A server that fails to start fails the scenario's first request.
                let _ =
                    pingers_server::serve_with(config, outside, server_listener, server_done).await;
            }),
            tokio::spawn(tap::serve(
                tap_listener,
                server,
                RELAY_EGRESS,
                Arc::clone(&tap),
                tap_done,
            )),
            tokio::spawn(pingers_testkit::harness::run_ohttp_relay(
                relay_listener,
                tap_address,
                relay_done,
            )),
        ];
        let credentials = Arc::new(crate::TokenPool::new(server, server_id, clock.clone()));
        Ok(Self {
            clock,
            server,
            relay,
            key_config,
            push_relay_key,
            push_relay_private,
            directory,
            tap,
            delivery,
            credentials,
            stop,
            tasks,
        })
    }

    /// The clock the server reads; devices take their fixes and buckets from it too.
    #[must_use]
    pub const fn clock(&self) -> &ManualClock {
        &self.clock
    }

    /// The current time.
    #[must_use]
    pub fn now(&self) -> UnixSeconds {
        Clock::now(&self.clock)
    }

    /// Moves wall and monotonic time forward, then gives the server's timers a moment to run
    /// (the expiry sweep).
    pub async fn advance(&self, duration: Duration) {
        self.clock.advance(duration);
        TokioTimer::new().sleep(Duration::from_millis(50)).await;
    }

    /// The server's own address (devices never use it for anonymous requests).
    #[must_use]
    pub const fn server_address(&self) -> SocketAddr {
        self.server
    }

    /// The relay devices send anonymous requests to.
    #[must_use]
    pub const fn relay_address(&self) -> SocketAddr {
        self.relay
    }

    /// The gateway key config devices pin.
    #[must_use]
    pub const fn key_config(&self) -> &KeyConfig {
        &self.key_config
    }

    /// The push relay key devices seal push tokens to, and its id on the server's list.
    #[must_use]
    pub const fn push_relay(&self) -> (RelayId, &PushRelayPublicKey) {
        (PUSH_RELAY_ID, &self.push_relay_key)
    }

    /// The push relay's private key: only the relay could open a sealed token with it.
    #[must_use]
    pub const fn push_relay_private_key(&self) -> &PushRelayPrivateKey {
        &self.push_relay_private
    }

    /// Bytes and peers between relay and server.
    #[must_use]
    pub fn tap(&self) -> &Tap {
        &self.tap
    }

    /// The Meetup group delivery service (standing in for the unwired group routes).
    #[must_use]
    pub const fn delivery(&self) -> &GroupDelivery {
        &self.delivery
    }

    /// The Privacy Pass tokens the devices spend on anonymous requests.
    #[must_use]
    pub fn credentials(&self) -> Arc<crate::TokenPool> {
        Arc::clone(&self.credentials)
    }

    /// The server's mailbox database file.
    #[must_use]
    pub fn mailbox_database(&self) -> PathBuf {
        self.directory.join("mailboxes.sqlite")
    }

    /// The delivery service's database file.
    #[must_use]
    pub fn delivery_database(&self) -> PathBuf {
        self.directory.join("delivery.sqlite")
    }

    /// The directory holding both database files.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// A device's OHTTP transport: through the relay, pinned to the gateway key.
    ///
    /// # Errors
    ///
    /// [`E2eError::Transport`] if the transport cannot be built.
    pub fn transport(&self, seed: u64) -> Result<OhttpTransport, E2eError> {
        let relay = Relay::new(
            &format!("http://{}/ohttp", self.relay),
            &format!("http://{}/v1/ohttp/config", self.relay),
        )?;
        let mut config = OhttpConfig::new(
            vec![relay],
            KeyPin::Pinned(self.key_config.hash()),
            TlsRoots::from_der(&[])?,
        );
        config.max_response_bytes = 4 * 1024 * 1024;
        Ok(OhttpTransport::new(
            config,
            Arc::new(TokioTimer::new()),
            Box::new(seeded_rng(seed)),
        )?)
    }

    /// A plain HTTP/2 request straight to the server from [`DEVICE_ADDRESS`], bypassing the
    /// relay: what a device that ignored the relay would do. Returns the status and body.
    ///
    /// # Errors
    ///
    /// [`E2eError::Io`] or [`E2eError::Http`] if the request cannot be made.
    #[expect(
        clippy::too_many_arguments,
        reason = "a raw HTTP request: method, path, media type, body"
    )]
    pub async fn direct(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), E2eError> {
        http2(
            self.server,
            Http2Request {
                method,
                path,
                content_type,
                authorization: None,
                body,
            },
        )
        .await
    }

    /// The server's metrics text (`GET /metrics`, which the server answers for
    /// [`DEVICE_ADDRESS`] only).
    ///
    /// # Errors
    ///
    /// As [`Stack::direct`], and [`E2eError::Status`] unless the server answers 200.
    pub async fn metrics(&self) -> Result<String, E2eError> {
        let (status, body) = self.direct("GET", "/metrics", None, Vec::new()).await?;
        if status != 200 {
            return Err(E2eError::Status {
                what: "metrics".to_owned(),
                status,
            });
        }
        String::from_utf8(body).map_err(|_| E2eError::step("metrics are not UTF-8"))
    }

    /// Stops the services, waits for them, and removes the database files.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(());
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        for task in &self.tasks {
            task.abort();
        }
        // Best effort: a leftover temporary directory fails nothing.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// A direct request to the server.
pub(crate) struct Http2Request<'a> {
    pub(crate) method: &'a str,
    pub(crate) path: &'a str,
    pub(crate) content_type: Option<&'a str>,
    pub(crate) authorization: Option<&'a str>,
    pub(crate) body: Vec<u8>,
}

/// Sends `request` over a fresh HTTP/2 connection to `server`; returns the status and body.
pub(crate) async fn http2(
    server: SocketAddr,
    request: Http2Request<'_>,
) -> Result<(u16, Vec<u8>), E2eError> {
    let Http2Request {
        method,
        path,
        content_type,
        authorization,
        body,
    } = request;
    let stream = tokio::net::TcpStream::connect(server).await?;
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream)).await?;
    let connection = tokio::spawn(connection);
    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(format!("http://{server}{path}"));
    if let Some(content_type) = content_type {
        builder = builder.header(hyper::header::CONTENT_TYPE, content_type);
    }
    if let Some(authorization) = authorization {
        builder = builder.header(hyper::header::AUTHORIZATION, authorization);
    }
    let response = sender
        .send_request(builder.body(OnceBody(Some(Bytes::from(body))))?)
        .await?;
    let status = response.status().as_u16();
    let body = collect(response.into_body()).await?;
    drop(sender);
    connection.abort();
    Ok((status, body))
}

/// A body sent in one frame.
struct OnceBody(Option<Bytes>);

impl Body for OnceBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Ready(self.get_mut().0.take().map(|bytes| Ok(Frame::data(bytes))))
    }
}

async fn collect(mut body: Incoming) -> Result<Vec<u8>, E2eError> {
    let mut bytes = Vec::new();
    while let Some(frame) =
        std::future::poll_fn(|context| Pin::new(&mut body).poll_frame(context)).await
    {
        if let Ok(data) = frame?.into_data() {
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes)
}
