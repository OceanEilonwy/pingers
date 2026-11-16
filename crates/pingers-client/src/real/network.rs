// SPDX-License-Identifier: AGPL-3.0-only
//! The Pingers instance a device talks to, and the transport [`Router`] built from it.
//!
//! The hosts come from outside: the app's build configuration (environment variables at
//! build time) or an invite link. Nothing here names a deployment.

use std::sync::Arc;

use pingers_ohttp::KeyPins;
use pingers_proto::{SecretText, ServerId};
use pingers_transport::{
    DirectConfig, DirectTransport, KeyPin, OhttpConfig, OhttpTransport, Relay, Router, Routes,
    TlsRoots, Transport, certificates_from_pem,
};

use pingers_proto::AnonymousCredential;
use pingers_store::{ServerRecord, ServerTrust};
use pingers_sync::{
    CommitAttempts, CredentialPolicy, ProcessRole, ProverStatus, QueueCapacity, SyncEngine,
    SyncParts, SyncSettings, TokenStock,
};

use super::{Backend, Outside};
use crate::ClientError;

/// Where this device's server, OHTTP relay and push relay are, and the keys they must have.
///
/// Official builds carry pin sets for the gateway and the push relay (the key served now and
/// the next one, design B.12 and proposal §2.1); with a set, first-use pinning is off. The
/// Privacy Pass issuer key is not pinned at build time: it rotates weekly.
#[derive(Clone, Debug)]
pub struct Instance {
    /// The server origin (`https://...`), also the OHTTP gateway.
    pub server: ServerId,
    /// The OHTTP relay's origin (`https://...`): requests go to `/ohttp`, the gateway's key
    /// configuration comes from `/v1/ohttp/config`.
    pub relay: String,
    /// The push relay's origin, for sealed push registration: the server must list it in
    /// `/v1/params`.
    pub push_relay: Option<String>,
    /// The gateway key config hashes allowed, or `None` to pin on first use.
    pub gateway_pins: Option<KeyPins>,
    /// The push relay key config hashes allowed, or `None` to pin on first use.
    pub push_relay_pins: Option<KeyPins>,
    /// Which certificate authorities HTTPS trusts.
    pub trust: Trust,
    /// The operator's registration code for this device's first registration (design B.1),
    /// for the first owner of a server that requires one: a debug build's
    /// `PINGERS_DEV_REGISTRATION_CODE`. Release builds pass `None`; other devices get their
    /// code from the invite link.
    pub registration_code: Option<SecretText>,
}

/// Which certificate authorities HTTPS connections trust.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Mozilla's root program: every release build.
    Mozilla,
    /// Only these DER certificates: a debug build pointed at a local test stack's own CA.
    Only(Vec<Vec<u8>>),
}

impl Trust {
    /// Only the certificates in a PEM bundle.
    ///
    /// # Errors
    ///
    /// [`ClientError::Transport`] if the bundle holds no valid certificate.
    pub fn from_pem(pem: &str) -> Result<Self, ClientError> {
        Ok(Self::Only(certificates_from_pem(pem)?))
    }

    pub(crate) fn roots(&self) -> Result<TlsRoots, ClientError> {
        Ok(match self {
            Self::Mozilla => TlsRoots::mozilla(),
            Self::Only(certificates) => TlsRoots::from_der(certificates)?,
        })
    }
}

/// The relay's origin from a server's relay setting, which may name the origin or, as servers
/// advertise it (`ohttp_relay_urls`) and deployments configure it, the relay URL ending in
/// `/ohttp`: both mean the same relay, never `.../ohttp/ohttp`.
pub(crate) fn relay_origin(setting: &str) -> &str {
    let trimmed = setting.trim_end_matches('/');
    trimmed.strip_suffix("/ohttp").unwrap_or(trimmed)
}

