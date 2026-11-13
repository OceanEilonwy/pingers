// SPDX-License-Identifier: AGPL-3.0-only
//! Relays: which one to use, which are resting after failures, and the pinned key config.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use pingers_ohttp::{KeyConfig, KeyPins, OhttpError, check_pins};
use pingers_proto::KeyConfigHash;
use pingers_time::MonotonicTime;

use crate::TransportError;
use crate::endpoint::Endpoint;

/// One OHTTP relay: where encapsulated requests are posted, and where the gateway's key
/// config is fetched through it (design B.12: key configs come through the relay, never
/// direct).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relay {
    request: Endpoint,
    key_config: Endpoint,
}

impl Relay {
    /// A relay from its two URLs, for example `https://relay.example/ohttp` and
    /// `https://relay.example/v1/ohttp/config`. `http://` means HTTP/2 without TLS, for a
    /// relay on loopback or behind a private tunnel only.
    ///
    /// # Errors
    ///
    /// [`TransportError::InvalidUrl`] if either is not an absolute `https://` or `http://`
    /// URL.
    pub fn new(request_url: &str, key_config_url: &str) -> Result<Self, TransportError> {
        Ok(Self {
            request: Endpoint::parse(request_url)?,
            key_config: Endpoint::parse(key_config_url)?,
        })
    }

    pub(crate) fn request_endpoint(&self) -> &Endpoint {
        &self.request
    }

    pub(crate) fn key_config_endpoint(&self) -> &Endpoint {
        &self.key_config
    }
}

/// How long a relay rests after consecutive failures, and how long the live channel waits
/// between reconnects: `first`, doubling, at most `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// The wait after the first failure.
    pub first: Duration,
    /// The longest wait.
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(1),
            max: Duration::from_secs(60),
        }
    }
}

impl Backoff {
    /// The wait after `failures` consecutive failures (zero for none).
    #[must_use]
    pub fn delay(self, failures: u32) -> Duration {
        let Some(doublings) = failures.checked_sub(1) else {
            return Duration::ZERO;
        };
        let factor = 2_u32.checked_pow(doublings).unwrap_or(u32::MAX);
        self.first.saturating_mul(factor).min(self.max)
    }
}

/// Whether a relay forwards both bodies of a chunked exchange as they stream (design B.4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Duplex {
    /// Not tried yet: try full duplex first.
    Unknown,
    /// It streamed both directions.
    Full,
    /// It held the request body back: use the split channel.
    ResponseOnly,
}

#[derive(Clone, Copy, Debug)]
struct Health {
    failures: u32,
    rest_until: Option<MonotonicTime>,
    duplex: Duplex,
}

/// The configured relays, in order of preference, and their health.
pub(crate) struct RelayPool {
    relays: Vec<Relay>,
    health: Mutex<Vec<Health>>,
    backoff: Backoff,
}

impl RelayPool {
    /// A pool of at least one relay: a server must advertise one (design B.12).
    pub(crate) fn new(relays: Vec<Relay>, backoff: Backoff) -> Result<Self, TransportError> {
        if relays.is_empty() {
            return Err(TransportError::NoRelay);
        }
        let health = relays
            .iter()
            .map(|_| Health {
                failures: 0,
                rest_until: None,
                duplex: Duplex::Unknown,
            })
            .collect();
        Ok(Self {
            relays,
            health: Mutex::new(health),
            backoff,
        })
    }

    fn health(&self) -> MutexGuard<'_, Vec<Health>> {
        // The data is a few counters, consistent after any single write.
        self.health.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The relays not resting at `now`, most preferred first.
    pub(crate) fn available(&self, now: MonotonicTime) -> Vec<usize> {
        self.health()
            .iter()
            .enumerate()
            .filter(|(_, health)| health.rest_until.is_none_or(|until| now >= until))
            .map(|(index, _)| index)
            .collect()
    }

    pub(crate) fn relay(&self, index: usize) -> Option<&Relay> {
        self.relays.get(index)
    }

    /// The relay failed at `now`: it rests for the backoff of its consecutive failures.
    pub(crate) fn record_failure(&self, index: usize, now: MonotonicTime) {
        if let Some(health) = self.health().get_mut(index) {
            health.failures = health.failures.saturating_add(1);
            health.rest_until = Some(now.saturating_add(self.backoff.delay(health.failures)));
        }
    }

    /// The relay answered: it is healthy again.
    pub(crate) fn record_success(&self, index: usize) {
        if let Some(health) = self.health().get_mut(index) {
            health.failures = 0;
            health.rest_until = None;
        }
    }

    pub(crate) fn duplex(&self, index: usize) -> Duplex {
        self.health()
            .get(index)
            .map_or(Duplex::Unknown, |health| health.duplex)
    }

    pub(crate) fn set_duplex(&self, index: usize, duplex: Duplex) {
        if let Some(health) = self.health().get_mut(index) {
            health.duplex = duplex;
        }
    }
}

