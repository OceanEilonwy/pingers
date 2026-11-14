// SPDX-License-Identifier: AGPL-3.0-only
//! What the client learns about its server before using it: the gateway key check through the
//! relay (the reachability check), the server's parameters, and a listed push relay's key
//! config checked against its pins (design B.4.9, B.12). All of it is public, anonymous-class
//! traffic: no credential, account, Party or mailbox goes with it.

use pingers_crypto::{PushRelayPublicKey, key_config_hash};
use pingers_ohttp::{KeyPins, OhttpError};
use pingers_proto::{Params, PushRelayConfig, RelayId, ScheduledPushRelayConfig};
use pingers_time::UnixSeconds;
use pingers_transport::TransportError;

use crate::{ApiRequest, Route, SyncEngine, SyncError, WakeBudget};

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

/// A push relay key announced to activate later, already checked against the pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledPushRelay {
    /// The key, as it will be published.
    pub verified: VerifiedPushRelay,
    /// When the relay starts publishing it and opening blobs sealed to it.
    pub activates_at: UnixSeconds,
}

/// What the push relay says about its next key (`GET /v1/config/next`).
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub enum NextPushRelayKey {
    /// No rotation is scheduled.
    NotScheduled,
    /// A rotation is scheduled to a key the pins allow: pre-seal to it from its activation.
    Pinned(ScheduledPushRelay),
    /// A rotation is scheduled to a key the pins do not allow. Nothing is sealed to it; once it
    /// is published, [`SyncEngine::push_relay_key`] reports the mismatch.
    Unpinned {
        /// When it is due to be published.
        activates_at: UnixSeconds,
    },
}

fn verified(relay: RelayId, config: PushRelayConfig) -> VerifiedPushRelay {
    VerifiedPushRelay {
        relay,
        key: PushRelayPublicKey::new(config.key_config.key_id.get(), config.key_config.public_key),
        config,
    }
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

    /// The server's parameters (`GET /v1/params`, through the relay). Its clock is observed
    /// for skew correction.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; [`SyncError::BadResponse`]; as [`SyncEngine::send`].
    pub async fn params(&self, budget: WakeBudget) -> Result<Params, SyncError> {
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
        if !allowed(&config, pins) {
            return Err(SyncError::PushRelayKeyMismatch);
        }
        Ok(verified(relay, config))
    }

    /// The key a listed push relay publishes next and when (`GET
    /// /v1/push-relays/{id}/config/next`, through the relay), checked against `pins` like
    /// [`SyncEngine::push_relay_key`]. Fetched ahead of a rotation so devices switch to it on
    /// time without asking again, and seal nothing to the old key past its retirement.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] for an unlisted relay (404 means none is scheduled only for a
    /// listed one: the server answers 404 for both, and both mean nothing to pre-seal);
    /// [`SyncError::BadResponse`]; as [`SyncEngine::send`].
    pub async fn push_relay_next_key(
        &self,
        relay: RelayId,
        pins: Option<&KeyPins>,
        budget: WakeBudget,
    ) -> Result<NextPushRelayKey, SyncError> {
        let response = self
            .exchange(ApiRequest::push_relay_next_config(relay), budget)
            .await?;
        if response.status() == 404 {
            return Ok(NextPushRelayKey::NotScheduled);
        }
        let scheduled: ScheduledPushRelayConfig = response.success()?.decode()?;
        if !allowed(&scheduled.config, pins) {
            return Ok(NextPushRelayKey::Unpinned {
                activates_at: scheduled.activates_at,
            });
        }
        Ok(NextPushRelayKey::Pinned(ScheduledPushRelay {
            verified: verified(relay, scheduled.config),
            activates_at: scheduled.activates_at,
        }))
    }
}

/// Whether `pins` allow the config's key; no pins (a build without them) allow any.
fn allowed(config: &PushRelayConfig, pins: Option<&KeyPins>) -> bool {
    let hash = key_config_hash(&config.key_config.to_bytes());
    pins.is_none_or(|pins| pins.contains(&hash))
}
