// SPDX-License-Identifier: AGPL-3.0-only
//! Pingers servers allowed to use the relay, each with a bearer credential issued by the relay
//! operator and its own rate limit (design B.4.9, C.1a).
//!
//! The relay stores only `credential_hash = HMAC-SHA-256(hash_key, "pingers/v1/relay-credential"
//! || credential)` (pingers-crypto's [`keyed_hash`] for [`KeyedHashPurpose::ServerCredential`]),
//! so its configuration file never holds a credential a reader could present.

use std::collections::HashSet;

use pingers_crypto::{CryptoError, KeyedHash, KeyedHashKey, KeyedHashPurpose, keyed_hash};
use subtle::ConstantTimeEq as _;

use crate::error::ConfigError;
use crate::rate::RateLimit;

/// Shortest credential a server may present: 32 characters of a random token is at least 128
/// bits even in hex. Shorter ones are refused before hashing.
pub const MIN_CREDENTIAL_LENGTH: usize = 32;

/// One Pingers server allowed to use the relay.
pub struct ServerEntry {
    /// The operator's label for the server; appears only in configuration errors.
    pub server_id: String,
    /// The keyed hash of its bearer credential.
    pub credential_hash: KeyedHash,
    /// How many pushes per minute it may ask for.
    pub limit: RateLimit,
}

/// Which configured server a request authenticated as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ServerIndex(usize);

/// The configured servers and the key their credentials are hashed with.
pub struct ServerRegistry {
    hash_key: KeyedHashKey,
    entries: Vec<ServerEntry>,
}

impl ServerRegistry {
    /// Checks the server list. `hash_key` is the key the credential hashes were made with.
    ///
    /// # Errors
    ///
    /// [`ConfigError::NoServers`], [`ConfigError::DuplicateServerId`] or
    /// [`ConfigError::DuplicateCredential`].
    pub fn new(hash_key: KeyedHashKey, entries: Vec<ServerEntry>) -> Result<Self, ConfigError> {
        if entries.is_empty() {
            return Err(ConfigError::NoServers);
        }
        let mut ids = HashSet::new();
        for (position, entry) in entries.iter().enumerate() {
            if !ids.insert(entry.server_id.as_str()) {
                return Err(ConfigError::DuplicateServerId {
                    server_id: entry.server_id.clone(),
                });
            }
            let shared = entries
                .iter()
                .skip(position.saturating_add(1))
                .any(|other| bool::from(other.credential_hash.ct_eq(&entry.credential_hash)));
            if shared {
                return Err(ConfigError::DuplicateCredential);
            }
        }
        Ok(Self { hash_key, entries })
    }

    /// The server whose credential is `presented`, or `None`. A credential shorter than
    /// [`MIN_CREDENTIAL_LENGTH`] is refused unhashed. Every entry is compared in constant time,
    /// so timing does not reveal which entry matched.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Internal`] if hashing fails (it cannot with a 32-byte key).
    pub(crate) fn authenticate(
        &self,
        presented: &[u8],
    ) -> Result<Option<ServerIndex>, CryptoError> {
        if presented.len() < MIN_CREDENTIAL_LENGTH {
            return Ok(None);
        }
        let hash = keyed_hash(
            &self.hash_key,
            KeyedHashPurpose::ServerCredential,
            presented,
        )?;
        let mut found = None;
        for (position, entry) in self.entries.iter().enumerate() {
            if bool::from(entry.credential_hash.ct_eq(&hash)) {
                found = Some(ServerIndex(position));
            }
        }
        Ok(found)
    }

