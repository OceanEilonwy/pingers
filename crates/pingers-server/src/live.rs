// SPDX-License-Identifier: AGPL-3.0-only
//! The Meetup live channel (design B.4.9, D `POST /v1/groups/{id}/live`): the server's half of
//! the chunked OHTTP plumbing, and the seam where pingers-delivery's fan-out registry plugs in.
//!
//! The gateway opens each request chunk into one [`LiveFrame`] and hands the stream to a
//! [`LiveChannel`]; whatever frames the channel returns are sealed as response chunks. When the
//! returned stream ends, the gateway sends the final chunk.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes, HttpBody};
use hyper::body::Frame;
use pingers_ohttp::{
    BinaryRequest, ChunkPosition, ChunkedGateway, OhttpError, RequestChunkDecoder,
};
use pingers_time::Timer;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Frames buffered between the gateway and the channel in each direction.
pub const LIVE_BUFFER: usize = 16;

/// One opaque, padded live frame (`Frame { epoch, body, ephemeral }` encrypted by the members;
/// the server never looks inside).
#[derive(Clone, PartialEq, Eq)]
pub struct LiveFrame {
    bytes: Vec<u8>,
}

impl LiveFrame {
    /// Wraps a frame's bytes (wire boundary).
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// The frame's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl core::fmt::Debug for LiveFrame {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LiveFrame")
            .field("length", &self.bytes.len())
            .finish()
    }
}

/// Why a live channel was not opened. The client sees the channel end at once either way: a
/// chunked response has no status of its own to say why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveRefused {
    /// pingers-delivery's fan-out registry is not wired in yet.
    NotWired,
    /// An uplink frame names a channel with no open downlink.
    NoChannel,
}

/// Joins a member's live exchange to its group.
pub trait LiveChannel: Send + Sync + 'static {
    /// Opens a channel for `request` (the inner request head: path, query and headers carry
    /// the group, subscription and `Grp` credentials). `inbound` yields the member's frames and
    /// ends when the member's stream does; the returned receiver yields the frames to send back
    /// and ends the exchange when it closes.
    ///
    /// # Errors
    ///
    /// [`LiveRefused`] if the channel cannot be opened.
    fn open(
        &self,
        request: &BinaryRequest,
        inbound: mpsc::Receiver<LiveFrame>,
    ) -> Result<mpsc::Receiver<LiveFrame>, LiveRefused>;

    /// Delivers one uplink frame of a split channel (pingers-transport's contract): a one-shot
    /// OHTTP request with the live head's method, path and headers, for a member whose relay
    /// buffers request bodies and so holds only a downlink exchange.
    ///
    /// # Errors
    ///
    /// [`LiveRefused`] if the frame cannot be delivered.
    fn send(&self, request: &BinaryRequest, frame: LiveFrame) -> Result<(), LiveRefused>;
}

/// The production live channel: pingers-delivery's fan-out registry once it has one; until
/// then every channel is refused.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeliveryLive;

impl LiveChannel for DeliveryLive {
    fn open(
        &self,
        _request: &BinaryRequest,
        _inbound: mpsc::Receiver<LiveFrame>,
    ) -> Result<mpsc::Receiver<LiveFrame>, LiveRefused> {
        // Waiting on pingers-delivery: its fan-out registry takes the member's inbound frames
        // and returns the merged stream of the other members' frames.
        Err(LiveRefused::NotWired)
    }

    fn send(&self, _request: &BinaryRequest, _frame: LiveFrame) -> Result<(), LiveRefused> {
        // Waiting on pingers-delivery, as `open`.
        Err(LiveRefused::NotWired)
    }
}

/// pingers-transport's keep-alive chunk: sent after [`KEEP_ALIVE_INTERVAL`] without other
/// chunks, and discarded on receipt. The client drops an exchange after 65 s of silence.
pub const KEEP_ALIVE_CHUNK: [u8; 1] = [0x00];
/// How long the gateway's half of a live exchange may be silent before a keep-alive.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(20);

type Idle = Pin<Box<dyn Future<Output = ()> + Send>>;

fn idle(timer: &Arc<dyn Timer>) -> Idle {
    let timer = Arc::clone(timer);
    Box::pin(async move { timer.sleep(KEEP_ALIVE_INTERVAL).await })
}

/// Aborts the task when dropped, so a pump never outlives its exchange.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The response body of a live exchange: the response nonce, one sealed chunk per frame the
/// channel yields, then the final chunk.
pub(crate) struct LiveBody {
    gateway: ChunkedGateway,
    preamble: Option<Vec<u8>>,
    /// `None` when the channel was refused: only the final chunk follows.
    outbound: Option<mpsc::Receiver<LiveFrame>>,
    /// Sends keep-alives while the channel is quiet (open exchanges only).
    timer: Option<Arc<dyn Timer>>,
    idle: Option<Idle>,
    finished: bool,
    /// Reads the request stream; aborted when this body is dropped.
    _pump: Option<AbortOnDrop>,
}

impl LiveBody {
    /// An exchange that ends at once, after the response nonce.
    pub(crate) fn closed(gateway: ChunkedGateway) -> Self {
        let preamble = Some(gateway.response_preamble());
        Self {
            gateway,
            preamble,
            outbound: None,
            timer: None,
            idle: None,
            finished: false,
            _pump: None,
        }
    }

