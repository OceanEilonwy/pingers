// SPDX-License-Identifier: AGPL-3.0-only
//! Safety numbers (design B.1) and inviter fingerprints (design D, invite flow).

use core::fmt;

use pingers_proto::InviterFingerprint;
use sha2::{Digest as _, Sha512};

use crate::{AccountPublicKey, account_id, labels};

/// How many digits a safety number has (design B.1).
pub const SAFETY_NUMBER_DIGITS: usize = 60;
/// Each key contributes half the digits.
const DIGITS_PER_KEY: usize = SAFETY_NUMBER_DIGITS / 2;
/// Digits taken from each five-byte slice of the hash.
const GROUP_DIGITS: usize = 5;
/// Bytes of hash per group of five digits: 40 bits, reduced modulo 10⁵.
const GROUP_BYTES: usize = 5;
/// SHA-512 iterations per key. Decision: the same work factor as Signal's safety numbers, so a
/// second key matching someone's 30 digits costs about 2¹⁰⁰ × 5200 hashes.
const ITERATIONS: u32 = 5200;

/// A 60-digit safety number over two members' leaf keys (design B.1). Both members compute
/// the same number: each key contributes 30 digits, the bytewise smaller key first.
///
/// Each key's digits come from `h = SHA-512("pingers/v1/safety-number" || pk)`, then
/// `h = SHA-512(h || pk)` 5199 more times; the first 30 bytes of `h`, read as six big-endian
/// 40-bit integers, each give five digits (the integer modulo 10⁵). `Debug` does not print the
/// digits (style guide §9: they identify the pair).
#[derive(Clone, PartialEq, Eq)]
pub struct SafetyNumber {
    digits: [u8; SAFETY_NUMBER_DIGITS],
}

impl SafetyNumber {
    /// The digits, most significant first, each 0 to 9.
    #[must_use]
    pub const fn digits(&self) -> &[u8; SAFETY_NUMBER_DIGITS] {
        &self.digits
    }
}

impl fmt::Debug for SafetyNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SafetyNumber(redacted)")
    }
}

/// The safety number for a pair of leaf keys, in either order (design B.1).
#[must_use]
pub fn safety_number(first: &AccountPublicKey, second: &AccountPublicKey) -> SafetyNumber {
    let (low, high) = if first.as_bytes() <= second.as_bytes() {
        (first, second)
    } else {
        (second, first)
    };
    let mut digits = [0; SAFETY_NUMBER_DIGITS];
    let (left, right) = digits.split_at_mut(DIGITS_PER_KEY);
    key_digits(low, left);
    key_digits(high, right);
    SafetyNumber { digits }
}

/// Writes `key`'s 30 digits into `out`.
fn key_digits(key: &AccountPublicKey, out: &mut [u8]) {
    let mut hash = Sha512::new()
        .chain_update(labels::SAFETY_NUMBER)
        .chain_update(key.as_bytes())
        .finalize();
    for _ in 1..ITERATIONS {
        hash = Sha512::new()
            .chain_update(hash)
            .chain_update(key.as_bytes())
            .finalize();
    }
    let (groups, _) = out.as_chunks_mut::<GROUP_DIGITS>();
    let (slices, _) = hash.as_chunks::<GROUP_BYTES>();
    for (group, bytes) in groups.iter_mut().zip(slices) {
        let mut wide = [0_u8; 8];
        for (target, source) in wide.iter_mut().skip(8 - GROUP_BYTES).zip(bytes) {
            *target = *source;
        }
        let mut value = u64::from_be_bytes(wide);
        // The five lowest decimal digits are the value modulo 10⁵.
        for slot in group.iter_mut().rev() {
            let digit = value.checked_rem(10).unwrap_or(0);
            // A remainder of division by 10 always fits.
            *slot = u8::try_from(digit).unwrap_or(0);
            value = value.checked_div(10).unwrap_or(0);
        }
    }
}

/// The inviter fingerprint in an invite link: the inviter's whole account id,
/// `SHA-256(pk)[..16]` (design D: `inviter_fpr[16]`, widened from 8
/// bytes on 2026-11-17 so that no other key can be searched for that matches it).
#[must_use]
pub fn inviter_fingerprint(key: &AccountPublicKey) -> InviterFingerprint {
    let id = account_id(key);
    let mut bytes = [0; InviterFingerprint::LENGTH];
    for (target, source) in bytes.iter_mut().zip(id.as_bytes()) {
        *target = *source;
    }
    InviterFingerprint::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_testkit::load_vectors;

    use super::*;
    use crate::test_support::{TestError, input_array};

    fn key(fill: u8) -> AccountPublicKey {
        AccountPublicKey::from_bytes([fill; 32])
    }

    fn shown(number: &SafetyNumber) -> String {
        number
            .digits()
            .iter()
            .map(|digit| char::from(b'0'.saturating_add(*digit)))
            .collect()
    }

    #[test]
    fn safety_number_vectors() -> Result<(), TestError> {
        let file = load_vectors("crypto/safety-number")?;
        assert_eq!(file.name, "crypto/safety-number");
        assert!(!file.vectors.is_empty());
        for vector in file.vectors {
            let first = AccountPublicKey::from_bytes(input_array(&vector, "first_key")?);
            let second = AccountPublicKey::from_bytes(input_array(&vector, "second_key")?);
            let expected = vector.expected_str("digits")?;
            assert_eq!(
                shown(&safety_number(&first, &second)),
                expected,
                "{}",
                vector.name
            );
            assert_eq!(
                shown(&safety_number(&second, &first)),
                expected,
                "{}",
                vector.name
            );
            assert_eq!(
                inviter_fingerprint(&first).as_bytes().as_slice(),
                vector.expected_bytes("first_fingerprint")?,
                "{}",
                vector.name
            );
        }
        Ok(())
    }

    #[test]
    fn each_key_moves_its_own_half() {
        let base = shown(&safety_number(&key(1), &key(2)));
        let changed = shown(&safety_number(&key(1), &key(3)));
        assert_eq!(base.get(..DIGITS_PER_KEY), changed.get(..DIGITS_PER_KEY));
        assert_ne!(base.get(DIGITS_PER_KEY..), changed.get(DIGITS_PER_KEY..));
        assert!(
            safety_number(&key(1), &key(2))
                .digits()
                .iter()
                .all(|digit| *digit < 10)
        );
    }

    #[test]
    fn debug_hides_the_digits() {
        assert_eq!(
            format!("{:?}", safety_number(&key(1), &key(2))),
            "SafetyNumber(redacted)"
        );
    }
}
