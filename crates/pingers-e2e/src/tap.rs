// SPDX-License-Identifier: AGPL-3.0-only
//! A TCP forwarder between the OHTTP relay and the server: counts the bytes in each direction
//! and connects to the server from its own loopback address, so the server sees the relay's
//! address and nothing else on its gateway.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpSocket};
use tokio::task::JoinSet;

/// What crossed between the relay and the server.
#[derive(Debug, Default)]
pub struct Tap {
    to_server: AtomicU64,
    from_server: AtomicU64,
    connections: AtomicU64,
    sources: Mutex<Vec<IpAddr>>,
    egress: Mutex<Vec<IpAddr>>,
}

impl Tap {
    /// Bytes the relay sent to the server so far (encapsulated requests and HTTP/2 framing).
    #[must_use]
    pub fn bytes_to_server(&self) -> u64 {
        self.to_server.load(Ordering::Relaxed)
    }

    /// Bytes the server sent back to the relay so far.
    #[must_use]
    pub fn bytes_from_server(&self) -> u64 {
        self.from_server.load(Ordering::Relaxed)
    }

    /// Connections forwarded so far.
    #[must_use]
    pub fn connections(&self) -> u64 {
        self.connections.load(Ordering::Relaxed)
    }

    /// The address of every peer that connected to the tap (the relay), one per connection.
    #[must_use]
    pub fn sources(&self) -> Vec<IpAddr> {
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The local address of every connection the tap opened to the server: what the server saw
    /// as the peer.
    #[must_use]
    pub fn egress(&self) -> Vec<IpAddr> {
        self.egress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn count(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

async fn pump(mut from: OwnedReadHalf, mut to: OwnedWriteHalf, counter: Arc<Tap>, up: bool) {
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = match from.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let total = if up {
            &counter.to_server
        } else {
            &counter.from_server
        };
        total.fetch_add(count(read), Ordering::Relaxed);
        let Some(chunk) = buffer.get(..read) else {
            break;
        };
        if to.write_all(chunk).await.is_err() {
            break;
        }
    }
    // The other side sees the end of the stream; it may already be gone.
    let _ = to.shutdown().await;
}

/// Forwards every connection on `listener` to `server`, connecting from `egress`, until
/// `shutdown` completes.
#[expect(
    clippy::too_many_arguments,
    reason = "the listener, both ends, the counters and the stop signal"
)]
pub(crate) async fn serve(
    listener: TcpListener,
    server: SocketAddr,
    egress: IpAddr,
    tap: Arc<Tap>,
    shutdown: impl Future<Output = ()>,
) {
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((inbound, peer)) = accepted else {
                    continue;
                };
                let tap = Arc::clone(&tap);
                connections.spawn(async move {
                    let socket = match server {
                        SocketAddr::V4(_) => TcpSocket::new_v4(),
                        SocketAddr::V6(_) => TcpSocket::new_v6(),
                    };
                    let Ok(socket) = socket else { return };
                    if socket.bind(SocketAddr::new(egress, 0)).is_err() {
                        return;
                    }
                    let Ok(outbound) = socket.connect(server).await else {
                        return;
                    };
                    let _ = inbound.set_nodelay(true);
                    let _ = outbound.set_nodelay(true);
                    tap.connections.fetch_add(1, Ordering::Relaxed);
                    tap.sources
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(peer.ip());
                    if let Ok(local) = outbound.local_addr() {
                        tap.egress
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(local.ip());
                    }
                    let (in_read, in_write) = inbound.into_split();
                    let (out_read, out_write) = outbound.into_split();
                    tokio::join!(
                        pump(in_read, out_write, Arc::clone(&tap), true),
                        pump(out_read, in_write, tap, false),
                    );
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
}
