// SPDX-License-Identifier: AGPL-3.0-only
//! The Meetup live channel: one chunked OHTTP exchange through a relay (design B.4.9).
//!
//! A driver task, owned by the [`LiveChannel`] handle and aborted when it is dropped, holds
//! the exchange. It sends a keep-alive chunk after 20 s without other traffic, treats a
//! silent downlink as a dropped exchange, and reconnects with a fresh HPKE context after the
//! backoff, timed by the injected [`Timer`](pingers_time::Timer). If a relay holds request
//! bodies back (no response head within the probe time), the driver switches to the split
//! channel on the same relay: a chunked exchange whose request ends after its head carries
//! the downlink, and each outgoing frame goes up as its own one-shot OHTTP request.
//!
//! Wire conventions the gateway shares (pingers-server):
//! - every chunk after the request head is one frame; [`KEEP_ALIVE_CHUNK`] is a keep-alive,
//!   discarded by the receiver, and the gateway sends one at the same cadence;
//! - a chunked request whose request stream ends right after its head is a split-channel
//!   downlink; a one-shot request with the live path and headers carries one uplink frame in
//!   its body;
//! - the gateway answers the head of a chunked request before reading further chunks, which
//!   is how a full-duplex relay is told apart from one that buffers.

use std::collections::VecDeque;
use std::sync::Arc;

