// SPDX-License-Identifier: AGPL-3.0-only
//! What a [`Link`] learns from its server, and the wake-up pushes sealed with it.
//!
//! - The server's `/v1/params`, once per process: the map host and the push relays it lists,
//!   with who runs them.
//! - The push relay's keys: a server's push relay is named by origin, the server lists it with a
//!   relay id, and pingers-sync fetches its published key and the key it publishes next (with
//!   its activation time) through that server's OHTTP relay, checking both hashes against the
//!   relay's pins (the build's for the build's own push relay, otherwise those recorded with the
//!   server). Nothing is pinned on first use: a push relay without pins gets no push token.
//!   Fetched again once a day, so a process that lives for days still learns of a rotation.
//! - pingers-sync's `WakePush`, sealed from the platform's push token for the key to use now:
//!   the next key from an hour after its activation, otherwise the published one, with
//!   `not_after` no later than the old key's retirement (activation plus the key overlap). It
//!   is sealed again when the token or the key changes, and before its `not_after` comes near,
//!   so pingers-sync's maintenance renews every registration in time (design B.4.9). Each
//!   server's engine holds its own seals: its mailboxes and subscriptions name its push relay.

use std::sync::{Arc, Mutex, PoisonError};

use pingers_crypto::PushRelayPublicKey;
use pingers_ohttp::KeyPins;
use pingers_proto::{
    PUSH_RELAY_KEY_OVERLAP, Params, PushClass, PushTokenPlaintext, RelayRef, TimeToLive, Urgency,
};
use pingers_sync::{
    NextPushRelayKey, PushSeal, RENEW_AHEAD, ScheduledPushRelay, SyncError, VerifiedPushRelay,
    WakeBudget, WakePush,
};
use pingers_time::UnixSeconds;

use super::records::corrupt;
use super::servers::{Link, host_of};
use crate::snapshot::{Operator, PushDowngrade, ServerOperators};
use crate::{ClientError, Platform, PushTokens, Unavailable};

/// The longest a sealed token is good for: a Party subscription lives 30 days (design B.4.9),
/// unless the relay allows less.
pub(super) const TOKEN_LIFETIME: core::time::Duration = core::time::Duration::from_hours(30 * 24);

/// How often the relay's keys are fetched again in a long-lived process.
const KEY_REFRESH: core::time::Duration = core::time::Duration::from_hours(24);

/// How long after the next key's activation devices switch to it: the relay opens blobs sealed
/// to it only from its activation, so a device clock a little ahead must not seal to it early.
/// The old key keeps opening blobs for the whole overlap.
const SWITCH_AFTER: core::time::Duration = core::time::Duration::from_hours(1);

/// A server's push relay and the pins its key must match.
#[derive(Clone, Debug)]
pub(crate) struct PushRelaySetup {
    /// The push relay's origin.
    pub(crate) origin: String,
    /// The pins; `None` means none are known, and no token is sealed to it.
    pub(crate) pins: Option<KeyPins>,
}

/// The relay's keys as last fetched.
#[derive(Clone, Debug)]
pub(super) struct RelayKeys {
    pub(super) active: VerifiedPushRelay,
    pub(super) next: Option<ScheduledPushRelay>,
    pub(super) fetched_at: UnixSeconds,
}

/// The key to seal to now, and the latest `not_after` a token sealed to it may carry.
pub(super) struct SealTo<'keys> {
    pub(super) relay: &'keys VerifiedPushRelay,
    pub(super) until: Option<UnixSeconds>,
}

impl RelayKeys {
    /// Whether they were fetched within the last [`KEY_REFRESH`].
    pub(super) fn fresh(&self, now: UnixSeconds) -> bool {
        self.fetched_at
            .checked_add(KEY_REFRESH)
            .is_some_and(|due| now < due)
    }

    pub(super) fn seal_to(&self, now: UnixSeconds) -> SealTo<'_> {
        match &self.next {
            Some(next)
                if next
                    .activates_at
                    .checked_add(SWITCH_AFTER)
                    .is_some_and(|switch| now >= switch) =>
            {
                SealTo {
                    relay: &next.verified,
                    until: None,
                }
            }
            Some(next) => SealTo {
                relay: &self.active,
                until: next.activates_at.checked_add(PUSH_RELAY_KEY_OVERLAP),
            },
            None => SealTo {
                relay: &self.active,
                until: None,
            },
        }
    }
}