/// What a fetched key config is checked against (design B.12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyPin {
    /// The hashes the config may have: those shipped with an official build (the key served now
    /// and the next one, for two-slot rotation), or the one pinned in a Party's encrypted state.
    /// First-use pinning is off.
    Pinned(KeyPins),
    /// No pin yet: the first config fetched becomes the pin; read it back with
    /// [`OhttpTransport::key_config_hash`](crate::OhttpTransport::key_config_hash) and store it.
    FirstUse,
}

#[derive(Clone, Debug)]
enum KeyState {
    /// Nothing fetched yet (or re-pinned since).
    Unverified,
    /// Fetched and matching the pin.
    Verified(KeyConfig),
    /// A fetched config did not match: anonymous traffic stops until re-pinned.
    Mismatch,
}

/// The pin and what was last fetched.
#[derive(Debug)]
pub(crate) struct KeyGuard {
    pin: KeyPin,
    state: KeyState,
}

impl KeyGuard {
    pub(crate) fn new(pin: KeyPin) -> Self {
        Self {
            pin,
            state: KeyState::Unverified,
        }
    }

    /// The verified config, `None` if it must be fetched, or the latched mismatch.
    pub(crate) fn current(&self) -> Result<Option<KeyConfig>, TransportError> {
        match &self.state {
            KeyState::Unverified => Ok(None),
            KeyState::Verified(config) => Ok(Some(config.clone())),
            KeyState::Mismatch => Err(OhttpError::KeyMismatch.into()),
        }
    }

    /// Checks a freshly fetched config against the pin. A mismatch is latched: every later
    /// request fails closed without touching the network until [`KeyGuard::repin`].
    pub(crate) fn accept(&mut self, fetched: KeyConfig) -> Result<KeyConfig, TransportError> {
        if let KeyState::Mismatch = self.state {
            return Err(OhttpError::KeyMismatch.into());
        }
        match &self.pin {
            KeyPin::FirstUse => self.pin = KeyPin::Pinned(fetched.hash().into()),
            KeyPin::Pinned(pins) => {
                if let Err(error) = check_pins(&fetched, pins) {
                    self.state = KeyState::Mismatch;
                    return Err(error.into());
                }
            }
        }
        self.state = KeyState::Verified(fetched.clone());
        Ok(fetched)
    }

    /// Replaces the pin (after the user resolved a `RelayKeyMismatch`); the config is fetched
    /// again on the next request.
    pub(crate) fn repin(&mut self, pin: KeyPin) {
        self.pin = pin;
        self.state = KeyState::Unverified;
    }

    /// The hash of the config fetched and found to match the pin, once there is one.
    pub(crate) fn verified_hash(&self) -> Option<KeyConfigHash> {
        match &self.state {
            KeyState::Verified(config) => Some(config.hash()),
            KeyState::Unverified | KeyState::Mismatch => None,
        }
    }