use hyper::StatusCode;
use hyper::body::{Bytes, Incoming};
use pingers_ohttp::{
    BinaryRequest, ChunkPosition, ChunkedClient, KeyConfig, OhttpError, ResponseChunkDecoder,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::http::{OutBody, SendError, next_data};
use crate::ohttp::{OHTTP_CHUNKED_REQUEST, Shared, is_relay_fault, post};
use crate::relay::Duplex;
use crate::timer::within;
use crate::{DuplexStream, TransportError};

/// The payload of a transport keep-alive chunk. Real frames are padded to at least 256
/// bytes (design B.6.1), so a one-byte chunk is never a frame.
pub const KEEP_ALIVE_CHUNK: &[u8] = &[0];

/// The largest frame one chunk carries (draft-08 chunks are capped at 16 KiB of plaintext in
/// pingers-ohttp).
pub const MAX_FRAME_BYTES: usize = 16 * 1024;

/// Frames queued between the handle and the driver, each way.
const QUEUE: usize = 32;

/// Frames kept while reconnecting; older ones are dropped first (a newer location frame
/// supersedes an older one, and stored messages are in the group log anyway).
const PENDING_WHILE_RECONNECTING: usize = 16;

/// Whether a reconnect cannot help: the key no longer matches, or the request is wrong.
fn is_fatal(error: &TransportError) -> bool {
    matches!(
        error,
        TransportError::Ohttp(_) | TransportError::InvalidRequest | TransportError::ClassNotAllowed
    )
}

/// Opens the live channel described by `head` (its body must be empty: the body is the
/// frame stream). The first exchange is made before returning, so a missing relay or a key
/// mismatch is reported here.
pub(crate) async fn open(
    shared: Arc<Shared>,
    head: BinaryRequest,
) -> Result<Box<dyn DuplexStream>, TransportError> {
    if !head.body.is_empty() {
        return Err(TransportError::InvalidRequest);
    }
    shared.check_key()?;
    let session = connect(&shared, &head).await?;
    let (outgoing, outgoing_receiver) = mpsc::channel(QUEUE);
    let (incoming_sender, incoming) = mpsc::channel(QUEUE);
    let driver = Driver {
        shared: Arc::clone(&shared),
        head,
        outgoing: outgoing_receiver,
        incoming: incoming_sender,
        pending: VecDeque::new(),
    };
    let task = tokio::spawn(driver.run(session));
    Ok(Box::new(LiveChannel {
        shared,
        outgoing: Some(outgoing),
        incoming,
        failed: false,
        driver: Some(task),
    }))
}

/// One exchange: through which relay, in which mode, and the response stream.
struct Session {
    relay: usize,
    mode: Mode,
    response: Incoming,
    decoder: ResponseChunkDecoder,
}

enum Mode {
    /// Both directions in one exchange.
    Duplex {
        client: ChunkedClient,
        upstream: mpsc::Sender<Bytes>,
    },
    /// Downlink only; uplink frames go as one-shot requests.
    Split,
}

/// Opens an exchange on the first relay that can be reached.
async fn connect(shared: &Shared, head: &BinaryRequest) -> Result<Session, TransportError> {
    for index in shared.pool.available(shared.timer.now()) {
        match open_on(shared, head, index).await {
            Ok(session) => {
                shared.pool.record_success(index);
                return Ok(session);
            }
            Err(SendError::Unreachable(_)) => shared.pool.record_failure(index, shared.timer.now()),
            Err(SendError::Failed(error)) => {
                if !is_relay_fault(&error) {
                    return Err(error);
                }
                shared.pool.record_failure(index, shared.timer.now());
            }
        }
    }
    Err(TransportError::NoRelay)
}

async fn open_on(
    shared: &Shared,
    head: &BinaryRequest,
    index: usize,
) -> Result<Session, SendError> {
    let config = shared.key_config(index).await?;
    match shared.pool.duplex(index) {
        Duplex::ResponseOnly => open_split(shared, head, index, &config).await,
        Duplex::Full => open_duplex(shared, head, index, &config).await,
        Duplex::Unknown => match open_duplex(shared, head, index, &config).await {
            Ok(session) => {
                shared.pool.set_duplex(index, Duplex::Full);
                Ok(session)
            }
            Err(SendError::Failed(TransportError::Timeout)) => {
                shared.pool.set_duplex(index, Duplex::ResponseOnly);
                open_split(shared, head, index, &config).await
            }
            Err(other) => Err(other),
        },
    }
}

fn start(
    shared: &Shared,
    head: &BinaryRequest,
    config: &KeyConfig,
) -> Result<pingers_ohttp::ChunkedClientStart, SendError> {
    // A fresh HPKE context for every exchange, reconnects included (design B.4.9).
    shared
        .with_rng(|rng| ChunkedClient::start(config, head, rng))
        .map_err(|error| SendError::Failed(error.into()))
}

/// Sends the request and waits for the response head until `deadline`.
async fn begin(
    shared: &Shared,
    index: usize,
    body: OutBody,
    deadline: pingers_time::MonotonicTime,
) -> Result<Incoming, SendError> {
    let endpoint = shared.relay(index)?.request_endpoint();
    let request = post(endpoint, OHTTP_CHUNKED_REQUEST, body).map_err(SendError::Failed)?;
    let response = shared
        .http
        .send(&endpoint.origin(), request, deadline)
        .await?;
    if response.status() != StatusCode::OK {
        return Err(SendError::Failed(TransportError::Status {
            status: response.status().as_u16(),
        }));
    }
    Ok(response.into_body())
}

async fn open_duplex(
    shared: &Shared,
    head: &BinaryRequest,
    index: usize,
    config: &KeyConfig,
) -> Result<Session, SendError> {
    let start = start(shared, head, config)?;
    let (upstream, receiver) = mpsc::channel(QUEUE);
    upstream
        .try_send(Bytes::from(start.preamble))
        .map_err(|_| SendError::Failed(TransportError::StreamClosed))?;
    let deadline = shared.deadline(shared.live.duplex_probe);
    let response = begin(shared, index, OutBody::Stream(receiver), deadline).await?;
    let decoder = start.client.response_decoder();
    Ok(Session {
        relay: index,
        mode: Mode::Duplex {
            client: start.client,
            upstream,
        },
        response,
        decoder,
    })
}

async fn open_split(
    shared: &Shared,
    head: &BinaryRequest,
    index: usize,
    config: &KeyConfig,
) -> Result<Session, SendError> {
    let mut start = start(shared, head, config)?;
    let mut body = start.preamble;
    body.extend(
        start
            .client
            .seal_chunk(&[], ChunkPosition::Final)
            .map_err(|error| SendError::Failed(error.into()))?,
    );
    let deadline = shared.deadline(shared.timeouts.request);
    let response = begin(shared, index, OutBody::full(body), deadline).await?;
    Ok(Session {
        relay: index,
        mode: Mode::Split,
        response,
        decoder: start.client.response_decoder(),
    })
}

/// The response half of an exchange.
struct Downlink {
    response: Incoming,
    decoder: ResponseChunkDecoder,
}

/// How an exchange ended.
enum Outcome {
    /// The handle closed the channel.
    Closed,
    /// The gateway sent its final chunk.
    GatewayEnded,
    /// The exchange broke: reconnect.
    Dropped,
    /// It cannot continue (a chunk failed to open).
    Fatal(TransportError),
}

struct Driver {
    shared: Arc<Shared>,
    head: BinaryRequest,
    outgoing: mpsc::Receiver<Vec<u8>>,
    incoming: mpsc::Sender<Result<Vec<u8>, TransportError>>,
    /// Frames taken from the handle while there was no exchange to send them on.
    pending: VecDeque<Vec<u8>>,
}

impl Driver {
    async fn run(mut self, mut session: Session) {
        loop {
            match self.serve(session).await {
                Outcome::Closed | Outcome::GatewayEnded => return,
                Outcome::Fatal(error) => {
                    let _ = self.incoming.send(Err(error)).await;
                    return;
                }
                Outcome::Dropped => {}
            }
            match self.reconnect().await {
                Ok(next) => session = next,
                Err(None) => return,
                Err(Some(error)) => {
                    let _ = self.incoming.send(Err(error)).await;
                    return;
                }
            }
        }
    }

    /// Reconnects with backoff. `Err(None)` if the handle closed meanwhile.
    async fn reconnect(&mut self) -> Result<Session, Option<TransportError>> {
        let mut failures: u32 = 0;
        loop {
            let error = match connect(&self.shared, &self.head).await {
                Ok(session) => return Ok(session),
                Err(error) if is_fatal(&error) => return Err(Some(error)),
                Err(error) => error,
            };
            failures = failures.saturating_add(1);
            if failures > self.shared.live.max_reconnects {
                return Err(Some(error));
            }
            let wake = self.shared.deadline(self.shared.backoff.delay(failures));
            loop {
                tokio::select! {
                    () = self.shared.timer.sleep_until(wake) => break,
                    frame = self.outgoing.recv() => match frame {
                        Some(frame) => self.keep_pending(frame),
                        None => return Err(None),
                    },
                }
            }
        }
    }

    fn keep_pending(&mut self, frame: Vec<u8>) {
        if self.pending.len() >= PENDING_WHILE_RECONNECTING {
            self.pending.pop_front();
        }
        self.pending.push_back(frame);
    }

    /// Hands one opened payload to the handle, unless it is a keep-alive. `false` if the
    /// handle is gone.
    async fn deliver(&self, payload: Vec<u8>) -> bool {
        if payload == KEEP_ALIVE_CHUNK || payload.is_empty() {
            return true;
        }
        self.incoming.send(Ok(payload)).await.is_ok()
    }

    /// Sends one frame upstream on the current exchange.
    async fn send_frame(&self, relay: usize, mode: &mut Mode, frame: &[u8]) -> Result<(), Outcome> {
        match mode {
            Mode::Duplex { client, upstream } => {
                let sealed = client
                    .seal_chunk(frame, ChunkPosition::NonFinal)
                    .map_err(|error| Outcome::Fatal(error.into()))?;
                upstream
                    .send(Bytes::from(sealed))
                    .await
                    .map_err(|_| Outcome::Dropped)
            }
            Mode::Split => {
                let request = BinaryRequest {
                    method: self.head.method.clone(),
                    path: self.head.path.clone(),
                    headers: self.head.headers.clone(),
                    body: frame.to_vec(),
                };
                match self.shared.one_shot(relay, &request).await {
                    Ok(_) => Ok(()),
                    Err(error) => {
                        let error = error.into_error();
                        Err(if is_fatal(&error) {
                            Outcome::Fatal(error)
                        } else {
                            Outcome::Dropped
                        })
                    }
                }
            }
        }
    }

    /// Runs one exchange until it ends.
    async fn serve(&mut self, session: Session) -> Outcome {
        let Session {
            relay,
            mut mode,
            mut response,
            mut decoder,
        } = session;
        while let Some(frame) = self.pending.pop_front() {
            if let Err(ended) = self.send_frame(relay, &mut mode, &frame).await {
                return ended;
            }
        }
        let live = self.shared.live;
        let shared = Arc::clone(&self.shared);
        let timer = &*shared.timer;
        let mut keep_alive_at = timer.now().saturating_add(live.keep_alive);
        let mut idle_at = timer.now().saturating_add(live.idle);
        let duplex = matches!(mode, Mode::Duplex { .. });
        loop {
            tokio::select! {
                frame = self.outgoing.recv() => {
                    let Some(frame) = frame else {
                        if let Mode::Duplex { client, upstream } = mode {
                            let downlink = Downlink { response, decoder };
                            self.close_duplex(client, upstream, downlink).await;
                        }
                        return Outcome::Closed;
                    };
                    if let Err(ended) = self.send_frame(relay, &mut mode, &frame).await {
                        return ended;
                    }
                    keep_alive_at = timer.now().saturating_add(live.keep_alive);
                }
                () = timer.sleep_until(keep_alive_at), if duplex => {
                    if let Err(ended) = self.send_frame(relay, &mut mode, KEEP_ALIVE_CHUNK).await {
                        return ended;
                    }
                    keep_alive_at = timer.now().saturating_add(live.keep_alive);
                }
                () = timer.sleep_until(idle_at) => return Outcome::Dropped,
                data = next_data(&mut response) => match data {
                    Ok(Some(bytes)) => {
                        idle_at = timer.now().saturating_add(live.idle);
                        let chunks = match decoder.feed(&bytes) {
                            Ok(chunks) => chunks,
                            Err(error) => return Outcome::Fatal(error.into()),
                        };
                        for chunk in chunks {
                            if !self.deliver(chunk.payload).await {
                                return Outcome::Closed;
                            }
                        }
                    }
                    Ok(None) => return self.finish(decoder).await,
                    Err(_) => return Outcome::Dropped,
                },
            }
        }
    }

    /// Sends the final chunk, then reads the response to its end, so the request's end is not
    /// cut off by a stream reset. Frames still arriving are handed over if there is room.
    /// Bounded by [`DuplexStream::close`]'s deadline, which aborts the driver.
    async fn close_duplex(
        &self,
        mut client: ChunkedClient,
        upstream: mpsc::Sender<Bytes>,
        downlink: Downlink,
    ) {
        let Downlink {
            mut response,
            mut decoder,
        } = downlink;
        if let Ok(sealed) = client.seal_chunk(&[], ChunkPosition::Final) {
            let _ = upstream.send(Bytes::from(sealed)).await;
        }
        drop(upstream);
        while let Ok(Some(bytes)) = next_data(&mut response).await {
            let Ok(chunks) = decoder.feed(&bytes) else {
                return;
            };
            for chunk in chunks {
                if chunk.payload != KEEP_ALIVE_CHUNK {
                    let _ = self.incoming.try_send(Ok(chunk.payload));
                }
            }
        }
    }

    /// The response stream ended: with a final chunk the gateway closed the channel;
    /// without one the relay or the network dropped it.
    async fn finish(&self, decoder: ResponseChunkDecoder) -> Outcome {
        match decoder.finish() {
            Ok(Some(last)) => {
                self.deliver(last.payload).await;
                Outcome::GatewayEnded
            }
            Ok(None) => Outcome::GatewayEnded,
            Err(OhttpError::Truncated) => Outcome::Dropped,
            Err(error) => Outcome::Fatal(error.into()),
        }
    }
}

/// The handle to a live channel. Dropping it aborts the driver; [`DuplexStream::close`]
/// ends the exchange cleanly first.
struct LiveChannel {
    shared: Arc<Shared>,
    outgoing: Option<mpsc::Sender<Vec<u8>>>,
    incoming: mpsc::Receiver<Result<Vec<u8>, TransportError>>,
    /// An error was returned; the channel is over.
    failed: bool,
    driver: Option<JoinHandle<()>>,
}

impl Drop for LiveChannel {
    fn drop(&mut self) {
        if let Some(driver) = self.driver.take() {
            driver.abort();
        }
    }
}

/// Checks a frame before it is queued.
fn check_frame(frame: &[u8]) -> Result<(), TransportError> {
    if frame.is_empty() || frame.len() > MAX_FRAME_BYTES || frame == KEEP_ALIVE_CHUNK {
        Err(TransportError::InvalidRequest)
    } else {
        Ok(())
    }
}

#[async_trait::async_trait]
impl DuplexStream for LiveChannel {
    async fn send(&mut self, frame: Vec<u8>) -> Result<(), TransportError> {
        check_frame(&frame)?;
        let outgoing = self.outgoing.as_ref().ok_or(TransportError::StreamClosed)?;
        outgoing
            .send(frame)
            .await
            .map_err(|_| TransportError::StreamClosed)
    }

    async fn receive(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if self.failed {
            return Err(TransportError::StreamClosed);
        }
        match self.incoming.recv().await {
            Some(Ok(frame)) => Ok(Some(frame)),
            Some(Err(error)) => {
                self.failed = true;
                Err(error)
            }
            None => Ok(None),
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.outgoing = None;
        let Some(driver) = self.driver.take() else {
            return Ok(());
        };
        let abort = driver.abort_handle();
        let deadline = self.shared.deadline(self.shared.timeouts.request);
        let finished = within(&*self.shared.timer, deadline, driver).await;
        if finished.is_err() {
            abort.abort();
        }
        finished.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_must_be_non_empty_bounded_and_not_a_keep_alive() {
        assert!(check_frame(&[]).is_err());
        assert!(check_frame(KEEP_ALIVE_CHUNK).is_err());
        assert!(check_frame(&vec![1; MAX_FRAME_BYTES + 1]).is_err());
        assert!(check_frame(&vec![1; MAX_FRAME_BYTES]).is_ok());
        assert!(check_frame(&[0, 0]).is_ok());
        assert!(check_frame(&[1]).is_ok());
    }

    #[test]
    fn only_key_and_request_problems_stop_reconnecting() {
        assert!(is_fatal(&OhttpError::KeyMismatch.into()));
        assert!(is_fatal(&TransportError::InvalidRequest));
        assert!(is_fatal(&TransportError::ClassNotAllowed));
        assert!(!is_fatal(&TransportError::NoRelay));
        assert!(!is_fatal(&TransportError::Timeout));
        assert!(!is_fatal(&TransportError::Status { status: 502 }));
        assert!(!is_fatal(&TransportError::Connection));
    }
}
