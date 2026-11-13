// SPDX-License-Identifier: AGPL-3.0-only
//! Pingers servers allowed to use the relay, each with a bearer credential issued by the relay
//! operator and its own rate limit (design B.4.9, C.1a).

use std::collections::HashSet;

use subtle::{Choice, ConstantTimeEq as _};
use zeroize::Zeroizing;

use crate::error::ConfigError;
use crate::rate::RateLimit;

/// Shortest accepted credential: 32 characters of a random token is at least 128 bits even
/// in hex.
const MIN_CREDENTIAL_LENGTH: usize = 32;

/// A server's bearer credential. No `Debug` or `Clone`: it is a secret (style guide §8).
pub struct ServerCredential(Zeroizing<Vec<u8>>);

impl ServerCredential {
    /// Wraps a credential from configuration.
    #[must_use]
    pub const fn new(credential: Zeroizing<Vec<u8>>) -> Self {
        Self(credential)
    }

    fn matches(&self, presented: &[u8]) -> Choice {
        self.0.as_slice().ct_eq(presented)
    }
}

/// One Pingers server allowed to use the relay.
pub struct ServerEntry {
    /// The operator's label for the server; appears only in configuration errors.
    pub server_id: String,
    /// Its bearer credential.
    pub credential: ServerCredential,
    /// How many pushes per minute it may ask for.
    pub limit: RateLimit,
}

/// Which configured server a request authenticated as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ServerIndex(usize);

/// The configured servers.
pub struct ServerRegistry {
    entries: Vec<ServerEntry>,
}

impl ServerRegistry {
    /// Checks the server list.
    ///
    /// # Errors
    ///
    /// [`ConfigError::NoServers`], [`ConfigError::DuplicateServerId`],
    /// [`ConfigError::DuplicateCredential`] or [`ConfigError::WeakCredential`].
    pub fn new(entries: Vec<ServerEntry>) -> Result<Self, ConfigError> {
        if entries.is_empty() {
            return Err(ConfigError::NoServers);
        }
        let mut ids = HashSet::new();
        for (position, entry) in entries.iter().enumerate() {
            if entry.credential.0.len() < MIN_CREDENTIAL_LENGTH {
                return Err(ConfigError::WeakCredential {
                    server_id: entry.server_id.clone(),
                });
            }
            if !ids.insert(entry.server_id.as_str()) {
                return Err(ConfigError::DuplicateServerId {
                    server_id: entry.server_id.clone(),
                });
            }
            let shared = entries
                .iter()
                .skip(position.saturating_add(1))
                .any(|other| bool::from(other.credential.matches(&entry.credential.0)));
            if shared {
                return Err(ConfigError::DuplicateCredential);
            }
        }
        Ok(Self { entries })
    }

    /// The server whose credential equals `presented`. Every entry is compared in constant
    /// time, so timing does not reveal how much of a credential was right or which entry
    /// matched.
    pub(crate) fn authenticate(&self, presented: &[u8]) -> Option<ServerIndex> {
        let mut found = None;
        for (position, entry) in self.entries.iter().enumerate() {
            if bool::from(entry.credential.matches(presented)) {
                found = Some(ServerIndex(position));
            }
        }
        found
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

    use super::*;

    pub(crate) const CREDENTIAL_A: &str = "server-a-credential-0123456789abcdef";
    pub(crate) const CREDENTIAL_B: &str = "server-b-credential-0123456789abcdef";

    pub(crate) fn entry(server_id: &str, credential: &str, per_minute: u32) -> ServerEntry {
        ServerEntry {
            server_id: server_id.to_owned(),
            credential: ServerCredential::new(Zeroizing::new(credential.as_bytes().to_vec())),
            limit: RateLimit::per_minute(NonZeroU32::new(per_minute).unwrap_or(NonZeroU32::MIN)),
        }
    }

    fn registry() -> Result<ServerRegistry, ConfigError> {
        ServerRegistry::new(vec![
            entry("a", CREDENTIAL_A, 10),
            entry("b", CREDENTIAL_B, 20),
        ])
    }

    #[test]
    fn each_credential_authenticates_its_own_server() -> Result<(), ConfigError> {
        let registry = registry()?;
        let a = registry.authenticate(CREDENTIAL_A.as_bytes());
        let b = registry.authenticate(CREDENTIAL_B.as_bytes());
        assert_eq!(a, Some(ServerIndex(0)));
        assert_eq!(b, Some(ServerIndex(1)));
        Ok(())
    }

    #[test]
    fn a_wrong_or_truncated_credential_is_refused() -> Result<(), ConfigError> {
        let registry = registry()?;
        assert_eq!(
            registry.authenticate(b"server-a-credential-0123456789abcdeX"),
            None
        );
        assert_eq!(registry.authenticate(b"server-a-credential"), None);
        assert_eq!(registry.authenticate(b""), None);
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
            ServerRegistry::new(Vec::new()),
            Err(ConfigError::NoServers)
        ));
    }

    #[test]
    fn a_short_credential_is_refused() {
        let thirty_one = "0123456789012345678901234567890";
        let result = ServerRegistry::new(vec![entry("a", thirty_one, 1)]);
        assert!(matches!(result, Err(ConfigError::WeakCredential { .. })));
        let thirty_two = "01234567890123456789012345678901";
        assert!(ServerRegistry::new(vec![entry("a", thirty_two, 1)]).is_ok());
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let result = ServerRegistry::new(vec![
            entry("a", CREDENTIAL_A, 1),
            entry("a", CREDENTIAL_B, 1),
        ]);
        assert!(matches!(result, Err(ConfigError::DuplicateServerId { .. })));
    }

    #[test]
    fn shared_credentials_are_refused() {
        let result = ServerRegistry::new(vec![
            entry("a", CREDENTIAL_A, 1),
            entry("b", CREDENTIAL_B, 1),
            entry("c", CREDENTIAL_A, 1),
        ]);
        assert!(matches!(result, Err(ConfigError::DuplicateCredential)));
    }
}
