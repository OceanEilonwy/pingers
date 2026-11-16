// SPDX-License-Identifier: AGPL-3.0-only
//! The relay's HPKE keys, their rotation, and the key config devices pin (design B.4.9, C.1a).
//!
//! Rotation: the operator adds a new key with a future `activated_at` and gives the old key a
//! `retired_at` at least [`KEY_OVERLAP`] later. From `activated_at` the new key is published;
//! until `retired_at` blobs sealed to the old key still open.

use std::collections::HashSet;
use std::time::Duration;

use pingers_crypto::{CryptoError, PushRelayPrivateKey, PushRelayPublicKey};
use pingers_proto::{KeyConfigHash, PushRelayKeyConfig, PushRelayKeyId};
use pingers_time::UnixSeconds;

use crate::error::ConfigError;

/// How long a replaced key keeps opening blobs: the longest a sealed token lives (30 days,
/// design B.4.9), so every blob sealed before the rotation can still be used. Devices rely on
/// it to re-seal in time, so it is the protocol's constant.
pub const KEY_OVERLAP: Duration = pingers_proto::PUSH_RELAY_KEY_OVERLAP;

/// When a key may be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyLifetime {
    /// From this time the key is published and opens blobs.
    pub activated_at: UnixSeconds,
    /// From this time the key opens nothing. `None`: not scheduled for retirement.
    pub retired_at: Option<UnixSeconds>,
}

impl KeyLifetime {
    fn opens_at(self, now: UnixSeconds) -> bool {
        self.activated_at <= now && self.retired_at.is_none_or(|retired| now < retired)
    }
}

/// One HPKE key pair of the relay.
pub struct RelayKey {
    private_key: PushRelayPrivateKey,
    public_key: PushRelayPublicKey,
    lifetime: KeyLifetime,
}

impl RelayKey {
    /// A key from configuration; the public key is derived from the private one.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Internal`] if the public key cannot be derived (it always can for 32
    /// bytes).
    pub fn new(
        key_id: PushRelayKeyId,
        private_key: PushRelayPrivateKey,
        lifetime: KeyLifetime,
    ) -> Result<Self, CryptoError> {
        let public_key = private_key.public_key(key_id.get())?;
        Ok(Self {
            private_key,
            public_key,
            lifetime,
        })
    }

    /// A key with a given public key, so tests can tell keys apart without deriving them.
    #[cfg(test)]
    pub(crate) const fn from_pair(
        private_key: PushRelayPrivateKey,
        public_key: PushRelayPublicKey,
        lifetime: KeyLifetime,
    ) -> Self {
        Self {
            private_key,
            public_key,
            lifetime,
        }
    }

    /// The key id.
    #[must_use]
    pub const fn key_id(&self) -> PushRelayKeyId {
        PushRelayKeyId::new(self.public_key.key_id())
    }

    /// When the key is first published.
    #[must_use]
    pub const fn activated_at(&self) -> UnixSeconds {
        self.lifetime.activated_at
    }

    /// The private key, for opening.
    #[must_use]
    pub const fn private_key(&self) -> &PushRelayPrivateKey {
        &self.private_key
    }

    /// The public key.
    #[must_use]
    pub const fn public_key(&self) -> &PushRelayPublicKey {
        &self.public_key
    }

    /// The key config devices fetch and pin.
    #[must_use]
    pub const fn key_config(&self) -> PushRelayKeyConfig {
        PushRelayKeyConfig {
            key_id: self.key_id(),
            public_key: *self.public_key.public_key(),
        }
    }
}

/// SHA-256 of a key config's RFC 9458 encoding: the value members pin in Party state.
#[must_use]
pub fn key_config_hash(config: &PushRelayKeyConfig) -> KeyConfigHash {
    pingers_crypto::key_config_hash(&config.to_bytes())
}

/// All configured keys, ordered by activation time.
pub struct KeyRing {
    keys: Vec<RelayKey>,
}

impl KeyRing {
    /// Checks the rotation schedule.
    ///
    /// # Errors
    ///
    /// [`ConfigError::NoKeys`], [`ConfigError::DuplicateKeyId`],
    /// [`ConfigError::DuplicateActivation`], [`ConfigError::RetiredBeforeActivated`], or
    /// [`ConfigError::OverlapTooShort`] if a key retires less than [`KEY_OVERLAP`] after the
    /// next key activates (or has no successor at all).
    pub fn new(mut keys: Vec<RelayKey>) -> Result<Self, ConfigError> {
        if keys.is_empty() {
            return Err(ConfigError::NoKeys);
        }
        let mut ids = HashSet::new();
        let mut activations = HashSet::new();
        for key in &keys {
            if !ids.insert(key.key_id()) {
                return Err(ConfigError::DuplicateKeyId {
                    key_id: key.key_id().get(),
                });
            }
            if !activations.insert(key.lifetime.activated_at) {
                return Err(ConfigError::DuplicateActivation);
            }
            if key
                .lifetime
                .retired_at
                .is_some_and(|retired| retired <= key.lifetime.activated_at)
            {
                return Err(ConfigError::RetiredBeforeActivated {
                    key_id: key.key_id().get(),
                });
            }
        }
        keys.sort_by_key(|key| key.lifetime.activated_at);
        for (position, key) in keys.iter().enumerate() {
            let Some(retired_at) = key.lifetime.retired_at else {
                continue;
            };
            let successor = position.checked_add(1).and_then(|next| keys.get(next));
            let overlap_end =
                successor.and_then(|next| next.lifetime.activated_at.checked_add(KEY_OVERLAP));
            if overlap_end.is_none_or(|end| retired_at < end) {
                return Err(ConfigError::OverlapTooShort {
                    key_id: key.key_id().get(),
                });
            }
        }
        Ok(Self { keys })
    }