/// What a server's engine's seals were made from in this process.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sealed {
    tokens: PushTokens,
    key: PushRelayPublicKey,
    not_after: UnixSeconds,
}

impl Sealed {
    /// Whether seals made from `tokens` to `key` would add nothing now: same token, same key,
    /// and not yet within the renewal window.
    fn still_good(&self, tokens: &PushTokens, key: &PushRelayPublicKey, now: UnixSeconds) -> bool {
        self.tokens == *tokens
            && self.key == *key
            && now
                .checked_add(RENEW_AHEAD)
                .is_some_and(|soon| self.not_after > soon)
    }
}

/// The next key an earlier fetch announced, kept when the next-key fetch was not answered,
/// as long as the published key is still the one it was announced beside.
fn carried_next(
    previous: Option<&RelayKeys>,
    active: &VerifiedPushRelay,
) -> Option<ScheduledPushRelay> {
    previous
        .filter(|keys| keys.active.key == active.key)
        .and_then(|keys| keys.next.clone())
}

/// What a server told this process, and the push relay set up for it.
pub(crate) struct Learned {
    push_relay: Option<PushRelaySetup>,
    /// Where the platform's push token is read when seals must be made again (a new process).
    platform: Option<Arc<dyn Platform>>,
    params: Mutex<Option<Params>>,
    keys: Mutex<Option<RelayKeys>>,
    sealed: Mutex<Option<Sealed>>,
    /// Why wake-up pushes could not be sealed for this server the last time they were tried,
    /// for the snapshot; `None` once they were.
    problem: Mutex<Option<PushDowngrade>>,
}

impl Learned {
    pub(crate) const fn new(
        push_relay: Option<PushRelaySetup>,
        platform: Option<Arc<dyn Platform>>,
    ) -> Self {
        Self {
            push_relay,
            platform,
            params: Mutex::new(None),
            keys: Mutex::new(None),
            sealed: Mutex::new(None),
            problem: Mutex::new(None),
        }
    }

    /// The pins of the push relay this server's pushes go through, if any are known.
    pub(crate) fn push_relay_pins(&self) -> Option<&KeyPins> {
        self.push_relay.as_ref()?.pins.as_ref()
    }

    /// The push relay's origin, if the server has one set up.
    pub(crate) fn push_relay_origin(&self) -> Option<&str> {
        self.push_relay.as_ref().map(|setup| setup.origin.as_str())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether two origins name the same place, ignoring a trailing slash.
pub(crate) fn same_origin(first: &str, second: &str) -> bool {
    first.trim_end_matches('/') == second.trim_end_matches('/')
}

/// Why sealing failed, as the snapshot names it; `None` for a failure that says nothing about
/// push delivery (the network, the store).
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other failure is the network's or the store's, not push delivery's"
)]
pub(super) fn downgrade_of(error: &ClientError) -> Option<PushDowngrade> {
    match error {
        ClientError::Unavailable(Unavailable::WakePushesOff) => Some(PushDowngrade::WakePushesOff),
        ClientError::Unavailable(Unavailable::NoPushToken) => Some(PushDowngrade::NoPushToken),
        ClientError::Unavailable(Unavailable::NoPushRelay) => Some(PushDowngrade::NoPushRelay),
        ClientError::Sync(SyncError::PushRelayKeyMismatch) => Some(PushDowngrade::RelayKeyMismatch),
        _ => None,
    }
}

impl Link {
    /// The server's parameters, fetched through its relay once per process.
    ///
    /// # Errors
    ///
    /// [`ClientError::Sync`].
    pub(crate) async fn server_params(&self) -> Result<Params, ClientError> {
        if let Some(params) = lock(&self.learned.params).clone() {
            return Ok(params);
        }
        let result = self.engine.params(WakeBudget::UNLIMITED).await;
        self.observe(&result);
        let params = result?;
        *lock(&self.learned.params) = Some(params.clone());
        Ok(params)
    }

