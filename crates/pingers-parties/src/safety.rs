// SPDX-License-Identifier: AGPL-3.0-only
//! Inviter fingerprints and safety codes (design B.1, design D invite flow).

use core::fmt;

/// The inviter fingerprint (`SHA-256(pk)[..8]`), derived in pingers-crypto.
pub use pingers_crypto::inviter_fingerprint;
use pingers_crypto::{AccountPublicKey, SAFETY_NUMBER_DIGITS, safety_number};

const GROUP: usize = 5;

/// A 60-digit safety code over two members' leaf keys (design B.1), shown as twelve groups of
/// five digits. Both members compute the same code: pingers-crypto's `safety_number`, where
/// each key contributes 30 digits of an iterated, labelled SHA-512 and the smaller key comes
/// first.
#[derive(Clone, PartialEq, Eq)]
pub struct SafetyCode {
    digits: [u8; SAFETY_NUMBER_DIGITS],
}

impl SafetyCode {
    /// The code for a pair of keys, in either order.
    #[must_use]
    pub fn between(first: &AccountPublicKey, second: &AccountPublicKey) -> Self {
        Self {
            digits: *safety_number(first, second).digits(),
        }
    }

    /// Parses a code the user typed: 60 digits, with any spaces between them.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut digits = [0; SAFETY_NUMBER_DIGITS];
        let mut count = 0_usize;
        for character in text.chars().filter(|character| !character.is_whitespace()) {
            let value = character.to_digit(10)?;
            *digits.get_mut(count)? = u8::try_from(value).ok()?;
            count = count.checked_add(1)?;
        }
        (count == digits.len()).then_some(Self { digits })
    }
}

impl fmt::Display for SafetyCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, group) in self.digits.chunks(GROUP).enumerate() {
            if index > 0 {
                formatter.write_str(" ")?;
            }
            for digit in group {
                write!(formatter, "{digit}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for SafetyCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SafetyCode(redacted)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(fill: u8) -> AccountPublicKey {
        AccountPublicKey::from_bytes([fill; 32])
    }

    #[test]
    fn both_members_see_the_same_code() {
        assert_eq!(
            SafetyCode::between(&key(1), &key(2)),
            SafetyCode::between(&key(2), &key(1))
        );
        assert_ne!(
            SafetyCode::between(&key(1), &key(2)),
            SafetyCode::between(&key(1), &key(3))
        );
    }

    #[test]
    fn code_shows_the_crypto_safety_number_in_groups_of_five() {
        let code = SafetyCode::between(&key(1), &key(2));
        let shown = code.to_string();
        assert_eq!(shown.len(), 60 + 11);
        assert!(shown.split(' ').all(|group| group.len() == 5));
        let digits: String = shown.split(' ').collect();
        let expected: String = safety_number(&key(1), &key(2))
            .digits()
            .iter()
            .map(|digit| char::from(b'0'.saturating_add(*digit)))
            .collect();
        assert_eq!(digits, expected);
    }

    #[test]
    fn typed_codes_parse_with_spaces_only() {
        let code = SafetyCode::between(&key(4), &key(5));
        assert_eq!(SafetyCode::parse(&code.to_string()), Some(code.clone()));
        let compact: String = code.to_string().split(' ').collect();
        assert_eq!(SafetyCode::parse(&compact), Some(code));
        let shorter: String = compact.chars().skip(1).collect();
        assert_eq!(SafetyCode::parse(&shorter), None);
        assert_eq!(SafetyCode::parse(&format!("{compact}1")), None);
        assert_eq!(SafetyCode::parse(&format!("x{shorter}")), None);
    }

    #[test]
    fn debug_hides_the_code() {
        assert_eq!(
            format!("{:?}", SafetyCode::between(&key(1), &key(1))),
            "SafetyCode(redacted)"
        );
    }
}
