// SPDX-License-Identifier: AGPL-3.0-only
//! The Pingers instance a device talks to, and the transport [`Router`] built from it.
//!
//! The hosts come from outside: the app's build configuration (environment variables at
//! build time) or an invite link. Nothing here names a deployment.

use std::sync::Arc;

use pingers_proto::{KeyConfigHash, ServerId};
use pingers_time::Timer;
use pingers_transport::{
    DirectConfig, DirectTransport, KeyPin, OhttpConfig, OhttpTransport, Relay, Router, Routes,
    TlsRoots, Transport,
};

use super::Outside;
use crate::ClientError;

/// Where this device's server, OHTTP relay and Privacy Pass issuer are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// The server origin (`https://...`), also the OHTTP gateway.
    pub server: ServerId,
    /// The OHTTP relay's origin (`https://...`): requests go to `/ohttp`, the gateway's key
    /// configuration comes from `/v1/ohttp/config`.
    pub relay: String,
    /// The push relay's origin, for sealed push registration.
    pub push_relay: Option<String>,
    /// The pinned hash of the gateway's key configuration, or `None` to pin on first use.
    pub gateway_key: Option<KeyConfigHash>,
    /// The Privacy Pass issuer's public key (49-byte compressed P-384), if known yet; the
    /// server's `/v1/config` gives it otherwise.
    pub token_issuer_key: Option<Vec<u8>>,
}

/// The router over the instance: anonymous and stream requests through the OHTTP relay,
/// identified requests direct to the server. TLS trusts the Mozilla root store.
pub(crate) fn router(
    instance: &Instance,
    timer: Arc<dyn Timer>,
    outside: &Outside,
) -> Result<Arc<dyn Transport>, ClientError> {
    let origin = instance.relay.trim_end_matches('/');
    let relay = Relay::new(
        &format!("{origin}/ohttp"),
        &format!("{origin}/v1/ohttp/config"),
    )?;
    let pin = instance
        .gateway_key
        .map_or(KeyPin::FirstUse, KeyPin::Pinned);
    let ohttp: Arc<dyn Transport> = Arc::new(OhttpTransport::new(
        OhttpConfig::new(vec![relay], pin, TlsRoots::mozilla()),
        Arc::clone(&timer),
        (outside.rng)(),
    )?);
    let direct = DirectTransport::new(
        &DirectConfig::new(instance.server.as_str().to_owned(), TlsRoots::mozilla()),
        timer,
    )?;
    Ok(Arc::new(Router::new(Routes {
        anonymous: Arc::clone(&ohttp),
        identified: Arc::new(direct),
        stream: ohttp,
    })?))
}
