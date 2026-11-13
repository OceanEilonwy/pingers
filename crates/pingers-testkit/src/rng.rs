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

/// An RNG that returns exactly the bytes it was given, so a test vector can list the
/// randomness a computation consumes (`vectors/README.md`, "Randomness"). Words are read
/// little-endian. Reading past the end yields zeros and is recorded, so a test can assert that
/// every byte was used and no more ([`FixedRng::exhausted_exactly`]).
pub struct FixedRng {
    bytes: Vec<u8>,
    position: usize,
    overrun: bool,
}

impl FixedRng {
    /// An RNG that will hand out `bytes`, in order.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            position: 0,
            overrun: false,
        }
    }

    /// Whether every byte was consumed and nothing more was asked for.
    #[must_use]
    pub fn exhausted_exactly(&self) -> bool {
        !self.overrun && self.position == self.bytes.len()
    }

    /// Whether more bytes were asked for than it holds.
    #[must_use]
    pub const fn overrun(&self) -> bool {
        self.overrun
    }
}

impl rand_core::TryRng for FixedRng {
    type Error = core::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0; 4];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0; 8];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Self::Error> {
        for byte in destination {
            if let Some(source) = self.bytes.get(self.position) {
                *byte = *source;
                self.position = self.position.saturating_add(1);
            } else {
                *byte = 0;
                self.overrun = true;
            }
        }
        Ok(())
    }
}

impl rand_core::TryCryptoRng for FixedRng {}

#[cfg(test)]
mod tests {
    use rand_core::Rng as _;

    use super::*;

    #[test]
    fn a_fixed_rng_hands_out_its_bytes_in_order() {
        let mut rng = FixedRng::new(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
        let mut first = [0; 1];
        rng.fill_bytes(&mut first);
        assert_eq!(first, [1]);
        assert_eq!(
            takes_crypto_rng(&mut rng),
            u64::from_le_bytes([2, 3, 4, 5, 6, 7, 8, 9])
        );
        assert!(!rng.exhausted_exactly(), "four bytes left");
        assert_eq!(rng.next_u32(), u32::from_le_bytes([10, 11, 12, 13]));
        assert!(rng.exhausted_exactly());
        assert!(!rng.overrun());
    }

    #[test]
    fn reading_past_the_end_gives_zeros_and_is_recorded() {
        let mut rng = FixedRng::new(&[9]);
        let mut bytes = [7; 3];
        rng.fill_bytes(&mut bytes);
        assert_eq!(bytes, [9, 0, 0]);
        assert!(rng.overrun());
        assert!(!rng.exhausted_exactly());
    }

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
