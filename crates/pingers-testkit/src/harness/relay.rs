// SPDX-License-Identifier: AGPL-3.0-only
//! The harness's OHTTP relay: pingers-ohttp-relay itself, through its library, in front of
//! the harness gateway, so harness tests exercise the relay operators run.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use pingers_ohttp_relay::{Config, Relay};
use tokio::net::TcpListener;

/// Relays to the gateway at `gateway` until `shutdown` completes, then drops its connections.
pub(super) async fn serve(
    listener: TcpListener,
    gateway: SocketAddr,
    shutdown: impl Future<Output = ()>,
) {
    let Ok(listen) = listener.local_addr() else {
        return;
    };
    // The relay's default paths (`/ohttp`, `/v1/ohttp/config`) are the harness gateway's.
    let Ok(config) = Config::parse(&format!(
        "listen = \"{listen}\"\n\
         gateway = \"http://{gateway}/ohttp\"\n\
         gateway_key_config = \"http://{gateway}/v1/ohttp/config\"\n"
    )) else {
        return;
    };
    // `serve` returns only once `shutdown` completes.
    let _stopped =
        pingers_ohttp_relay::serve(listener, Arc::new(Relay::new(config)), shutdown).await;
}
