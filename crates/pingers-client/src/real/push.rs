// SPDX-License-Identifier: AGPL-3.0-only
//! What [`Net`] learns from the server, and the wake-up pushes sealed with it.
//!
//! - The server's `/v1/params`, once per process: the map host (also kept in a record, so the
//!   app's map host chain has it before the first wake) and the push relays it lists.
//! - The push relay's keys: the instance names the push relay by origin, the server lists it
//!   with a relay id, and pingers-sync fetches its published key and the key it publishes
//!   next (with its activation time) through the OHTTP relay, checking both hashes against the
//!   build's pin set. A build without pins takes the first key it sees and keeps that hash in
//!   a record, adding each announced next key: from then on they are the pins. Fetched again
//!   once a day, so a process that lives for days still learns of a rotation.
//! - pingers-sync's `WakePush`, sealed from the platform's push token for the key to use now:
//!   the next key from an hour after its activation, otherwise the published one, with
//!   `not_after` no later than the old key's retirement (activation plus the key overlap). It
//!   is sealed again when the token or the key changes, and before its `not_after` comes near,
//!   so pingers-sync's maintenance renews every registration in time (design B.4.9).

use std::sync::{Arc, Mutex, PoisonError};

use pingers_crypto::{PushRelayPublicKey, key_config_hash};
use pingers_ohttp::KeyPins;
use pingers_proto::{
    KeyConfigHash, MapHostUrl, PUSH_RELAY_KEY_OVERLAP, Params, PushClass, PushTokenPlaintext,
    RelayRef, TimeToLive, Urgency,
};
use pingers_sync::{
    NextPushRelayKey, PushSeal, RENEW_AHEAD, ScheduledPushRelay, SyncError, VerifiedPushRelay,
    WakeBudget, WakePush,
};
use pingers_time::UnixSeconds;

use super::records::{self, Key, corrupt};
use super::sync::Net;
use crate::snapshot::{Host, Operator, PushDowngrade, ServerOperators};
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

/// The instance's push relay and the pins its key must match.
#[derive(Clone, Debug)]
pub(crate) struct PushRelaySetup {
    /// The push relay's origin, as the instance names it.
    pub(crate) origin: String,
    /// The build's pin set; `None` pins on first use.
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

/// What pingers-sync's seals were made from in this process.
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

/// The first-use pins with the published and the announced next key added, or `None` when
/// they hold both already.
fn grown_pins(
    known: Option<&KeyPins>,
    active: &VerifiedPushRelay,
    next: Option<&ScheduledPushRelay>,
) -> Option<Vec<KeyConfigHash>> {
    let mut hashes: Vec<KeyConfigHash> =
        known.map(|pins| pins.hashes().to_vec()).unwrap_or_default();
    for config in std::iter::once(&active.config).chain(next.map(|next| &next.verified.config)) {
        let hash = key_config_hash(&config.key_config.to_bytes());
        if !hashes.contains(&hash) {
            hashes.push(hash);
        }
    }
    if known.is_some_and(|known| known.hashes() == hashes.as_slice()) {
        None
    } else {
        Some(hashes)
    }
}

/// What the server told this process, and the push relay set up for it.
pub(crate) struct Learned {
    push_relay: Option<PushRelaySetup>,
    /// Where the platform's push token is read when seals must be made again (a new process).
    platform: Option<Arc<dyn Platform>>,
    params: Mutex<Option<Params>>,
    keys: Mutex<Option<RelayKeys>>,
    sealed: Mutex<Option<Sealed>>,
    /// Why wake-up pushes could not be sealed the last time they were tried, for the
    /// snapshot; `None` once they were.
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

    /// How the instance's push relay key is pinned, for a setup link's checks.
    pub(super) fn push_relay_pinning(&self) -> PushRelayPinning<'_> {
        match &self.push_relay {
            None => PushRelayPinning::NoPushRelay,
            Some(PushRelaySetup {
                pins: Some(pins), ..
            }) => PushRelayPinning::Build(pins),
            Some(PushRelaySetup { pins: None, .. }) => PushRelayPinning::FirstUse,
        }
    }
}

/// How the instance's push relay key is pinned.
pub(super) enum PushRelayPinning<'pins> {
    /// The instance names no push relay.
    NoPushRelay,
    /// The build's pin set.
    Build(&'pins KeyPins),
    /// No build pins: the first key met, kept in a record, or a setup link's pins.
    FirstUse,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether two origins name the same place, ignoring a trailing slash.
fn same_origin(first: &str, second: &str) -> bool {
    first.trim_end_matches('/') == second.trim_end_matches('/')
}

/// Why sealing failed, as the snapshot names it; `None` for a failure that says nothing about
/// push delivery (the network, the store).
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other failure is the network's or the store's, not push delivery's"
)]
fn downgrade_of(error: &ClientError) -> Option<PushDowngrade> {
    match error {
        ClientError::Unavailable(Unavailable::WakePushesOff) => Some(PushDowngrade::WakePushesOff),
        ClientError::Unavailable(Unavailable::NoPushToken) => Some(PushDowngrade::NoPushToken),
        ClientError::Unavailable(Unavailable::NoPushRelay) => Some(PushDowngrade::NoPushRelay),
        ClientError::Sync(SyncError::PushRelayKeyMismatch) => Some(PushDowngrade::RelayKeyMismatch),
        _ => None,
    }
}