    /// Forgets the verified config so the next request fetches it again (a reachability
    /// check). A latched mismatch stays latched.
    pub(crate) fn refetch(&mut self) {
        if let KeyState::Verified(_) = self.state {
            self.state = KeyState::Unverified;
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_ohttp::GatewayKey;
    use pingers_testkit::seeded_rng;

    use super::*;

    fn at(seconds: u64) -> MonotonicTime {
        MonotonicTime::from_elapsed(Duration::from_secs(seconds))
    }

    fn relay(name: &str) -> Result<Relay, TransportError> {
        Relay::new(
            &format!("https://{name}/ohttp"),
            &format!("https://{name}/v1/ohttp/config"),
        )
    }

    fn pool(count: usize) -> Result<RelayPool, TransportError> {
        let relays = (0..count)
            .map(|index| relay(&format!("relay{index}.example")))
            .collect::<Result<Vec<_>, _>>()?;
        RelayPool::new(relays, Backoff::default())
    }

    #[test]
    fn backoff_doubles_from_first_up_to_max() {
        let backoff = Backoff {
            first: Duration::from_secs(2),
            max: Duration::from_secs(20),
        };
        let delays: Vec<u64> = (0..7).map(|n| backoff.delay(n).as_secs()).collect();
        assert_eq!(delays, [0, 2, 4, 8, 16, 20, 20]);
        assert_eq!(backoff.delay(u32::MAX), Duration::from_secs(20));
    }

    #[test]
    fn a_pool_needs_a_relay() {
        assert!(matches!(
            RelayPool::new(Vec::new(), Backoff::default()),
            Err(TransportError::NoRelay)
        ));
    }

    #[test]
    fn relay_urls_are_checked() {
        assert!(matches!(
            Relay::new("https://relay/ohttp", "ftp://relay/config"),
            Err(TransportError::InvalidUrl)
        ));
        assert!(matches!(
            Relay::new("relay/ohttp", "https://relay/config"),
            Err(TransportError::InvalidUrl)
        ));
    }

    #[test]
    fn relays_are_offered_in_order_of_preference() -> Result<(), TransportError> {
        let pool = pool(3)?;
        assert_eq!(pool.available(at(0)), [0, 1, 2]);
        let second = pool.relay(1).ok_or(TransportError::NoRelay)?;
        assert_eq!(
            second.request_endpoint().uri()?.to_string(),
            "https://relay1.example/ohttp"
        );
        assert_eq!(
            second.key_config_endpoint().uri()?.to_string(),
            "https://relay1.example/v1/ohttp/config"
        );
        assert!(pool.relay(3).is_none());
        Ok(())
    }

    #[test]
    fn a_failed_relay_rests_for_its_backoff_then_returns() -> Result<(), TransportError> {
        let pool = pool(2)?;
        pool.record_failure(0, at(100));
        assert_eq!(pool.available(at(100)), [1]);
        assert_eq!(
            pool.available(at(100).saturating_add(Duration::from_millis(999))),
            [1]
        );
        assert_eq!(pool.available(at(101)), [0, 1]);

        // A second failure in a row rests twice as long.
        pool.record_failure(0, at(101));
        assert_eq!(pool.available(at(102)), [1]);
        assert_eq!(pool.available(at(103)), [0, 1]);
        Ok(())
    }

    #[test]
    fn success_clears_the_failure_count() -> Result<(), TransportError> {
        let pool = pool(1)?;
        pool.record_failure(0, at(0));
        pool.record_failure(0, at(0));
        pool.record_success(0);
        assert_eq!(pool.available(at(0)), [0]);
        // The next failure starts the backoff from the beginning again.
        pool.record_failure(0, at(10));
        assert_eq!(pool.available(at(11)), [0]);
        Ok(())
    }

    #[test]
    fn duplex_support_is_remembered_per_relay() -> Result<(), TransportError> {
        let pool = pool(2)?;
        assert_eq!(pool.duplex(0), Duplex::Unknown);
        pool.set_duplex(1, Duplex::ResponseOnly);
        assert_eq!(pool.duplex(0), Duplex::Unknown);
        assert_eq!(pool.duplex(1), Duplex::ResponseOnly);
        assert_eq!(pool.duplex(9), Duplex::Unknown);
        Ok(())
    }

    fn config(seed: u64) -> Result<KeyConfig, TransportError> {
        Ok(GatewayKey::generate(1, &mut seeded_rng(seed))?.config())
    }

    #[test]
    fn a_matching_config_is_verified_and_kept() -> Result<(), TransportError> {
        let fetched = config(1)?;
        let mut guard = KeyGuard::new(KeyPin::Pinned(fetched.hash().into()));
        assert!(guard.current()?.is_none());
        assert_eq!(guard.accept(fetched.clone())?, fetched);
        assert_eq!(guard.current()?, Some(fetched.clone()));
        assert_eq!(guard.verified_hash(), Some(fetched.hash()));
        Ok(())
    }

    #[test]
    fn a_mismatch_is_latched_until_repinned() -> Result<(), TransportError> {
        let pinned = config(1)?;
        let other = config(2)?;
        let mut guard = KeyGuard::new(KeyPin::Pinned(pinned.hash().into()));
        assert!(matches!(
            guard.accept(other.clone()),
            Err(TransportError::Ohttp(OhttpError::KeyMismatch))
        ));
        assert!(matches!(
            guard.current(),
            Err(TransportError::Ohttp(OhttpError::KeyMismatch))
        ));
        // Even the right config is refused while the mismatch stands.
        assert!(guard.accept(pinned.clone()).is_err());

        guard.repin(KeyPin::Pinned(other.hash().into()));
        assert!(guard.current()?.is_none());
        assert_eq!(guard.accept(other.clone())?, other);
        Ok(())
    }

    #[test]
    fn first_use_pins_the_first_config() -> Result<(), TransportError> {
        let first = config(1)?;
        let mut guard = KeyGuard::new(KeyPin::FirstUse);
        assert_eq!(guard.verified_hash(), None);
        guard.accept(first.clone())?;
        assert_eq!(guard.verified_hash(), Some(first.hash()));

        // From then on the first config is the pin: a different one is a mismatch.
        assert!(guard.accept(config(2)?).is_err());
        Ok(())
    }

    #[test]
    fn a_pin_set_accepts_the_next_key_after_rotation() -> Result<(), TransportError> {
        let current = config(1)?;
        let next = config(2)?;
        let pins = KeyPins::new([current.hash(), next.hash()])?;
        let mut guard = KeyGuard::new(KeyPin::Pinned(pins));
        guard.accept(current)?;
        // The operator starts serving the second slot: no mismatch, no first-use repin.
        guard.refetch();
        assert!(guard.current()?.is_none());
        assert_eq!(guard.accept(next.clone())?, next);
        assert_eq!(guard.verified_hash(), Some(next.hash()));
        // A third key is refused even though two are pinned.
        guard.refetch();
        assert!(guard.accept(config(3)?).is_err());
        assert_eq!(guard.verified_hash(), None);
        Ok(())
    }

    #[test]
    fn a_refetch_keeps_a_latched_mismatch() -> Result<(), TransportError> {
        let pinned = config(1)?;
        let mut guard = KeyGuard::new(KeyPin::Pinned(pinned.hash().into()));
        assert!(guard.accept(config(2)?).is_err());
        guard.refetch();
        assert!(guard.current().is_err());
        assert!(guard.accept(pinned).is_err());
        Ok(())
    }
}
