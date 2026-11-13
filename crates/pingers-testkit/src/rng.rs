// SPDX-License-Identifier: AGPL-3.0-only
//! Seeded randomness (style guide §6).

use rand_core::SeedableRng as _;

/// The RNG tests pass wherever production code takes `&mut impl CryptoRng`.
pub type TestRng = rand_chacha::ChaCha20Rng;

/// A deterministic, cryptographically strong RNG for tests: the same seed gives the same dummy
/// slots, nonces and padding on every run (style guide §6). Use a different seed per party in
/// a test (Alice 1, Bob 2, ...) so their randomness does not coincide.
#[must_use]
pub fn seeded_rng(seed: u64) -> TestRng {
    TestRng::seed_from_u64(seed)
}

/// `N` random bytes, for ids and secrets in fixtures.
pub fn random_array<const N: usize>(rng: &mut impl rand_core::CryptoRng) -> [u8; N] {
    let mut bytes = [0_u8; N];
    rng.fill_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Production signatures take `impl CryptoRng`; this does not compile if `TestRng` is not one.
    fn takes_crypto_rng(rng: &mut impl rand_core::CryptoRng) -> u64 {
        rng.next_u64()
    }

    #[test]
    fn same_seed_gives_same_stream() {
        let mut first = seeded_rng(7);
        let mut second = seeded_rng(7);
        assert_eq!(takes_crypto_rng(&mut first), takes_crypto_rng(&mut second));
        assert_eq!(
            random_array::<32>(&mut first),
            random_array::<32>(&mut second)
        );
    }

    #[test]
    fn different_seeds_give_different_streams() {
        assert_ne!(
            random_array::<16>(&mut seeded_rng(1)),
            random_array::<16>(&mut seeded_rng(2))
        );
    }

    #[test]
    fn random_array_fills_every_byte() {
        // A zero tail would mean the buffer was only partly filled.
        let bytes = random_array::<64>(&mut seeded_rng(3));
        assert!(bytes.iter().rev().take(8).any(|byte| *byte != 0));
    }
}