/// A URL's host (and port), as screens name a server: "relay.quietpath.net".
fn host_of(url: &str) -> Option<Host> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    Host::new(rest.split('/').next().unwrap_or(rest)).ok()
}

impl Net {
    /// The server's parameters, fetched through the relay once per process. The map host is
    /// kept in a record for the next start.
    ///
    /// # Errors
    ///
    /// [`Unavailable::Sync`] without an instance; [`ClientError::Sync`]; a storage failure.
    pub(crate) async fn server_params(&self) -> Result<Params, ClientError> {
        if let Some(params) = lock(&self.learned.params).clone() {
            return Ok(params);
        }
        let result = self.engine()?.params(WakeBudget::UNLIMITED).await;
        self.observe(&result);
        let params = result?;
        let map_host = params.map_host.clone();
        self.backend
            .store(move |session, _clock, _rng| match &map_host {
                Some(url) => records::write(session, &Key::ServerMapHost, |writer| {
                    writer.text(url.as_str());
                }),
                None => records::delete(session, &Key::ServerMapHost),
            })
            .await?;
        *lock(&self.learned.params) = Some(params.clone());
        Ok(params)
    }

    /// Who runs the server and the instance's push relay, and where the default privacy relay
    /// and the push relay are, from the parameters this process read; `None` before they were
    /// read or if the server declares no operator.
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

    /// The map host the server advertises: from this process's `/v1/params`, or as last
    /// recorded.
    ///
    /// # Errors
    ///
    /// A storage failure, or a corrupt record.
    pub(crate) async fn map_host(&self) -> Result<Option<MapHostUrl>, ClientError> {
        if let Some(params) = lock(&self.learned.params).as_ref() {
            return Ok(params.map_host.clone());
        }
        self.backend
            .store(|session, _clock, _rng| {
                records::read(session, &Key::ServerMapHost, |reader| {
                    MapHostUrl::new(reader.text()?).map_err(|_| corrupt())
                })
            })
            .await
    }

    /// The instance's push relay keys, as fetched within the last day: the published key,
    /// which must match the pins (or the first-use pins), and the next key if one is
    /// scheduled and pinned.
    ///
    /// # Errors
    ///
    /// [`Unavailable::NoPushRelay`] if the instance has none or the server does not list it;
    /// [`ClientError::Sync`] (`PushRelayKeyMismatch` raises `Notice::RelayKeyMismatch`); a
    /// storage failure.
    async fn relay_keys(&self) -> Result<RelayKeys, ClientError> {
        let setup = self
            .learned
            .push_relay
            .clone()
            .ok_or(Unavailable::NoPushRelay)?;
        let now = self.engine()?.corrected_now();
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
        let first_use = match &setup.pins {
            Some(_) => None,
            None => self.first_use_pins().await?,
        };
        let pins = setup.pins.clone().or(first_use.clone());
        let engine = self.engine()?;
        let result = engine
            .push_relay_key(listed.relay_id, pins.as_ref(), WakeBudget::UNLIMITED)
            .await;
        self.observe(&result);
        let active = result?;
        let next = match engine
            .push_relay_next_key(listed.relay_id, pins.as_ref(), WakeBudget::UNLIMITED)
            .await
        {
            Ok(NextPushRelayKey::Pinned(next)) => Some(next),
            Ok(NextPushRelayKey::NotScheduled | NextPushRelayKey::Unpinned { .. }) => None,
            Err(SyncError::Store(error)) => return Err(SyncError::Store(error).into()),
            // Not answered now: keep what an earlier fetch announced for this key.
            Err(_) => carried_next(lock(&self.learned.keys).as_ref(), &active),
        };
        if setup.pins.is_none() {
            self.remember_first_use(first_use.as_ref(), &active, next.as_ref())
                .await?;
        }
        let keys = RelayKeys {
            active,
            next,
            fetched_at: now,
        };
        *lock(&self.learned.keys) = Some(keys.clone());
        Ok(keys)
    }

    /// The hashes a build without pins took on first use.
    async fn first_use_pins(&self) -> Result<Option<KeyPins>, ClientError> {
        let hashes = self
            .backend
            .store(|session, _clock, _rng| {
                records::read(session, &Key::PushRelayPin, |reader| {
                    let bytes = reader.rest();
                    let mut hashes = Vec::new();
                    for chunk in bytes.chunks(32) {
                        let hash: [u8; 32] = chunk.try_into().map_err(|_| corrupt())?;
                        hashes.push(KeyConfigHash::from_bytes(hash));
                    }
                    Ok(hashes)
                })
            })
            .await?;
        hashes
            .map(|hashes| KeyPins::new(hashes).map_err(|_| corrupt()))
            .transpose()
    }

