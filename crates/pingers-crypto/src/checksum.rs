// SPDX-License-Identifier: AGPL-3.0-only
//! Content checksums of public files: SHA-256 over map packs, place indexes and manifests
//! (design B.12, "Map host"). The map host tags each file with it, pingers-tiles writes it into
//! manifests, and the app checks every download against the manifest before installing it.
//!
//! Nothing secret goes through these functions and they are not keyed: a checksum says which
//! bytes a file holds, not who made it. They live here because only this crate calls hash
//! primitives (style guide §8).

use sha2::{Digest as _, Sha256};

/// The SHA-256 of a file's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    /// The digest as lowercase hex, as manifests and entity tags write it.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut text = String::with_capacity(64);
        for byte in self.0 {
            text.push(hex_digit(byte >> 4));
            text.push(hex_digit(byte & 0x0f));
        }
        text
    }

    /// Parses 64 hex digits, either case: a manifest's `sha256`.
    ///
    /// # Errors
    ///
    /// [`ChecksumError::NotHex`] for anything else.
    pub fn from_hex(text: &str) -> Result<Self, ChecksumError> {
        let digits = text.as_bytes();
        if digits.len() != 64 {
            return Err(ChecksumError::NotHex);
        }
        let mut bytes = [0_u8; 32];
        let (pairs, _rest) = digits.as_chunks::<2>();
        for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = (hex_value(high)? << 4) | hex_value(low)?;
        }
        Ok(Self(bytes))
    }

    /// The digest's bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Why a checksum could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ChecksumError {
    /// Not 64 hex digits.
    #[error("not a SHA-256 in hex")]
    NotHex,
}

/// SHA-256 over bytes fed in pieces, for files too large to hold in memory.
#[derive(Clone, Default)]
pub struct ContentHasher(Sha256);

impl ContentHasher {
    /// A hasher that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self(Sha256::new())
    }

    /// Feeds the next bytes.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The digest of everything fed.
    #[must_use]
    pub fn finish(self) -> ContentDigest {
        let digest = self.0.finalize();
        let mut bytes = [0_u8; 32];
        for (target, source) in bytes.iter_mut().zip(digest.iter()) {
            *target = *source;
        }
        ContentDigest(bytes)
    }
}

/// The SHA-256 of `bytes`.
#[must_use]
pub fn content_digest(bytes: &[u8]) -> ContentDigest {
    let mut hasher = ContentHasher::new();
    hasher.update(bytes);
    hasher.finish()
}

fn hex_digit(nibble: u8) -> char {
    char::from(
        b"0123456789abcdef"
            .get(usize::from(nibble))
            .copied()
            .unwrap_or(b'0'),
    )
}

fn hex_value(digit: u8) -> Result<u8, ChecksumError> {
    match digit {
        b'0'..=b'9' => Ok(digit.wrapping_sub(b'0')),
        b'a'..=b'f' => Ok(digit.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Ok(digit.wrapping_sub(b'A').wrapping_add(10)),
        _ => Err(ChecksumError::NotHex),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    /// FIPS 180-2's "abc" vector.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn digest_of_abc_is_the_standard_vector() {
        assert_eq!(content_digest(b"abc").to_hex(), ABC);
    }

    #[test]
    fn pieces_hash_like_the_whole() {
        let mut hasher = ContentHasher::new();
        hasher.update(b"a");
        hasher.update(b"");
        hasher.update(b"bc");
        assert_eq!(hasher.finish(), content_digest(b"abc"));
    }

    #[test]
    fn hex_round_trips_in_either_case() -> Result<(), ChecksumError> {
        let digest = content_digest(b"abc");
        assert_eq!(ContentDigest::from_hex(ABC)?, digest);
        assert_eq!(ContentDigest::from_hex(&ABC.to_uppercase())?, digest);
        assert_eq!(digest.as_bytes().first(), Some(&0xba));
        Ok(())
    }

    #[test]
    fn malformed_hex_is_refused() {
        let tail: String = ABC.chars().skip(1).collect();
        let head: String = ABC.chars().take(63).collect();
        let mut cases = vec![
            String::new(),
            tail.clone(),
            format!("{ABC}0"),
            format!("g{tail}"),
        ];
        // The characters either side of each hex range.
        cases.extend(['/', ':', '@', 'G', '`', 'g'].map(|last| format!("{head}{last}")));
        for text in cases {
            assert_eq!(
                ContentDigest::from_hex(&text),
                Err(ChecksumError::NotHex),
                "{text}"
            );
        }
    }
}
