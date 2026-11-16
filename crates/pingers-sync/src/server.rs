// SPDX-License-Identifier: AGPL-3.0-only
//! What the client learns about its server before using it: the gateway key check through the
//! relay (the reachability check), the server's parameters, and a listed push relay's key
//! config checked against its pins (design B.4.9, B.12). All of it is public, anonymous-class
//! traffic: no credential, account, Party or mailbox goes with it.

use pingers_crypto::{PushRelayPublicKey, key_config_hash};
use pingers_ohttp::{KeyPins, OhttpError};
use pingers_proto::{Params, PushRelayConfig, RelayId};
use pingers_transport::{PrivacyClass, Transport, TransportError};

use crate::engine::Response;
use crate::{ApiRequest, Route, SyncEngine, SyncError, WakeBudget};

/// A server's parameters fetched direct (`GET /v1/params` on the identified route), before
/// the device knows which OHTTP relays the server advertises.
///
/// Used once per server, when a server is added by address or an invite names one this device
/// has not used. It tells the server this address is about to use it, which the registration
/// that follows on the same direct route (design B.12, `Identified`) tells it anyway; nothing
/// else goes with it. Everything after it goes through the advertised relay, including the
/// parameters again (so a server cannot show this address a relay list of its own without
/// the relay seeing a different one), the gateway key and the push relay key.
///
/// # Errors
///
/// [`SyncError::Transport`] if the server cannot be reached; [`SyncError::Status`];
/// [`SyncError::BadResponse`].
pub async fn params_direct(direct: &dyn Transport) -> Result<Params, SyncError> {
    let request = ApiRequest::params();
    let route = request.route();
    let answer = direct
        .request(PrivacyClass::Identified, request.to_binary(None))
        .await
        .map_err(SyncError::Transport)?;
    Response::new(route, answer).success()?.decode()
}

/// A push relay key config that matched its pins (or was taken on first use).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedPushRelay {
    /// The relay, as the server lists it: what sealed pushes name.
    pub relay: RelayId,
    /// The key to seal push tokens to.
    pub key: PushRelayPublicKey,
    /// What the relay published with it (platforms, longest token lifetime, operator).
    pub config: PushRelayConfig,
}

impl SyncEngine {
    /// Fetches the gateway's key config afresh through the relay and checks it against the
    /// pin. Nothing reaches the gateway; the check is the fetch every first request makes.
    ///
    /// # Errors
    ///
    /// [`SyncError::NoRelay`] (route [`Route::OhttpConfig`]) if no relay answered;
    /// [`SyncError::RelayKeyMismatch`]; [`SyncError::BudgetExhausted`];
    /// [`SyncError::Transport`].
    pub async fn check_relay(&self, budget: WakeBudget) -> Result<(), SyncError> {
        let route = Route::OhttpConfig;
        match self
            .within(budget, self.transport.check_gateway_key())
            .await
        {
            None => Err(SyncError::BudgetExhausted { route }),
            Some(Ok(())) => Ok(()),
            Some(Err(TransportError::NoRelay)) => Err(SyncError::NoRelay { route }),
            Some(Err(TransportError::Ohttp(OhttpError::KeyMismatch))) => {
                Err(SyncError::RelayKeyMismatch)
            }
            Some(Err(error)) => Err(SyncError::Transport(error)),
        }
    }

    /// The server's parameters (`GET /v1/params`, through the relay), adopted for credentials
    /// ([`SyncEngine::apply_params`]). Its clock is observed for skew correction.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; [`SyncError::BadResponse`]; [`SyncError::Store`]; as
    /// [`SyncEngine::send`].
    pub async fn params(&self, budget: WakeBudget) -> Result<Params, SyncError> {
        let params = self.read_params(budget).await?;
        self.apply_params(&params).await?;
        Ok(params)
    }

    /// The server's parameters through the relay, and nothing kept: for a server only being
    /// looked at, which has no rows in the store yet.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; [`SyncError::BadResponse`]; as [`SyncEngine::send`].
    pub async fn read_params(&self, budget: WakeBudget) -> Result<Params, SyncError> {
        let params: Params = self
            .exchange(ApiRequest::params(), budget)
            .await?
            .success()?
            .decode()?;
        let _status = self.observe_server_time(params.server_time);
        Ok(params)
    }

    /// A listed push relay's key config (`GET /v1/push-relays/{id}/config`, through the
    /// relay), checked against `pins`. With `pins` `None` the config is taken as it comes
    /// (a build without pins: the caller remembers the hash and passes it next time).
    ///
    /// # Errors
    ///
    /// [`SyncError::PushRelayKeyMismatch`] if its hash is not pinned: seal nothing to it;
    /// [`SyncError::Status`]; [`SyncError::BadResponse`]; as [`SyncEngine::send`].
    pub async fn push_relay_key(
        &self,
        relay: RelayId,
        pins: Option<&KeyPins>,
        budget: WakeBudget,
    ) -> Result<VerifiedPushRelay, SyncError> {
        let config: PushRelayConfig = self
            .exchange(ApiRequest::push_relay_config(relay), budget)
            .await?
            .success()?
            .decode()?;
        let hash = key_config_hash(&config.key_config.to_bytes());
        if pins.is_some_and(|pins| !pins.contains(&hash)) {
            return Err(SyncError::PushRelayKeyMismatch);
        }
        Ok(VerifiedPushRelay {
            relay,
            key: PushRelayPublicKey::new(
                config.key_config.key_id.get(),
                config.key_config.public_key,
            ),
            config,
        })
    }
}