    /// Starts with parameters read while the server was looked at: kept for this process, and
    /// what they say about credentials handed to the engine (as `SyncEngine::params` does).
    ///
    /// # Errors
    ///
    /// [`ClientError::Sync`] with a store failure.
    pub(crate) async fn learn_params(&self, params: Params) -> Result<(), ClientError> {
        self.engine.apply_params(&params).await?;
        *lock(&self.learned.params) = Some(params);
        Ok(())
    }

    /// The server's parameters if this call is the one that fetched them for this process;
    /// `None` when they were already known.
    ///
    /// # Errors
    ///
    /// [`ClientError::Sync`].
    pub(crate) async fn fetch_params_once(&self) -> Result<Option<Params>, ClientError> {
        if lock(&self.learned.params).is_some() {
            return Ok(None);
        }
        self.server_params().await.map(Some)
    }

    /// Who runs the server and its push relay, and where its default privacy relay and the
    /// push relay are, from the parameters this process read; `None` before they were read or
    /// if the server declares no operator.
    pub(crate) fn operators(&self) -> Option<ServerOperators> {
        let params = lock(&self.learned.params);
        let params = params.as_ref()?;
        let listed = self.learned.push_relay.as_ref().and_then(|setup| {
            params
                .push_relays
                .iter()
                .find(|relay| same_origin(relay.url.as_str(), &setup.origin))
        });
        Some(ServerOperators {
            server: Operator::new(&params.server_operator).ok()?,
            push_relay: listed.and_then(|relay| Operator::new(&relay.operator).ok()),
            relay_host: params
                .ohttp_relays
                .first()
                .and_then(|relay| host_of(relay.as_str())),
            push_relay_host: listed.and_then(|relay| host_of(relay.url.as_str())),
        })
    }

    /// The server's push relay keys, as fetched within the last day: the published key, which
    /// must match the relay's pins, and the next key if one is scheduled and pinned.
    ///
    /// # Errors
    ///
    /// [`Unavailable::NoPushRelay`] if the server has none, does not list it, or no pins are
    /// known for it; [`ClientError::Sync`] (`PushRelayKeyMismatch` raises
    /// `Notice::RelayKeyMismatch`).
    pub(super) async fn relay_keys(&self) -> Result<RelayKeys, ClientError> {
        let setup = self
            .learned
            .push_relay
            .clone()
            .ok_or(Unavailable::NoPushRelay)?;
        let pins = setup.pins.ok_or(Unavailable::NoPushRelay)?;
        let now = self.engine.corrected_now();
        if let Some(keys) = lock(&self.learned.keys).clone()
            && keys.fresh(now)
        {
            return Ok(keys);
        }
        let params = self.server_params().await?;
        let listed = params
            .push_relays
            .iter()
            .find(|relay| same_origin(relay.url.as_str(), &setup.origin))
            .ok_or(Unavailable::NoPushRelay)?;
        let result = self
            .engine
            .push_relay_key(listed.relay_id, Some(&pins), WakeBudget::UNLIMITED)
            .await;
        self.observe(&result);
        let active = result?;
        let next = match self
            .engine
            .push_relay_next_key(listed.relay_id, Some(&pins), WakeBudget::UNLIMITED)
            .await
        {
            Ok(NextPushRelayKey::Pinned(next)) => Some(next),
            Ok(NextPushRelayKey::NotScheduled | NextPushRelayKey::Unpinned { .. }) => None,
            Err(SyncError::Store(error)) => return Err(SyncError::Store(error).into()),
            // Not answered now: keep what an earlier fetch announced for this key.
            Err(_) => carried_next(lock(&self.learned.keys).as_ref(), &active),
        };
        let keys = RelayKeys {
            active,
            next,
            fetched_at: now,
        };
        *lock(&self.learned.keys) = Some(keys.clone());
        Ok(keys)
    }

    /// Seals `tokens` to the key to use now and hands the seals to this server's engine: new
    /// registrations carry them, and maintenance seals the older ones again.
    ///
    /// # Errors
    ///
    /// As [`Link::relay_keys`].
    pub(crate) async fn seal_wake_push(&self, tokens: &PushTokens) -> Result<(), ClientError> {
        let result = self.seal_wake_push_unrecorded(tokens).await;
        self.note_push_problem(&result);
        result
    }

