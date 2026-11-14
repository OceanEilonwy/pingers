// SPDX-License-Identifier: AGPL-3.0-only
//! What [`Net`] learns from the server, and the wake-up pushes sealed with it.
//!
//! - The server's `/v1/params`, once per process: the map host (also kept in a record, so the
//!   app's map host chain has it before the first wake) and the push relays it lists.
//! - The push relay's key: the instance names the push relay by origin, the server lists it
//!   with a relay id, and pingers-sync fetches its key config through the OHTTP relay and checks
//!   the hash against the build's pin set. A build without pins takes the first key it sees
//!   and keeps that hash in a record: from then on it is the pin.
//! - pingers-sync's `WakePush`, sealed from the platform's push token for that key.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pingers_crypto::key_config_hash;
use pingers_ohttp::KeyPins;
use pingers_proto::{
    KeyConfigHash, MapHostUrl, Params, PushClass, PushTokenPlaintext, RelayRef, TimeToLive, Urgency,
};
use pingers_sync::{PushSeal, VerifiedPushRelay, WakeBudget, WakePush};

use super::records::{self, Key, corrupt};
use super::sync::Net;
use crate::snapshot::{Operator, ServerOperators};
use crate::{ClientError, Platform, PushTokens, Unavailable};

/// The longest a sealed token is good for: a Party subscription lives 30 days (design B.4.9),
/// unless the relay allows less.
pub(super) const TOKEN_LIFETIME: core::time::Duration = core::time::Duration::from_hours(30 * 24);

/// The instance's push relay and the pins its key must match.
#[derive(Clone, Debug)]
pub(crate) struct PushRelaySetup {
    /// The push relay's origin, as the instance names it.
    pub(crate) origin: String,
    /// The build's pin set; `None` pins on first use.
    pub(crate) pins: Option<KeyPins>,
}