    /// An exchange carrying `outbound`, while a task feeds the member's frames from
    /// `request_body` to the channel's `inbound` sender.
    pub(crate) fn open(gateway: ChunkedGateway, request_body: Body, channel: OpenChannel) -> Self {
        let OpenChannel {
            inbound,
            outbound,
            timer,
        } = channel;
        let decoder = gateway.request_decoder();
        let preamble = Some(gateway.response_preamble());
        let pump = tokio::spawn(pump(request_body, decoder, inbound));
        Self {
            gateway,
            preamble,
            outbound: Some(outbound),
            idle: Some(idle(&timer)),
            timer: Some(timer),
            finished: false,
            _pump: Some(AbortOnDrop(pump)),
        }
    }

    fn seal(
        &mut self,
        payload: &[u8],
        position: ChunkPosition,
    ) -> Result<Frame<Bytes>, OhttpError> {
        let sealed = self.gateway.seal_chunk(payload, position);
        if sealed.is_err() || position == ChunkPosition::Final {
            self.finished = true;
        }
        // Any chunk restarts the silence count.
        if let Some(timer) = &self.timer {
            self.idle = Some(idle(timer));
        }
        sealed.map(|bytes| Frame::data(Bytes::from(bytes)))
    }
}

impl HttpBody for LiveBody {
    type Data = Bytes;
    type Error = OhttpError;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, OhttpError>>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        if let Some(preamble) = this.preamble.take() {
            return Poll::Ready(Some(Ok(Frame::data(Bytes::from(preamble)))));
        }
        loop {
            let Some(outbound) = this.outbound.as_mut() else {
                return Poll::Ready(Some(this.seal(&[], ChunkPosition::Final)));
            };
            match outbound.poll_recv(context) {
                // A non-final chunk may not be empty (draft-08 §6), and an empty frame says
                // nothing: skip it.
                Poll::Ready(Some(frame)) if frame.as_bytes().is_empty() => {}
                Poll::Ready(Some(frame)) => {
                    return Poll::Ready(Some(this.seal(frame.as_bytes(), ChunkPosition::NonFinal)));
                }
                Poll::Ready(None) => {
                    return Poll::Ready(Some(this.seal(&[], ChunkPosition::Final)));
                }
                Poll::Pending => {
                    let quiet_too_long = this
                        .idle
                        .as_mut()
                        .is_some_and(|idle| idle.as_mut().poll(context).is_ready());
                    if quiet_too_long {
                        return Poll::Ready(Some(
                            this.seal(&KEEP_ALIVE_CHUNK, ChunkPosition::NonFinal),
                        ));
                    }
                    return Poll::Pending;
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.finished
    }
}

/// The next DATA bytes of `body`, `Ok(None)` at its end.
pub(crate) async fn next_data(body: &mut Body) -> Result<Option<Bytes>, axum::Error> {
    loop {
        let frame = std::future::poll_fn(|context| Pin::new(&mut *body).poll_frame(context)).await;
        match frame {
            None => return Ok(None),
            Some(frame) => {
                // Trailers carry nothing a live channel uses.
                if let Ok(data) = frame?.into_data() {
                    return Ok(Some(data));
                }
            }
        }
    }
}

/// Sends each non-empty chunk's payload on; `false` once the channel has gone.
async fn forward(
    chunks: Vec<pingers_ohttp::OpenedChunk>,
    inbound: &mpsc::Sender<LiveFrame>,
) -> bool {
    for chunk in chunks {
        // Keep-alives only keep the exchange open; empty chunks carry nothing.
        if chunk.payload.is_empty() || chunk.payload == KEEP_ALIVE_CHUNK {
            continue;
        }
        if inbound.send(LiveFrame::new(chunk.payload)).await.is_err() {
            return false;
        }
    }
    true
}

/// Opens the member's request stream chunk by chunk and passes the frames on. Any error (a
/// broken connection, a chunk that does not open, a missing final chunk) ends the inbound
/// stream, which ends the exchange. Live bodies last as long as a Meetup, so there is no read
/// timeout here: noticing a silent member is the delivery service's job.
///
/// A request stream that ends right after its head (nothing but an empty final chunk) is a
/// split channel's downlink: the member's frames come as one-shot requests instead, so the
/// inbound stream is kept open until the exchange is dropped.
async fn pump(mut body: Body, mut decoder: RequestChunkDecoder, inbound: mpsc::Sender<LiveFrame>) {
    // Chunks that arrived with the head come out of the first feed.
    let Ok(first) = decoder.feed(&[]) else {
        return;
    };
    let mut chunks_seen = !first.is_empty();
    if !forward(first, &inbound).await {
        return;
    }
    loop {
        let Ok(next) = next_data(&mut body).await else {
            return;
        };
        let Some(bytes) = next else {
            break;
        };
        let Ok(chunks) = decoder.feed(&bytes) else {
            return;
        };
        chunks_seen |= !chunks.is_empty();
        if !forward(chunks, &inbound).await {
            return;
        }
    }
    let Ok(Some(last)) = decoder.finish() else {
        return;
    };
    if !chunks_seen && last.payload.is_empty() {
        // Downlink only: hold the inbound stream open; this task is aborted with the exchange.
        std::future::pending::<()>().await;
    }
    forward(vec![last], &inbound).await;
}

/// What an open exchange is made of.
pub(crate) struct OpenChannel {
    /// Where the member's frames go.
    pub(crate) inbound: mpsc::Sender<LiveFrame>,
    /// The frames to send back.
    pub(crate) outbound: mpsc::Receiver<LiveFrame>,
    /// For keep-alives.
    pub(crate) timer: Arc<dyn Timer>,
}