    /// The key devices should seal to now: the most recently activated key that is not
    /// retired. `None` if every key is retired or none has activated yet.
    #[must_use]
    pub fn published(&self, now: UnixSeconds) -> Option<&RelayKey> {
        self.keys
            .iter()
            .rev()
            .find(|key| key.lifetime.opens_at(now))
    }

    /// The key that will be published next: the earliest whose `activated_at` is after `now`.
    /// It is announced before it activates (`GET /v1/config/next`), so pin sets and clients
    /// can take it up before devices start sealing to it (two-slot rotation). `None` if no
    /// key is scheduled.
    #[must_use]
    pub fn next(&self, now: UnixSeconds) -> Option<&RelayKey> {
        self.keys.iter().find(|key| key.lifetime.activated_at > now)
    }

    /// Every configured key, earliest activation first: what `pingers-push-relay pins`
    /// prints. In the two-slot scheme that is the served key and the next one.
    pub fn all(&self) -> impl Iterator<Item = &RelayKey> {
        self.keys.iter()
    }

    /// The key that opens blobs sealed under `key_id` now, if it is active or still inside its
    /// overlap window.
    #[must_use]
    pub fn for_opening(&self, key_id: PushRelayKeyId, now: UnixSeconds) -> Option<&RelayKey> {
        self.keys
            .iter()
            .find(|key| key.key_id() == key_id && key.lifetime.opens_at(now))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use zeroize::Zeroizing;

    use super::*;

    /// RFC 7748 §6.1, Alice's X25519 key pair: a valid pair without calling a primitive.
    pub(crate) const RFC7748_PRIVATE: [u8; 32] = [
        0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d, 0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2, 0x66,
        0x45, 0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a, 0xb1, 0x77, 0xfb, 0xa5, 0x1d, 0xb9,
        0x2c, 0x2a,
    ];
    /// RFC 7748 §6.1, Alice's public key.
    pub(crate) const RFC7748_PUBLIC: [u8; 32] = [
        0x85, 0x20, 0xf0, 0x09, 0x89, 0x30, 0xa7, 0x54, 0x74, 0x8b, 0x7d, 0xdc, 0xb4, 0x3e, 0xf7,
        0x5a, 0x0d, 0xbf, 0x3a, 0x0d, 0x26, 0x38, 0x1a, 0xf4, 0xeb, 0xa4, 0xa9, 0x8e, 0xaa, 0x9b,
        0x4e, 0x6a,
    ];

    const DAY: u64 = 24 * 60 * 60;

    pub(crate) fn key(id: u8, activated_at: u64, retired_at: Option<u64>) -> RelayKey {
        let mut public = RFC7748_PUBLIC;
        if let Some(first) = public.first_mut() {
            *first ^= id;
        }
        RelayKey::from_pair(
            PushRelayPrivateKey::from_bytes(Zeroizing::new(RFC7748_PRIVATE)),
            PushRelayPublicKey::new(id, public),
            KeyLifetime {
                activated_at: UnixSeconds::new(activated_at),
                retired_at: retired_at.map(UnixSeconds::new),
            },
        )
    }

    fn published_id(ring: &KeyRing, now: u64) -> Option<u8> {
        ring.published(UnixSeconds::new(now))
            .map(|key| key.key_id().get())
    }

    fn opens(ring: &KeyRing, id: u8, now: u64) -> bool {
        ring.for_opening(PushRelayKeyId::new(id), UnixSeconds::new(now))
            .is_some()
    }

    /// Key 1 from day 0, key 2 from day 100, key 1 retired exactly at the end of the overlap.
    fn rotated_ring() -> Result<KeyRing, ConfigError> {
        KeyRing::new(vec![key(2, 100 * DAY, None), key(1, 0, Some(130 * DAY))])
    }

    #[test]
    fn rotation_publishes_the_new_key_from_its_activation() -> Result<(), ConfigError> {
        let ring = rotated_ring()?;
        assert_eq!(published_id(&ring, 100 * DAY - 1), Some(1));
        assert_eq!(published_id(&ring, 100 * DAY), Some(2));
        Ok(())
    }

    #[test]
    fn old_key_opens_until_the_overlap_ends() -> Result<(), ConfigError> {
        let ring = rotated_ring()?;
        assert!(opens(&ring, 1, 130 * DAY - 1));
        assert!(!opens(&ring, 1, 130 * DAY));
        assert!(opens(&ring, 2, 130 * DAY));
        Ok(())
    }

    #[test]
    fn a_key_opens_nothing_before_it_activates() -> Result<(), ConfigError> {
        let ring = rotated_ring()?;
        assert!(!opens(&ring, 2, 100 * DAY - 1));
        assert!(opens(&ring, 2, 100 * DAY));
        Ok(())
    }

    #[test]
    fn unknown_key_id_opens_nothing() -> Result<(), ConfigError> {
        let ring = rotated_ring()?;
        assert!(!opens(&ring, 3, 100 * DAY));
        Ok(())
    }

    #[test]
    fn nothing_is_published_before_the_first_activation_or_after_the_last_retirement()
    -> Result<(), ConfigError> {
        let early = KeyRing::new(vec![key(1, 10, None)])?;
        assert_eq!(published_id(&early, 9), None);
        let ring = rotated_ring()?;
        assert_eq!(published_id(&ring, u64::MAX), Some(2));
        Ok(())
    }

    fn next_id(ring: &KeyRing, now: u64) -> Option<u8> {
        ring.next(UnixSeconds::new(now))
            .map(|key| key.key_id().get())
    }

    #[test]
    fn the_next_key_is_announced_until_it_activates() -> Result<(), ConfigError> {
        let ring = rotated_ring()?;
        assert_eq!(next_id(&ring, 0), Some(2));
        assert_eq!(next_id(&ring, 100 * DAY - 1), Some(2));
        assert_eq!(next_id(&ring, 100 * DAY), None);
        Ok(())
    }

    #[test]
    fn the_next_key_is_the_earliest_scheduled_one() -> Result<(), ConfigError> {
        let ring = KeyRing::new(vec![
            key(3, 200 * DAY, None),
            key(1, 0, None),
            key(2, 100 * DAY, None),
        ])?;
        assert_eq!(next_id(&ring, 0), Some(2));
        let all: Vec<u8> = ring.all().map(|key| key.key_id().get()).collect();
        assert_eq!(all, [1, 2, 3]);
        Ok(())
    }

    #[test]
    fn retiring_a_key_without_a_successor_is_refused() {
        // Devices would hold tokens sealed to key 2 with nothing to replace it.
        let ring = KeyRing::new(vec![key(1, 0, None), key(2, 10, Some(20))]);
        assert!(matches!(
            ring,
            Err(ConfigError::OverlapTooShort { key_id: 2 })
        ));
    }

    #[test]
    fn overlap_one_second_short_is_refused() {
        let ring = KeyRing::new(vec![
            key(1, 0, Some(130 * DAY - 1)),
            key(2, 100 * DAY, None),
        ]);
        assert!(matches!(
            ring,
            Err(ConfigError::OverlapTooShort { key_id: 1 })
        ));
    }

    #[test]
    fn empty_ring_is_refused() {
        assert!(matches!(KeyRing::new(Vec::new()), Err(ConfigError::NoKeys)));
    }

    #[test]
    fn duplicate_key_ids_are_refused() {
        let ring = KeyRing::new(vec![key(1, 0, None), key(1, 10, None)]);
        assert!(matches!(
            ring,
            Err(ConfigError::DuplicateKeyId { key_id: 1 })
        ));
    }

    #[test]
    fn simultaneous_activations_are_refused() {
        let ring = KeyRing::new(vec![key(1, 5, None), key(2, 5, None)]);
        assert!(matches!(ring, Err(ConfigError::DuplicateActivation)));
    }

    #[test]
    fn retirement_at_activation_is_refused() {
        let ring = KeyRing::new(vec![key(1, 5, Some(5))]);
        assert!(matches!(
            ring,
            Err(ConfigError::RetiredBeforeActivated { key_id: 1 })
        ));
    }

    #[test]
    fn the_public_key_is_derived_from_the_private_key() -> Result<(), CryptoError> {
        let key = RelayKey::new(
            PushRelayKeyId::new(7),
            PushRelayPrivateKey::from_bytes(Zeroizing::new(RFC7748_PRIVATE)),
            KeyLifetime {
                activated_at: UnixSeconds::EPOCH,
                retired_at: None,
            },
        )?;
        assert_eq!(key.key_id(), PushRelayKeyId::new(7));
        assert_eq!(
            key.key_config(),
            PushRelayKeyConfig {
                key_id: PushRelayKeyId::new(7),
                public_key: RFC7748_PUBLIC,
            }
        );
        assert_eq!(
            key_config_hash(&key.key_config()),
            pingers_crypto::key_config_hash(&key.key_config().to_bytes())
        );
        Ok(())
    }
}
