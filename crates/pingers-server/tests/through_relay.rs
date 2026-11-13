// SPDX-License-Identifier: AGPL-3.0-only
//! The server behind a real relay: a chunked live channel echoed frame by frame through the
//! relay, and the in-process harness starting and stopping around it.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes, HttpBody};
use hyper::body::Frame;
use hyper::header::CONTENT_TYPE;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use pingers_ohttp::{BinaryRequest, ChunkPosition, ChunkedClient, GatewayKey, OpenedChunk};
use pingers_proto::ServerId;
use pingers_server::{
    LiveChannel, LiveFrame, LiveRefused, OhttpKeys, Outside, RelayAllowList, Required, RngSeed,
    ServerConfig, serve_with,
};
use pingers_testkit::{HarnessConfig, InProcessHarness, seeded_rng, test_clock};
use tokio::sync::{mpsc, watch};
use zeroize::Zeroizing;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const STEP: Duration = Duration::from_secs(10);
const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Echoes every frame back.
struct Echo;

impl LiveChannel for Echo {
    fn open(
        &self,
        _request: &BinaryRequest,
        inbound: mpsc::Receiver<LiveFrame>,
    ) -> Result<mpsc::Receiver<LiveFrame>, LiveRefused> {
        Ok(inbound)
    }

    fn send(&self, _request: &BinaryRequest, _frame: LiveFrame) -> Result<(), LiveRefused> {
        Err(LiveRefused::NoChannel)
    }
}

struct ChannelBody(mpsc::UnboundedReceiver<Bytes>);

impl HttpBody for ChannelBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.get_mut()
            .0
            .poll_recv(context)
            .map(|message| message.map(|bytes| Ok(Frame::data(bytes))))
    }
}

async fn next_data(body: &mut Body) -> Result<Option<Bytes>, TestError> {
    loop {
        let frame = tokio::time::timeout(
            STEP,
            std::future::poll_fn(|context| Pin::new(&mut *body).poll_frame(context)),
        )
        .await?;
        match frame {
            None => return Ok(None),
            Some(frame) => {
                if let Ok(data) = frame?.into_data() {
                    return Ok(Some(data));
                }
            }
        }
    }
}

/// The server, with the echo channel, behind the harness's relay.
struct Stack {
    relay: SocketAddr,
    key_config: pingers_ohttp::KeyConfig,
    stop: watch::Sender<()>,
    server_task: tokio::task::JoinHandle<Result<(), pingers_server::ServerError>>,
    relay_task: tokio::task::JoinHandle<()>,
}

async fn start_behind_relay() -> Result<Stack, TestError> {
    let key = GatewayKey::generate(1, &mut seeded_rng(1))?;
    let key_config = key.config();
    let server_listener = tokio::net::TcpListener::bind(SocketAddr::new(LOOPBACK, 0)).await?;
    let server = server_listener.local_addr()?;
    let relay_listener = tokio::net::TcpListener::bind(SocketAddr::new(LOOPBACK, 0)).await?;
    let relay = relay_listener.local_addr()?;
    let config = ServerConfig::new(Required {
        listen: server,
        server_id: ServerId::new("https://pingers.test".to_owned())?,
        ohttp_keys: OhttpKeys::new(vec![key])?,
        relays: RelayAllowList::new(vec![LOOPBACK])?,
    });
    let clock = test_clock();
    let outside = Outside {
        clock: clock.clone(),
        timer: clock,
        seed: RngSeed::from_bytes(Zeroizing::new([3; 32])),
        live: Echo,
    };
    let (stop, stopped) = watch::channel(());
    let mut server_stopped = stopped.clone();
    let mut relay_stopped = stopped;
    let server_task = tokio::spawn(serve_with(config, outside, server_listener, async move {
        let _ = server_stopped.changed().await;
    }));
    let relay_task = tokio::spawn(pingers_testkit::harness::run_ohttp_relay(
        relay_listener,
        server,
        async move {
            let _ = relay_stopped.changed().await;
        },
    ));
    Ok(Stack {
        relay,
        key_config,
        stop,
        server_task,
        relay_task,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_channel_echoes_through_the_relay_frame_by_frame() -> TestResult {
    let Stack {
        relay,
        key_config,
        stop,
        server_task,
        relay_task,
    } = start_behind_relay().await?;
    let stream = tokio::net::TcpStream::connect(relay).await?;
    let (mut client, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream)).await?;
    let connection = tokio::spawn(connection);
    let head = BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/groups/00ff/live?sub=01".to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let start = ChunkedClient::start(&key_config, &head, &mut seeded_rng(2))?;
    let mut chunked = start.client;
    let (sender, receiver) = mpsc::unbounded_channel();
    sender.send(Bytes::from(start.preamble))?;
    let request = Request::builder()
        .method(Method::POST)
        .uri("http://relay/ohttp")
        .header(CONTENT_TYPE, "message/ohttp-chunked-req")
        .body(Body::new(ChannelBody(receiver)))?;
    // The response starts while the member's stream is still open.
    let response = tokio::time::timeout(STEP, client.send_request(request)).await??;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .map(hyper::header::HeaderValue::as_bytes),
        Some(b"message/ohttp-chunked-res".as_slice())
    );
    let mut downlink = Body::new(response.into_body());
    let mut decoder = chunked.response_decoder();

    // One frame at a time, each waiting for its echo: only full-duplex streaming through the
    // relay and the gateway completes this.
    for round in 0_u8..10 {
        let frame = vec![round; 300];
        sender.send(Bytes::from(
            chunked.seal_chunk(&frame, ChunkPosition::NonFinal)?,
        ))?;
        let mut echoed = Vec::new();
        while echoed.is_empty() {
            let bytes = next_data(&mut downlink).await?.ok_or("ended early")?;
            echoed = decoder.feed(&bytes)?;
        }
        assert_eq!(
            echoed,
            [OpenedChunk {
                payload: frame,
                position: ChunkPosition::NonFinal,
            }],
            "round {round}"
        );
    }
    sender.send(Bytes::from(chunked.seal_chunk(&[], ChunkPosition::Final)?))?;
    drop(sender);
    while let Some(bytes) = next_data(&mut downlink).await? {
        assert!(decoder.feed(&bytes)?.is_empty());
    }
    assert_eq!(
        decoder.finish()?,
        Some(OpenedChunk {
            payload: Vec::new(),
            position: ChunkPosition::Final,
        })
    );

    drop(client);
    stop.send(())?;
    tokio::time::timeout(STEP, server_task).await???;
    tokio::time::timeout(STEP, relay_task).await??;
    connection.abort();
    Ok(())
}

#[test]
fn the_harness_starts_and_stops_cleanly() -> TestResult {
    let harness = InProcessHarness::start(&HarnessConfig::default())?;
    let endpoints = harness.endpoints().clone();
    assert_ne!(endpoints.server, endpoints.ohttp_relay);
    assert!(std::net::TcpStream::connect(endpoints.server).is_ok());
    harness.shutdown();
    for address in [
        endpoints.server,
        endpoints.ohttp_relay,
        endpoints.push_relay,
    ] {
        assert!(
            std::net::TcpStream::connect(address).is_err(),
            "{address} still accepts"
        );
    }
    // A second harness starts cleanly after the first is gone.
    InProcessHarness::start(&HarnessConfig::default())?.shutdown();
    Ok(())
}