/// What the server told this process, and the push relay set up for it.
pub(crate) struct Learned {
    push_relay: Option<PushRelaySetup>,
    /// Where the platform's push token is read when seals must be made again (a new process).
    platform: Option<Arc<dyn Platform>>,
    params: Mutex<Option<Params>>,
    verified: Mutex<Option<VerifiedPushRelay>>,
    /// Whether pingers-sync holds seals made in this process.
    sealed: AtomicBool,
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
            verified: Mutex::new(None),
            sealed: AtomicBool::new(false),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether two origins name the same place, ignoring a trailing slash.
fn same_origin(first: &str, second: &str) -> bool {
    first.trim_end_matches('/') == second.trim_end_matches('/')
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

    /// The map host the server advertises: from this process's `/v1/params`, or as last
    /// recorded.
    ///
    /// # Errors
    ///
    /// A storage failure, or a corrupt record.
    /// Who runs the server and the instance's push relay, from the parameters this process
    /// read; `None` before they were read or if the server declares no operator.
    pub(crate) fn operators(&self) -> Option<ServerOperators> {
        let params = lock(&self.learned.params);
        let params = params.as_ref()?;
        let push_relay = self.learned.push_relay.as_ref().and_then(|setup| {
            params
                .push_relays
                .iter()
                .find(|relay| same_origin(relay.url.as_str(), &setup.origin))
                .and_then(|relay| Operator::new(&relay.operator).ok())
        });
        Some(ServerOperators {
            server: Operator::new(&params.server_operator).ok()?,
            push_relay,
        })
    }

    pub(crate) async fn map_host(&self) -> Result<Option<MapHostUrl>, ClientError> {
        if let Some(params) = lock(&self.learned.params).as_ref() {
            return Ok(params.map_host.clone());
        }
        self.backend
            .store(|session, _clock, _rng| {
                records::read(session, &Key::ServerMapHost, |reader| {
                    MapHostUrl::new(reader.text()?).map_err(|_invalid| corrupt())
                })
            })
            .await
    }

    /// The instance's push relay with a key that matched its pins (or the first-use pin),
    /// fetched once per process.
    ///
    /// # Errors
    ///
    /// [`Unavailable::NoPushRelay`] if the instance has none or the server does not list it;
    /// [`ClientError::Sync`] (`PushRelayKeyMismatch` raises `Notice::RelayKeyMismatch`); a
    /// storage failure.
    pub(crate) async fn push_relay(&self) -> Result<VerifiedPushRelay, ClientError> {
        if let Some(verified) = lock(&self.learned.verified).clone() {
            return Ok(verified);
        }
        let setup = self
            .learned
            .push_relay
            .clone()
            .ok_or(Unavailable::NoPushRelay)?;
        let params = self.server_params().await?;
        let listed = params
            .push_relays
            .iter()
            .find(|relay| same_origin(relay.url.as_str(), &setup.origin))
            .ok_or(Unavailable::NoPushRelay)?;
        let first_use = match &setup.pins {
            Some(_) => None,
            None => self.first_use_pin().await?,
        };
        let pins = setup.pins.clone().or(first_use.clone());
        let result = self
            .engine()?
            .push_relay_key(listed.relay_id, pins.as_ref(), WakeBudget::UNLIMITED)
            .await;
        self.observe(&result);
        let verified = result?;
        if setup.pins.is_none() && first_use.is_none() {
            let hash = key_config_hash(&verified.config.key_config.to_bytes());
            self.backend
                .store(move |session, _clock, _rng| {
                    records::write(session, &Key::PushRelayPin, |writer| {
                        writer.fixed(hash.as_bytes());
                    })
                })
                .await?;
        }
        *lock(&self.learned.verified) = Some(verified.clone());
        Ok(verified)
    }

    async fn first_use_pin(&self) -> Result<Option<KeyPins>, ClientError> {
        self.backend
            .store(|session, _clock, _rng| {
                records::read(session, &Key::PushRelayPin, |reader| {
                    let bytes: [u8; 32] = reader.fixed()?;
                    Ok(KeyPins::from(KeyConfigHash::from_bytes(bytes)))
                })
            })
            .await
    }

    /// Seals `tokens` to the push relay's key and hands the seals to pingers-sync: new
    /// mailboxes and subscriptions carry them from now on.
    ///
    /// # Errors
    ///
    /// As [`Net::push_relay`].
    pub(crate) async fn seal_wake_push(&self, tokens: &PushTokens) -> Result<(), ClientError> {
        let verified = self.push_relay().await?;
        let push = wake_push(&verified, tokens, self.engine()?.corrected_now())?;
        self.engine()?.set_wake_push(Some(push));
        self.learned.sealed.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// No seals from now on ("No wake-up pushes", or the platform lost its token).
    pub(crate) fn clear_wake_push(&self) {
        if let Some(engine) = &self.engine {
            engine.set_wake_push(None);
        }
        self.learned.sealed.store(false, Ordering::Relaxed);
    }

    /// Makes sure pingers-sync holds seals, making them from the platform's token if this
    /// process has not yet (a join request must carry a sealed subscription).
    ///
    /// # Errors
    ///
    /// [`Unavailable::WakePushesOff`] with "No wake-up pushes" set;
    /// [`Unavailable::NoPushToken`]; as [`Net::seal_wake_push`].
    pub(crate) async fn ensure_wake_push(&self) -> Result<(), ClientError> {
        if self.learned.sealed.load(Ordering::Relaxed) {
            return Ok(());
        }
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
        self.seal_wake_push(&tokens).await
    }
}

/// The two seals of design B.4.9: knock mailboxes wake with the location class (on Android a
/// high-priority data message), everything else with a standard alert.
fn wake_push(
    verified: &VerifiedPushRelay,
    tokens: &PushTokens,
    now: pingers_time::UnixSeconds,
) -> Result<WakePush, ClientError> {
    let lifetime = TimeToLive::new(TOKEN_LIFETIME)
        .map_err(|_never| corrupt())?
        .min(verified.config.max_time_to_live);
    let not_after = now
        .checked_add(lifetime.duration())
        .ok_or(ClientError::Internal)?;
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
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use core::time::Duration;

    use pingers_crypto::PushRelayPublicKey;
    use pingers_proto::{
        PushPlatform, PushRelayConfig, PushRelayKeyConfig, PushRelayKeyId, PushToken, RelayId,
    };
    use pingers_time::UnixSeconds;

    use super::*;

    type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

    fn relay(max_days: u64) -> TestResult<VerifiedPushRelay> {
        Ok(VerifiedPushRelay {
            relay: RelayId::new(7),
            key: PushRelayPublicKey::new(3, [9; 32]),
            config: PushRelayConfig {
                key_config: PushRelayKeyConfig {
                    key_id: PushRelayKeyId::new(3),
                    public_key: [9; 32],
                },
                platforms: vec![PushPlatform::UnifiedPush],
                max_time_to_live: TimeToLive::new(Duration::from_hours(
                    max_days.saturating_mul(24),
                ))?,
                operator: "relay.invalid".to_owned(),
            },
        })
    }

    #[test]
    fn tokens_are_sealed_for_30_days_or_the_relay_s_limit() -> TestResult<()> {
        let tokens = PushTokens {
            platform: PushPlatform::UnifiedPush,
            token: PushToken::from_bytes(b"endpoint".to_vec()),
        };
        let now = UnixSeconds::new(1_000_000);
        let long = wake_push(&relay(60)?, &tokens, now)?;
        assert_eq!(long.alert.token.not_after.get(), 1_000_000 + 30 * 86_400);
        assert_eq!(long.knock.token.class, PushClass::Location);
        assert_eq!(long.alert.token.class, PushClass::Standard);
        assert_eq!(long.alert.relay, RelayRef::Listed(RelayId::new(7)));
        assert_eq!(long.alert.relay_key.key_id(), 3);
        let short = wake_push(&relay(2)?, &tokens, now)?;
        assert_eq!(short.knock.token.not_after.get(), 1_000_000 + 2 * 86_400);
        Ok(())
    }

    #[test]
    fn origins_match_with_or_without_a_trailing_slash() {
        assert!(same_origin("https://push.invalid/", "https://push.invalid"));
        assert!(same_origin("https://push.invalid", "https://push.invalid"));
        assert!(!same_origin(
            "https://push.invalid",
            "https://other.invalid"
        ));
    }
}
