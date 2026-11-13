// SPDX-License-Identifier: AGPL-3.0-only
//! BN254 scalar field helpers shared by both sides: canonical encoding, hashing into the
//! field, and Poseidon.
//!
//! Encoding: 32 bytes, little-endian, canonical (below the field modulus), as zerokit and
//! arkworks serialise `Fr`. Hashing into the field: SHA-256, read little-endian, reduced
//! modulo the field order.

use ark_ff::{BigInteger as _, PrimeField as _};
use rln::circuit::Fr;
use rln::hashers::{Hasher, PoseidonHash};
use sha2::{Digest as _, Sha256};

use crate::AnonError;

/// Decodes a canonical little-endian field element.
pub(crate) fn decode(bytes: &[u8; 32]) -> Result<Fr, AnonError> {
    let mut limbs = [0_u64; 4];
    let (chunks, _) = bytes.as_chunks::<8>();
    for (limb, chunk) in limbs.iter_mut().zip(chunks) {
        *limb = u64::from_le_bytes(*chunk);
    }
    // `from_bigint` returns `None` for values at or above the modulus.
    Fr::from_bigint(ark_ff::BigInt::new(limbs)).ok_or(AnonError::NotAFieldElement)
}

/// Encodes a field element canonically.
pub(crate) fn encode(element: Fr) -> [u8; 32] {
    let mut out = [0_u8; 32];
    for (chunk, byte) in out.iter_mut().zip(element.into_bigint().to_bytes_le()) {
        *chunk = byte;
    }
    out
}

/// Reduces any little-endian byte string modulo the field order.
pub(crate) fn reduce(bytes: &[u8]) -> Fr {
    Fr::from_le_bytes_mod_order(bytes)
}

/// SHA-256 of the concatenation of `parts`, reduced into the field.
pub(crate) fn sha256_to_field(parts: &[&[u8]]) -> Fr {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    reduce(&hasher.finalize())
}

/// Poseidon over 1 to 16 inputs (the circom parameters the circuit uses).
pub(crate) fn poseidon(inputs: &[Fr]) -> Fr {
    Hasher::<PoseidonHash>::hash_list(inputs)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    /// The BN254 scalar field modulus, little-endian.
    const MODULUS_LE: [u8; 32] = [
        0x01, 0x00, 0x00, 0xf0, 0x93, 0xf5, 0xe1, 0x43, 0x91, 0x70, 0xb9, 0x79, 0x48, 0xe8, 0x33,
        0x28, 0x5d, 0x58, 0x81, 0x81, 0xb6, 0x45, 0x50, 0xb8, 0x29, 0xa0, 0x31, 0xe1, 0x72, 0x4e,
        0x64, 0x30,
    ];

    #[test]
    fn the_modulus_is_not_a_field_element() {
        assert!(matches!(
            decode(&MODULUS_LE),
            Err(AnonError::NotAFieldElement)
        ));
    }

    #[test]
    fn the_largest_element_round_trips() -> Result<(), AnonError> {
        let mut largest = MODULUS_LE;
        largest[0] = 0;
        assert_eq!(encode(decode(&largest)?), largest);
        Ok(())
    }

    #[test]
    fn decoding_is_little_endian() -> Result<(), AnonError> {
        let mut one = [0_u8; 32];
        one[0] = 1;
        let mut high = [0_u8; 32];
        high[8] = 1;
        assert_eq!(decode(&one)?, Fr::from(1_u64));
        assert_eq!(decode(&high)?, Fr::from(1_u128 << 64));
        Ok(())
    }

    #[test]
    fn reduction_wraps_at_the_modulus() {
        assert_eq!(reduce(&MODULUS_LE), Fr::from(0_u64));
    }

    #[test]
    fn sha256_parts_are_concatenated() {
        assert_eq!(sha256_to_field(&[b"ab", b"c"]), sha256_to_field(&[b"abc"]));
        assert_ne!(sha256_to_field(&[b"abc"]), sha256_to_field(&[b"abd"]));
    }
}
