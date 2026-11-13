// SPDX-License-Identifier: AGPL-3.0-only
//! Inviter fingerprints and safety codes (design B.1, design D invite flow).

use core::fmt;

use pingers_crypto::{AccountPublicKey, account_id};
use pingers_proto::InviterFingerprint;

/// The 8-byte inviter fingerprint in an invite link: the first 8 bytes of the inviter's
/// account id (`SHA-256(pk)[..16]`, design B.1). The design names `inviter_fpr[8]` without a
/// formula; reusing the account id keeps hashing inside pingers-crypto.
#[must_use]
pub fn inviter_fingerprint(key: &AccountPublicKey) -> InviterFingerprint {
    let id = account_id(key);
    let mut bytes = [0; InviterFingerprint::LENGTH];
    for (target, source) in bytes.iter_mut().zip(id.as_bytes()) {
        *target = *source;
    }
    InviterFingerprint::from_bytes(bytes)
}

const DIGITS_PER_KEY: usize = 30;
const GROUP: usize = 5;

/// A 60-digit safety code over two members' leaf keys (design B.1), shown as twelve groups of
/// five digits. Both members compute the same code: each key contributes 30 digits, the
/// smaller key (bytewise) first.
///
/// Each key's digits are its account id (`SHA-256(pk)[..16]`, from pingers-crypto) read as a
/// big-endian integer modulo 10³⁰, so a key contributes about 100 bits. Interim: the crate's
/// CONTRACT.md asks pingers-crypto for a dedicated, iterated safety-number derivation; when it
/// exists, only this function's body changes.
#[derive(Clone, PartialEq, Eq)]
pub struct SafetyCode {
    digits: [u8; 2 * DIGITS_PER_KEY],
}

impl SafetyCode {
    /// The code for a pair of keys, in either order.
    #[must_use]
    pub fn between(first: &AccountPublicKey, second: &AccountPublicKey) -> Self {
        let (low, high) = if first.as_bytes() <= second.as_bytes() {
            (first, second)
        } else {
            (second, first)
        };
        let mut digits = [0; 2 * DIGITS_PER_KEY];
        let (left, right) = digits.split_at_mut(DIGITS_PER_KEY);
        key_digits(low, left);
        key_digits(high, right);
        Self { digits }
    }

    /// Parses a code the user typed: 60 digits, with any spaces between them.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut digits = [0; 2 * DIGITS_PER_KEY];
        let mut count = 0_usize;
        for character in text.chars().filter(|character| !character.is_whitespace()) {
            let value = character.to_digit(10)?;
            *digits.get_mut(count)? = u8::try_from(value).ok()?;
            count = count.checked_add(1)?;
        }
        (count == digits.len()).then_some(Self { digits })
    }
}

/// Writes `key`'s 30 digits, most significant first.
fn key_digits(key: &AccountPublicKey, out: &mut [u8]) {
    let mut value = u128::from_be_bytes(*account_id(key).as_bytes());
    for slot in out.iter_mut().rev() {
        // `value % 10` is below 10, so the conversion cannot fail.
        *slot = u8::try_from(value % 10).unwrap_or(0);
        value /= 10;
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
    fn code_is_each_account_id_modulo_ten_to_the_thirty() {
        let code = SafetyCode::between(&key(1), &key(2));
        let shown = code.to_string();
        assert_eq!(shown.len(), 60 + 11);
        let digits: String = shown.split(' ').collect();
        let expected = |fill| {
            let value = u128::from_be_bytes(*account_id(&key(fill)).as_bytes());
            format!("{:030}", value % 10_u128.pow(30))
        };
        assert_eq!(digits, format!("{}{}", expected(1), expected(2)));
        assert!(shown.split(' ').all(|group| group.len() == 5));
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
    fn fingerprint_is_the_account_id_prefix() {
        let id = account_id(&key(9));
        assert_eq!(
            inviter_fingerprint(&key(9)).as_bytes().as_slice(),
            id.as_bytes().get(..8).unwrap_or_default()
        );
        assert_eq!(
            format!("{:?}", SafetyCode::between(&key(1), &key(1))),
            "SafetyCode(redacted)"
        );
    }
}
