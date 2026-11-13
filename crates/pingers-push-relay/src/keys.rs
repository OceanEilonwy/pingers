// SPDX-License-Identifier: AGPL-3.0-only
//! The relay's HPKE keys, their rotation, and the key config devices pin (design B.4.9, C.1a).
//!
//! Rotation: the operator adds a new key with a future `activated_at` and gives the old key a
//! `retired_at` at least [`KEY_OVERLAP`] later. From `activated_at` the new key is published;
//! until `retired_at` blobs sealed to the old key still open.

use std::collections::HashSet;
use std::time::Duration;

use pingers_crypto::{PushRelayPrivateKey, PushRelayPublicKey};
use pingers_proto::KeyConfigHash;
use pingers_time::UnixSeconds;

use crate::error::ConfigError;

/// How long a replaced key keeps opening blobs: the longest a sealed token lives (30 days,
/// design B.4.9), so every blob sealed before the rotation can still be used.
pub const KEY_OVERLAP: Duration = Duration::from_hours(30 * 24);

/// The one-byte key id a push request names, as in an OHTTP key config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyId(u8);

impl KeyId {
    /// Wraps a key id from configuration or a request.
    #[must_use]
    pub const fn new(id: u8) -> Self {
        Self(id)
    }

    /// The id byte.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

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
    /// A key pair from configuration. The key id is taken from `public_key`.
    ///
    /// pingers-crypto cannot derive the public key from the private one yet, so both are
    /// configured; a pair that does not match makes every push answer 422.
    #[must_use]
    pub const fn new(
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
    pub const fn key_id(&self) -> KeyId {
        KeyId(self.public_key.key_id())
    }

    /// The private key, for opening.
    #[must_use]
    pub const fn private_key(&self) -> &PushRelayPrivateKey {
        &self.private_key
    }

    /// The public key, for the key config.
    #[must_use]
    pub const fn public_key(&self) -> &PushRelayPublicKey {
        &self.public_key
    }
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

    /// The key that opens blobs sealed under `key_id` now, if it is active or still inside its
    /// overlap window.
    #[must_use]
    pub fn for_opening(&self, key_id: KeyId, now: UnixSeconds) -> Option<&RelayKey> {
        self.keys
            .iter()
            .find(|key| key.key_id() == key_id && key.lifetime.opens_at(now))
    }
}

/// The OHTTP-style key config (RFC 9458 §3) of the published key: what devices fetch and pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyConfig {
    encoded: [u8; KeyConfig::LENGTH],
}

impl KeyConfig {
    /// Encoded length: key id, KEM id, public key, suites length, one suite.
    pub const LENGTH: usize = 41;
    /// DHKEM(X25519, HKDF-SHA256).
    const KEM_X25519: [u8; 2] = [0x00, 0x20];
    /// HKDF-SHA256.
    const KDF_HKDF_SHA256: [u8; 2] = [0x00, 0x01];
    /// AES-128-GCM.
    const AEAD_AES_128_GCM: [u8; 2] = [0x00, 0x01];

    /// `key_id || kem_id || public_key || len(suites) || kdf_id || aead_id`.
    #[must_use]
    pub fn new(public_key: &PushRelayPublicKey) -> Self {
        let mut encoded = Vec::with_capacity(Self::LENGTH);
        encoded.push(public_key.key_id());
        encoded.extend_from_slice(&Self::KEM_X25519);
        encoded.extend_from_slice(public_key.public_key());
        encoded.extend_from_slice(&[0x00, 0x04]);
        encoded.extend_from_slice(&Self::KDF_HKDF_SHA256);
        encoded.extend_from_slice(&Self::AEAD_AES_128_GCM);
        let mut fixed = [0; Self::LENGTH];
        for (target, source) in fixed.iter_mut().zip(encoded) {
            *target = source;
        }
        Self { encoded: fixed }
    }

    /// The encoded bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::LENGTH] {
        &self.encoded
    }

    /// SHA-256 of the encoding: the value members pin in Party state.
    #[must_use]
    pub fn hash(&self) -> KeyConfigHash {
        pingers_crypto::key_config_hash(&self.encoded)
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
        RelayKey::new(
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
        ring.for_opening(KeyId::new(id), UnixSeconds::new(now))
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
    fn key_config_encoding_follows_rfc_9458() {
        let config = KeyConfig::new(&PushRelayPublicKey::new(7, RFC7748_PUBLIC));
        let mut expected = vec![7, 0x00, 0x20];
        expected.extend_from_slice(&RFC7748_PUBLIC);
        expected.extend_from_slice(&[0x00, 0x04, 0x00, 0x01, 0x00, 0x01]);
        assert_eq!(config.as_bytes().as_slice(), expected.as_slice());
    }

    #[test]
    fn key_config_hash_is_sha256_of_the_encoding() {
        let config = KeyConfig::new(&PushRelayPublicKey::new(7, RFC7748_PUBLIC));
        assert_eq!(
            config.hash(),
            pingers_crypto::key_config_hash(config.as_bytes())
        );
    }
}
