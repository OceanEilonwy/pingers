// SPDX-License-Identifier: AGPL-3.0-only
//! Domain values that only the client's local storage has, so no lower crate defines them.

use core::fmt;

use crate::StoreError;

/// Defines user-entered text with a length bound. `Debug` is redacted: names are personal data
/// (style guide §9).
macro_rules! bounded_text {
    ($(#[$meta:meta])* $name:ident, $field:literal, $max:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            /// Longest accepted text, in bytes of UTF-8.
            pub const MAX_BYTES: usize = $max;

            /// Checks the length: 1 to [`Self::MAX_BYTES`] bytes.
            ///
            /// # Errors
            ///
            /// [`StoreError::InvalidText`] if empty or too long.
            pub fn new(text: String) -> Result<Self, StoreError> {
                if text.is_empty() || text.len() > $max {
                    Err(StoreError::InvalidText { field: $field, max: $max })
                } else {
                    Ok(Self(text))
                }
            }

            /// The text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(redacted)"))
            }
        }
    };
}

bounded_text!(
    /// A Party's name, as its members see it.
    PartyName,
    "party name",
    200
);
bounded_text!(
    /// A member's display name within a Party.
    MemberName,
    "member name",
    200
);
bounded_text!(
    /// A Spot's name.
    SpotName,
    "spot name",
    200
);
bounded_text!(
    /// An exclusion zone's name (local only).
    ZoneName,
    "zone name",
    200
);
bounded_text!(
    /// Who runs a push relay, as advertised, for "What your server sees".
    OperatorName,
    "operator name",
    200
);
bounded_text!(
    /// Why a fallback is active, as a short code chosen by the crate that activated it (for
    /// example pingers-sync), shown through pingers-words.
    FallbackReason,
    "fallback reason",
    200
);

/// An exclusion zone (design B.11). 16 random bytes; zones are local, so the id never leaves
/// the device.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ZoneId([u8; 16]);

impl ZoneId {
    /// Wraps bytes from a random generator.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Debug for ZoneId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ZoneId(redacted)")
    }
}

/// A member's avatar image, as opaque encoded bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Avatar(Vec<u8>);

impl Avatar {
    /// Wraps encoded image bytes.
    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The encoded image.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Avatar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Avatar(redacted)")
    }
}

/// An unspent Privacy Pass token, as the issuer's response produced it (pingers-anon's
/// `PrivacyPassToken::as_bytes`). Opaque here so the store does not link the prover.
#[derive(Clone, PartialEq, Eq)]
pub struct UnspentToken(Vec<u8>);

impl UnspentToken {
    /// Wraps the token bytes.
    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The token bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for UnspentToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UnspentToken(redacted)")
    }
}

/// The issuer key a Privacy Pass token was issued under (`SHA-256` of the issuer public key,
/// as pingers-anon's `IssuerKey::key_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TokenKeyId([u8; 32]);

impl TokenKeyId {
    /// Wraps the key id.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A leaf position in the RLN membership tree: pingers-proto's [`MembershipIndex`], which owns
/// the concept. The alias keeps the store's earlier name working.
///
/// [`MembershipIndex`]: pingers_proto::MembershipIndex
pub type TreePosition = pingers_proto::MembershipIndex;

/// A level of the RLN membership tree; leaves are level 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreeLevel(u8);

impl TreeLevel {
    /// Wraps a level.
    #[must_use]
    pub const fn new(level: u8) -> Self {
        Self(level)
    }

    /// The level.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// A cached interior node of the RLN membership tree: an encoded field element.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TreeNodeHash([u8; 32]);

impl TreeNodeHash {
    /// Wraps an encoded node.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The encoding.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_bounded_at_both_ends() {
        assert!(PartyName::new(String::new()).is_err());
        assert!(PartyName::new("a".into()).is_ok());
        assert!(PartyName::new("a".repeat(200)).is_ok());
        assert!(matches!(
            PartyName::new("a".repeat(201)),
            Err(StoreError::InvalidText {
                field: "party name",
                max: 200
            })
        ));
    }

    #[test]
    fn names_are_redacted_in_debug_output() {
        let name = SpotName::new("Home".into());
        assert_eq!(format!("{name:?}"), "Ok(SpotName(redacted))");
        assert_eq!(
            format!("{:?}", ZoneId::from_bytes([1; 16])),
            "ZoneId(redacted)"
        );
        assert_eq!(
            format!("{:?}", Avatar::from_bytes(vec![1])),
            "Avatar(redacted)"
        );
        assert_eq!(
            format!("{:?}", UnspentToken::from_bytes(vec![1])),
            "UnspentToken(redacted)"
        );
    }
}