impl Instance {
    /// The built-in server as the store keeps it: its relay, push relay and the build's pins,
    /// refreshed from the build at every start.
    #[must_use]
    pub fn record(&self) -> ServerRecord {
        ServerRecord {
            server: self.server.clone(),
            ohttp_relays: vec![self.relay.clone()],
            push_relay: self.push_relay.clone(),
            gateway_pins: self
                .gateway_pins
                .as_ref()
                .map(|pins| pins.hashes().to_vec())
                .unwrap_or_default(),
            push_relay_pins: self
                .push_relay_pins
                .as_ref()
                .map(|pins| pins.hashes().to_vec())
                .unwrap_or_default(),
            trust: ServerTrust::BuiltIn,
        }
    }
}

/// The direct route to `server`, for identified requests (and the one direct fetch of a new
/// server's parameters, pingers-sync's `params_direct`).
pub(crate) fn direct(
    server: &ServerId,
    trust: &Trust,
    outside: &Outside,
) -> Result<Arc<dyn Transport>, ClientError> {
    Ok(Arc::new(DirectTransport::new(
        &DirectConfig::new(server.as_str().to_owned(), trust.roots()?),
        Arc::clone(&outside.timer),
    )?))
}

/// The router to one server: anonymous and stream requests through its OHTTP relays (most
/// preferred first), identified requests direct. `pin` is what the gateway key must match:
/// the server's pins, or first use only while a new server is being looked at before the
/// user trusts it (nothing is recorded then).
/// Which server a router reaches, through which relays, under which pin.
pub(crate) struct Route<'route> {
    pub(crate) server: &'route ServerId,
    pub(crate) relays: &'route [String],
    pub(crate) pin: KeyPin,
}

pub(crate) fn router(
    route: Route<'_>,
    trust: &Trust,
    outside: &Outside,
) -> Result<Arc<dyn Transport>, ClientError> {
    let Route {
        server,
        relays,
        pin,
    } = route;
    let relays = relays
        .iter()
        .map(|setting| {
            let origin = relay_origin(setting);
            Relay::new(
                &format!("{origin}/ohttp"),
                &format!("{origin}/v1/ohttp/config"),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ohttp: Arc<dyn Transport> = Arc::new(OhttpTransport::new(
        OhttpConfig::new(relays, pin, trust.roots()?),
        Arc::clone(&outside.timer),
        (outside.rng)(),
    )?);
    Ok(Arc::new(Router::new(Routes {
        anonymous: Arc::clone(&ohttp),
        identified: direct(server, trust, outside)?,
        stream: ohttp,
    })?))
}

/// pingers-sync's engine for one server, for the whole process. The server needs a row in the
/// store before the engine reads or writes anything.
///
/// Credentials start as Privacy Pass only until the server's `/v1/params` say what it accepts
/// (read before credentials are prepared, `sync.rs`). The prover is allowed: pingers-sync proves
/// only once its self-test has passed and the leaf is in an accepted root (design B.4.8), and
/// uses Privacy Pass otherwise.
pub(crate) fn engine(
    server: &ServerId,
    transport: Arc<dyn Transport>,
    backend: &Backend,
) -> Result<Arc<SyncEngine>, ClientError> {
    let outside = &backend.outside;
    Ok(Arc::new(SyncEngine::new(
        SyncParts {
            transport,
            database: Arc::clone(&backend.database),
            clock: Arc::clone(&outside.clock),
            timer: Arc::clone(&outside.timer),
            rng: (outside.rng)(),
            self_test: backend.self_test.clone(),
        },
        SyncSettings {
            server: server.clone(),
            role: ProcessRole::MainApp,
            credentials: CredentialPolicy {
                accepted: vec![AnonymousCredential::PrivacyPass],
                prover: ProverStatus::Enabled,
            },
            token_stock: TokenStock::design_default()?,
            commit_attempts: CommitAttempts::default(),
            queue_capacity: QueueCapacity::default(),
            self_test: outside.prover_self_test,
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::relay_origin;

    #[test]
    fn a_relay_is_named_by_its_origin_or_its_ohttp_url() {
        for setting in [
            "https://relay.example",
            "https://relay.example/",
            "https://relay.example/ohttp",
            "https://relay.example/ohttp/",
        ] {
            assert_eq!(relay_origin(setting), "https://relay.example", "{setting}");
        }
        assert_eq!(
            relay_origin("https://relay.example/ohttp-relay"),
            "https://relay.example/ohttp-relay"
        );
    }
}
