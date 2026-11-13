// SPDX-License-Identifier: AGPL-3.0-only
//! HTTP/2 connections: one per origin, reused, re-made when closed.
//!
//! Every task (the connection driver and hyper's per-stream tasks) is spawned into a
//! [`JoinSet`] owned by the connection, so dropping the client or a dead connection cancels
//! them all; nothing is detached (style guide §10).

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::{Future, poll_fn};
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::client::conn::http2::{Builder, SendRequest};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use pingers_time::{MonotonicTime, Timer};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::TransportError;
use crate::endpoint::{Origin, Security};
use crate::timer::within;
use crate::tls::ALPN_H2;

/// A request body: all at once, or streamed from a channel (the live channel's chunks).
pub(crate) enum OutBody {
    /// The whole body; `None` once sent.
    Full(Option<Bytes>),
    /// One DATA frame per message; the body ends when every sender is dropped.
    Stream(mpsc::Receiver<Bytes>),
}

impl OutBody {
    pub(crate) fn full(bytes: Vec<u8>) -> Self {
        Self::Full(Some(Bytes::from(bytes)))
    }

    pub(crate) fn empty() -> Self {
        Self::Full(None)
    }
}

impl Body for OutBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        match self.get_mut() {
            Self::Full(bytes) => Poll::Ready(bytes.take().map(|bytes| Ok(Frame::data(bytes)))),
            Self::Stream(receiver) => receiver
                .poll_recv(context)
                .map(|message| message.map(|bytes| Ok(Frame::data(bytes)))),
        }
    }

    fn is_end_stream(&self) -> bool {
        matches!(self, Self::Full(None))
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Full(bytes) => SizeHint::with_exact(
                bytes
                    .as_ref()
                    .map_or(0, |bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
            ),
            Self::Stream(_) => SizeHint::default(),
        }
    }
}

/// The tasks of one connection. Dropping the last handle aborts them.
type Tasks = Arc<Mutex<JoinSet<()>>>;

/// Spawns hyper's tasks into the connection's [`JoinSet`]. Holds it weakly, so the tasks do
/// not keep their own owner alive; once the connection is gone, new tasks are not started.
#[derive(Clone)]
struct OwnedExecutor {
    tasks: Weak<Mutex<JoinSet<()>>>,
}

impl<F> hyper::rt::Executor<F> for OwnedExecutor
where
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, future: F) {
        if let Some(tasks) = self.tasks.upgrade() {
            let mut tasks = tasks.lock().unwrap_or_else(PoisonError::into_inner);
            // Reap finished tasks so a long-lived connection does not accumulate them.
            while tasks.try_join_next().is_some() {}
            tasks.spawn(future);
        }
    }
}

/// A TCP stream, with or without TLS.
enum Io {
    Cleartext(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Io {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Cleartext(stream) => Pin::new(stream).poll_read(context, buffer),
            Self::Tls(stream) => Pin::new(stream).poll_read(context, buffer),
        }
    }
}

impl AsyncWrite for Io {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Cleartext(stream) => Pin::new(stream).poll_write(context, buffer),
            Self::Tls(stream) => Pin::new(stream).poll_write(context, buffer),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Cleartext(stream) => Pin::new(stream).poll_flush(context),
            Self::Tls(stream) => Pin::new(stream).poll_flush(context),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Cleartext(stream) => Pin::new(stream).poll_shutdown(context),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(context),
        }
    }
}

/// One open HTTP/2 connection and the tasks that drive it.
struct Connection {
    sender: SendRequest<OutBody>,
    _tasks: Tasks,
}

/// Why a request was not answered.
#[derive(Debug)]
pub(crate) enum SendError {
    /// No connection could be made, so nothing was sent: another relay may be tried.
    Unreachable(TransportError),
    /// The request may have reached the other side; it must not be repeated elsewhere.
    Failed(TransportError),
}

impl SendError {
    pub(crate) fn into_error(self) -> TransportError {
        match self {
            Self::Unreachable(error) | Self::Failed(error) => error,
        }
    }
}

/// HTTP/2 connections for one transport. A transport never shares its client with another,
/// so identified and anonymous requests never share a connection (design B.12).
pub(crate) struct HttpClient {
    tls: Arc<ClientConfig>,
    timer: Arc<dyn Timer>,
    connect_timeout: Duration,
    connections: tokio::sync::Mutex<HashMap<Origin, Connection>>,
}