    /// Takes a setup link's push relay pins as the first-use pins, unless pins were learned
    /// already: the relay's key is then checked from the first fetch instead of trusted on
    /// first use. Only for a build without push relay pins.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub(super) async fn seed_first_use_pins(
        &self,
        pins: Vec<KeyConfigHash>,
    ) -> Result<(), ClientError> {
        if !matches!(
            self.learned.push_relay_pinning(),
            PushRelayPinning::FirstUse
        ) || pins.is_empty()
        {
            return Ok(());
        }
        self.backend
            .store(move |session, _clock, _rng| {
                session.transaction(|session| {
                    if records::read(session, &Key::PushRelayPin, |reader| {
                        drop(reader.rest());
                        Ok(())
                    })?
                    .is_some()
                    {
                        return Ok(());
                    }
                    records::write(session, &Key::PushRelayPin, |writer| {
                        for hash in &pins {
                            writer.fixed(hash.as_bytes());
                        }
                    })
                })
            })
            .await
    }

    /// Adds the published key and the announced next key to the first-use pins, so a build
    /// without pins follows the relay's rotation from the key it first trusted.
    async fn remember_first_use(
        &self,
        known: Option<&KeyPins>,
        active: &VerifiedPushRelay,
        next: Option<&ScheduledPushRelay>,
    ) -> Result<(), ClientError> {
        let Some(hashes) = grown_pins(known, active, next) else {
            return Ok(());
        };
        self.backend
            .store(move |session, _clock, _rng| {
                records::write(session, &Key::PushRelayPin, |writer| {
                    for hash in &hashes {
                        writer.fixed(hash.as_bytes());
                    }
                })
            })
            .await
    }

    /// Seals `tokens` to the key to use now and hands the seals to pingers-sync: new
    /// registrations carry them, and maintenance seals the older ones again.
    ///
    /// # Errors
    ///
    /// As [`Net::relay_keys`].
    pub(crate) async fn seal_wake_push(&self, tokens: &PushTokens) -> Result<(), ClientError> {
        let result = self.seal_wake_push_unrecorded(tokens).await;
        self.note_problem(&result);
        result
    }

    async fn seal_wake_push_unrecorded(&self, tokens: &PushTokens) -> Result<(), ClientError> {
        let keys = self.relay_keys().await?;
        let now = self.engine()?.corrected_now();
        let seal_to = keys.seal_to(now);
        let push = wake_push(seal_to.relay, tokens, now, seal_to.until)?;
        let sealed = Sealed {
            tokens: tokens.clone(),
            key: seal_to.relay.key,
            not_after: push.alert.token.not_after,
        };
        self.engine()?.set_wake_push(Some(push));
        *lock(&self.learned.sealed) = Some(sealed);
        Ok(())
    }

    fn note_problem<T>(&self, result: &Result<T, ClientError>) {
        match result {
            Ok(_) => *lock(&self.learned.problem) = None,
            Err(error) => {
                if let Some(problem) = downgrade_of(error) {
                    *lock(&self.learned.problem) = Some(problem);
                }
            }
        }
    }

    /// Whether the build names a push relay.
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

    /// Why wake-up pushes could not be sealed the last time they were tried.
    pub(crate) fn push_problem(&self) -> Option<PushDowngrade> {
        *lock(&self.learned.problem)
    }

    /// No seals from now on ("No wake-up pushes").
    pub(crate) fn clear_wake_push(&self) {
        if let Some(engine) = &self.engine {
            engine.set_wake_push(None);
        }
        *lock(&self.learned.sealed) = None;
    }

    /// Makes sure pingers-sync holds seals of the platform's current token to the key to use
    /// now, with `not_after` not yet near: seals them again when this process has none, the
    /// token or key changed, or they are about to run out.
    ///
    /// # Errors
    ///
    /// [`Unavailable::WakePushesOff`] with "No wake-up pushes" set;
    /// [`Unavailable::NoPushToken`]; as [`Net::seal_wake_push`].
    pub(crate) async fn ensure_wake_push(&self) -> Result<(), ClientError> {
        let result = self.ensure_wake_push_unrecorded().await;
        self.note_problem(&result);
        result
    }

    async fn ensure_wake_push_unrecorded(&self) -> Result<(), ClientError> {
        let setting = self
            .backend
            .store(|session, _clock, _rng| Ok(session.settings().wake_pushes()?))
            .await?;
        if setting == pingers_store::WakePushes::Disabled {
            return Err(Unavailable::WakePushesOff.into());
        }
        let tokens = self
            .learned
            .platform
            .as_ref()
            .and_then(|platform| platform.push_tokens())
            .ok_or(Unavailable::NoPushToken)?;
        let keys = self.relay_keys().await?;
        let now = self.engine()?.corrected_now();
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
        .map_err(|_| corrupt())?
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