    /// The rate limit of an authenticated server.
    pub(crate) fn limit(&self, server: ServerIndex) -> Option<RateLimit> {
        self.entries.get(server.0).map(|entry| entry.limit)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::num::NonZeroU32;

    use zeroize::Zeroizing;

    use super::*;

    pub(crate) const CREDENTIAL_A: &str = "server-a-credential-0123456789abcdef";
    pub(crate) const CREDENTIAL_B: &str = "server-b-credential-0123456789abcdef";
    pub(crate) const HASH_KEY: [u8; 32] = [0x42; 32];

    pub(crate) fn hash_key() -> KeyedHashKey {
        KeyedHashKey::from_bytes(Zeroizing::new(HASH_KEY))
    }

    pub(crate) fn credential_hash(credential: &str) -> KeyedHash {
        keyed_hash(
            &hash_key(),
            KeyedHashPurpose::ServerCredential,
            credential.as_bytes(),
        )
        .unwrap_or(KeyedHash::from_bytes([0; 32]))
    }

    pub(crate) fn entry(server_id: &str, credential: &str, per_minute: u32) -> ServerEntry {
        ServerEntry {
            server_id: server_id.to_owned(),
            credential_hash: credential_hash(credential),
            limit: RateLimit::per_minute(NonZeroU32::new(per_minute).unwrap_or(NonZeroU32::MIN)),
        }
    }

    fn registry() -> Result<ServerRegistry, ConfigError> {
        ServerRegistry::new(
            hash_key(),
            vec![entry("a", CREDENTIAL_A, 10), entry("b", CREDENTIAL_B, 20)],
        )
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn each_credential_authenticates_its_own_server() -> TestResult {
        let registry = registry()?;
        assert_eq!(
            registry.authenticate(CREDENTIAL_A.as_bytes())?,
            Some(ServerIndex(0))
        );
        assert_eq!(
            registry.authenticate(CREDENTIAL_B.as_bytes())?,
            Some(ServerIndex(1))
        );
        Ok(())
    }

    #[test]
    fn a_wrong_or_truncated_credential_is_refused() -> TestResult {
        let registry = registry()?;
        assert_eq!(
            registry.authenticate(b"server-a-credential-0123456789abcdeX")?,
            None
        );
        assert_eq!(registry.authenticate(b"server-a-credential")?, None);
        assert_eq!(registry.authenticate(b"")?, None);
        Ok(())
    }

    #[test]
    fn the_stored_hash_itself_is_not_a_credential() -> TestResult {
        // Someone who reads the configuration file learns the hash; presenting it fails.
        let registry = registry()?;
        let stolen = hex::encode(credential_hash(CREDENTIAL_A).as_bytes());
        assert_eq!(registry.authenticate(stolen.as_bytes())?, None);
        Ok(())
    }

    #[test]
    fn hashes_made_with_another_key_do_not_match() -> TestResult {
        let registry = ServerRegistry::new(
            KeyedHashKey::from_bytes(Zeroizing::new([0x43; 32])),
            vec![entry("a", CREDENTIAL_A, 10)],
        )?;
        assert_eq!(registry.authenticate(CREDENTIAL_A.as_bytes())?, None);
        Ok(())
    }

    #[test]
    fn credentials_shorter_than_32_bytes_are_refused_even_if_configured() -> TestResult {
        let thirty_one = "0123456789012345678901234567890";
        let thirty_two = "01234567890123456789012345678901";
        let registry = ServerRegistry::new(
            hash_key(),
            vec![entry("a", thirty_one, 1), entry("b", thirty_two, 1)],
        )?;
        assert_eq!(registry.authenticate(thirty_one.as_bytes())?, None);
        assert_eq!(
            registry.authenticate(thirty_two.as_bytes())?,
            Some(ServerIndex(1))
        );
        Ok(())
    }

    #[test]
    fn each_server_has_its_own_limit() -> Result<(), ConfigError> {
        let registry = registry()?;
        assert_ne!(
            registry.limit(ServerIndex(0)),
            registry.limit(ServerIndex(1))
        );
        assert_eq!(registry.limit(ServerIndex(2)), None);
        Ok(())
    }

    #[test]
    fn an_empty_server_list_is_refused() {
        assert!(matches!(
            ServerRegistry::new(hash_key(), Vec::new()),
            Err(ConfigError::NoServers)
        ));
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let result = ServerRegistry::new(
            hash_key(),
            vec![entry("a", CREDENTIAL_A, 1), entry("a", CREDENTIAL_B, 1)],
        );
        assert!(matches!(result, Err(ConfigError::DuplicateServerId { .. })));
    }

    #[test]
    fn shared_credentials_are_refused() {
        let result = ServerRegistry::new(
            hash_key(),
            vec![
                entry("a", CREDENTIAL_A, 1),
                entry("b", CREDENTIAL_B, 1),
                entry("c", CREDENTIAL_A, 1),
            ],
        );
        assert!(matches!(result, Err(ConfigError::DuplicateCredential)));
    }
}