impl HttpClient {
    pub(crate) fn new(
        tls: Arc<ClientConfig>,
        timer: Arc<dyn Timer>,
        connect_timeout: Duration,
    ) -> Self {
        Self {
            tls,
            timer,
            connect_timeout,
            connections: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Sends `request` to `origin` and waits for the response head until `deadline`.
    /// Cancellation-safe: dropping the future resets the stream and leaves the connection.
    pub(crate) async fn send(
        &self,
        origin: &Origin,
        request: Request<OutBody>,
        deadline: MonotonicTime,
    ) -> Result<Response<Incoming>, SendError> {
        let mut sender = self.sender(origin).await.map_err(SendError::Unreachable)?;
        match within(&*self.timer, deadline, sender.send_request(request)).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => {
                self.forget_if_closed(origin).await;
                Err(SendError::Failed(TransportError::Connection))
            }
            Err(timeout) => Err(SendError::Failed(timeout)),
        }
    }

    /// An open connection's sender, connecting first if there is none or it has closed.
    async fn sender(&self, origin: &Origin) -> Result<SendRequest<OutBody>, TransportError> {
        let mut connections = self.connections.lock().await;
        if let Some(connection) = connections.get(origin)
            && !connection.sender.is_closed()
        {
            return Ok(connection.sender.clone());
        }
        connections.remove(origin);
        let deadline = self.timer.now().saturating_add(self.connect_timeout);
        let connection = within(&*self.timer, deadline, connect(origin, &self.tls)).await??;
        let sender = connection.sender.clone();
        connections.insert(origin.clone(), connection);
        Ok(sender)
    }

    async fn forget_if_closed(&self, origin: &Origin) {
        let mut connections = self.connections.lock().await;
        if connections
            .get(origin)
            .is_some_and(|connection| connection.sender.is_closed())
        {
            connections.remove(origin);
        }
    }
}

/// Opens TCP, TLS (when the origin uses it) and the HTTP/2 handshake.
async fn connect(origin: &Origin, tls: &Arc<ClientConfig>) -> Result<Connection, TransportError> {
    let tcp = TcpStream::connect((origin.host.as_str(), origin.port))
        .await
        .map_err(|_| TransportError::Connection)?;
    tcp.set_nodelay(true)
        .map_err(|_| TransportError::Connection)?;
    let io = match origin.security {
        Security::Cleartext => Io::Cleartext(tcp),
        Security::Tls => {
            let name = ServerName::try_from(origin.host.clone())
                .map_err(|_| TransportError::InvalidUrl)?;
            let stream = TlsConnector::from(Arc::clone(tls))
                .connect(name, tcp)
                .await
                .map_err(|_| TransportError::Connection)?;
            // Without h2 there is no full-duplex streaming; refuse rather than fall back.
            if stream.get_ref().1.alpn_protocol() != Some(ALPN_H2) {
                return Err(TransportError::Connection);
            }
            Io::Tls(Box::new(stream))
        }
    };
    let tasks: Tasks = Arc::new(Mutex::new(JoinSet::new()));
    let executor = OwnedExecutor {
        tasks: Arc::downgrade(&tasks),
    };
    let (sender, connection) = Builder::new(executor)
        .handshake(TokioIo::new(io))
        .await
        .map_err(|_| TransportError::Connection)?;
    tasks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .spawn(async move {
            // A broken connection shows up as a closed sender; there is nothing else to report.
            let _ = connection.await;
        });
    Ok(Connection {
        sender,
        _tasks: tasks,
    })
}

/// The next DATA frame's bytes, `None` at the end of the body. Cancellation-safe.
pub(crate) async fn next_data(body: &mut Incoming) -> Result<Option<Bytes>, TransportError> {
    loop {
        let frame = poll_fn(|context| Pin::new(&mut *body).poll_frame(context)).await;
        match frame {
            None => return Ok(None),
            Some(Err(_)) => return Err(TransportError::Connection),
            Some(Ok(frame)) => {
                // Trailers carry nothing Pingers uses.
                if let Ok(data) = frame.into_data() {
                    return Ok(Some(data));
                }
            }
        }
    }
}

/// The whole body, refusing more than `limit` bytes.
pub(crate) async fn read_body(
    body: &mut Incoming,
    limit: usize,
) -> Result<Vec<u8>, TransportError> {
    let mut all = Vec::new();
    while let Some(data) = next_data(body).await? {
        if data.len() > limit.saturating_sub(all.len()) {
            return Err(TransportError::TooLarge);
        }
        all.extend_from_slice(&data);
    }
    Ok(all)
}
