// SPDX-License-Identifier: AGPL-3.0-only
//! What a [`Link`] learns from its server, and the wake-up pushes sealed with it.
//!
//! - The server's `/v1/params`, once per process: the map host and the push relays it lists,
//!   with who runs them.
//! - The push relay's key: a server's push relay is named by origin, the server lists it with a
//!   relay id, and pingers-sync fetches its key config through that server's OHTTP relay and
//!   checks the hash against the relay's pins (the build's for the build's own push relay,
//!   otherwise those recorded with the server). Nothing is pinned on first use: a push relay
//!   without pins gets no push token.
//! - pingers-sync's `WakePush`, sealed from the platform's push token for that key. Each
//!   server's engine holds its own seals: its mailboxes and subscriptions name its push relay.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pingers_ohttp::KeyPins;
use pingers_proto::{Params, PushClass, PushTokenPlaintext, RelayRef, TimeToLive, Urgency};
use pingers_sync::{PushSeal, VerifiedPushRelay, WakeBudget, WakePush};

use super::records::corrupt;
use super::servers::Link;
use crate::snapshot::{Operator, ServerOperators};
use crate::{ClientError, Platform, PushTokens, Unavailable};

/// The longest a sealed token is good for: a Party subscription lives 30 days (design B.4.9),
/// unless the relay allows less.
pub(super) const TOKEN_LIFETIME: core::time::Duration = core::time::Duration::from_hours(30 * 24);

/// A server's push relay and the pins its key must match.
#[derive(Clone, Debug)]
pub(crate) struct PushRelaySetup {
    /// The push relay's origin.
    pub(crate) origin: String,
    /// The pins; `None` means none are known, and no token is sealed to it.
    pub(crate) pins: Option<KeyPins>,
}

/// What a server told this process, and the push relay set up for it.
pub(crate) struct Learned {
    push_relay: Option<PushRelaySetup>,
    /// Where the platform's push token is read when seals must be made again (a new process).
    platform: Option<Arc<dyn Platform>>,
    params: Mutex<Option<Params>>,
    verified: Mutex<Option<VerifiedPushRelay>>,
    /// Whether this server's engine holds seals made in this process.
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

    /// Starts with parameters read while the server was looked at.
    pub(crate) fn learn_params(&self, params: Params) {
        *lock(&self.learned.params) = Some(params);
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

    /// Who runs the server and its push relay, from the parameters this process read; `None`
    /// before they were read or if the server declares no operator.
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

    /// The server's push relay with a key that matched its pins, fetched once per process.
    ///
    /// # Errors
    ///
    /// [`Unavailable::NoPushRelay`] if the server has none, does not list it, or no pins are
    /// known for it; [`ClientError::Sync`] (`PushRelayKeyMismatch` raises
    /// `Notice::RelayKeyMismatch`).
    pub(crate) async fn push_relay(&self) -> Result<VerifiedPushRelay, ClientError> {
        if let Some(verified) = lock(&self.learned.verified).clone() {
            return Ok(verified);
        }
        let setup = self
            .learned
            .push_relay
            .clone()
            .ok_or(Unavailable::NoPushRelay)?;
        let pins = setup.pins.ok_or(Unavailable::NoPushRelay)?;
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
        let verified = result?;
        *lock(&self.learned.verified) = Some(verified.clone());
        Ok(verified)
    }

    /// Seals `tokens` to the push relay's key and hands the seals to this server's engine: new
    /// mailboxes and subscriptions carry them from now on.
    ///
    /// # Errors
    ///
    /// As [`Link::push_relay`].
    pub(crate) async fn seal_wake_push(&self, tokens: &PushTokens) -> Result<(), ClientError> {
        let verified = self.push_relay().await?;
        let push = wake_push(&verified, tokens, self.engine.corrected_now())?;
        self.engine.set_wake_push(Some(push));
        self.learned.sealed.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// No seals from now on ("No wake-up pushes", or the platform lost its token).
    pub(crate) fn clear_wake_push(&self) {
        self.engine.set_wake_push(None);
        self.learned.sealed.store(false, Ordering::Relaxed);
    }

    /// Makes sure this server's engine holds seals, making them from the platform's token if
    /// this process has not yet. The caller has checked that wake-up pushes are on.
    ///
    /// # Errors
    ///
    /// [`Unavailable::NoPushToken`]; as [`Link::seal_wake_push`].
    pub(crate) async fn ensure_wake_push(&self) -> Result<(), ClientError> {
        if self.learned.sealed.load(Ordering::Relaxed) {
            return Ok(());
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