    async fn seal_wake_push_unrecorded(&self, tokens: &PushTokens) -> Result<(), ClientError> {
        let keys = self.relay_keys().await?;
        let now = self.engine.corrected_now();
        let seal_to = keys.seal_to(now);
        let push = wake_push(seal_to.relay, tokens, now, seal_to.until)?;
        let sealed = Sealed {
            tokens: tokens.clone(),
            key: seal_to.relay.key,
            not_after: push.alert.token.not_after,
        };
        self.engine.set_wake_push(Some(push));
        *lock(&self.learned.sealed) = Some(sealed);
        Ok(())
    }

    /// Keeps why sealing failed (or that it worked) for the snapshot.
    pub(super) fn note_push_problem<T>(&self, result: &Result<T, ClientError>) {
        match result {
            Ok(_) => *lock(&self.learned.problem) = None,
            Err(error) => {
                if let Some(problem) = downgrade_of(error) {
                    *lock(&self.learned.problem) = Some(problem);
                }
            }
        }
    }

    /// Whether the server has a push relay set up.
    pub(crate) const fn has_push_relay(&self) -> bool {
        self.learned.push_relay.is_some()
    }

    /// Whether the platform has a push token now.
    pub(crate) fn has_push_token(&self) -> bool {
        self.learned
            .platform
            .as_ref()
            .is_some_and(|platform| platform.push_tokens().is_some())
    }

    /// Why wake-up pushes could not be sealed for this server the last time they were tried.
    pub(crate) fn push_problem(&self) -> Option<PushDowngrade> {
        *lock(&self.learned.problem)
    }

    /// No seals from now on ("No wake-up pushes", or the platform lost its token).
    pub(crate) fn clear_wake_push(&self) {
        self.engine.set_wake_push(None);
        *lock(&self.learned.sealed) = None;
    }

    /// Makes sure this server's engine holds seals of the platform's current token to the key
    /// to use now, with `not_after` not yet near: seals them again when this process has none,
    /// the token or key changed, or they are about to run out. The caller has checked that
    /// wake-up pushes are on.
    ///
    /// # Errors
    ///
    /// [`Unavailable::NoPushToken`]; as [`Link::seal_wake_push`].
    pub(crate) async fn ensure_wake_push(&self) -> Result<(), ClientError> {
        let result = self.ensure_wake_push_unrecorded().await;
        self.note_push_problem(&result);
        result
    }

    async fn ensure_wake_push_unrecorded(&self) -> Result<(), ClientError> {
        let tokens = self
            .learned
            .platform
            .as_ref()
            .and_then(|platform| platform.push_tokens())
            .ok_or(Unavailable::NoPushToken)?;
        let keys = self.relay_keys().await?;
        let now = self.engine.corrected_now();
        let current = lock(&self.learned.sealed).clone();
        if current
            .is_some_and(|sealed| sealed.still_good(&tokens, &keys.seal_to(now).relay.key, now))
        {
            return Ok(());
        }
        self.seal_wake_push_unrecorded(&tokens).await
    }
}

/// The two seals of design B.4.9: knock mailboxes wake with the location class (on Android a
/// high-priority data message), everything else with a standard alert. `not_after` is 30 days
/// out, or the relay's limit, or `until` (the sealing key's retirement), whichever is first.
pub(super) fn wake_push(
    verified: &VerifiedPushRelay,
    tokens: &PushTokens,
    now: UnixSeconds,
    until: Option<UnixSeconds>,
) -> Result<WakePush, ClientError> {
    let lifetime = TimeToLive::new(TOKEN_LIFETIME)
        .map_err(|_never| corrupt())?
        .min(verified.config.max_time_to_live);
    let longest = now
        .checked_add(lifetime.duration())
        .ok_or(ClientError::Internal)?;
    let not_after = until.map_or(longest, |until| until.min(longest));
    let seal = |class, urgency| PushSeal {
        relay: RelayRef::Listed(verified.relay),
        relay_key: verified.key,
        token: PushTokenPlaintext {
            platform: tokens.platform,
            class,
            token: tokens.token.clone(),
            not_after,
        },
        urgency,
    };
    Ok(WakePush {
        knock: seal(PushClass::Location, Urgency::Alert),
        alert: seal(PushClass::Standard, Urgency::Alert),
    })
}

#[cfg(test)]
mod tests;
