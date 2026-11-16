// SPDX-License-Identifier: AGPL-3.0-only
//! The Party state snapshot padded to a size class before it is sealed (design B.2), so its
//! length tells the server nothing finer than the class about how many members, Spots or
//! names the Party has.
//!
//! Layout of the plaintext: `be32(length) || encoding || zero bytes`, the whole exactly one of
//! [`SNAPSHOT_CLASSES`] long. The largest class leaves room for the commit around it in one
//! request (the gateway takes 64 KiB).

use pingers_crypto::{StateKey, open_state_snapshot, seal_state_snapshot};
use pingers_proto::{ProtoError, SealedSnapshot};
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::PartiesError;

/// The lengths a padded snapshot plaintext may have, smallest first.
pub(crate) const SNAPSHOT_CLASSES: [usize; 4] = [1_024, 4_096, 16_384, 49_152];

/// The length prefix.
const PREFIX: usize = 4;

/// `encoded` padded to the smallest class that holds it with its prefix.
fn pad(encoded: &[u8]) -> Result<Zeroizing<Vec<u8>>, PartiesError> {
    let too_large = || PartiesError::Proto(ProtoError::OutOfRange { field: "snapshot" });
    let needed = encoded.len().checked_add(PREFIX).ok_or_else(too_large)?;
    let class = SNAPSHOT_CLASSES
        .into_iter()
        .find(|class| *class >= needed)
        .ok_or_else(too_large)?;
    let length = u32::try_from(encoded.len()).map_err(|_| too_large())?;
    let mut padded = Zeroizing::new(Vec::with_capacity(class));
    padded.extend_from_slice(&length.to_be_bytes());
    padded.extend_from_slice(encoded);
    padded.resize(class, 0);
    Ok(padded)
}

/// The encoding inside a padded plaintext; anything not exactly a class long, with a length
/// past its end or non-zero padding, is refused.
fn unpad(padded: &[u8]) -> Result<&[u8], PartiesError> {
    let bad = || PartiesError::Proto(ProtoError::BadPadding);
    if !SNAPSHOT_CLASSES.contains(&padded.len()) {
        return Err(bad());
    }
    let (prefix, rest) = padded.split_first_chunk::<PREFIX>().ok_or_else(bad)?;
    let length = usize::try_from(u32::from_be_bytes(*prefix)).map_err(|_| bad())?;
    let (encoded, padding) = rest.split_at_checked(length).ok_or_else(bad)?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err(bad());
    }
    Ok(encoded)
}

/// Pads and seals a snapshot's encoding under `key`.
pub(crate) fn seal(
    key: &StateKey,
    encoded: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<SealedSnapshot, PartiesError> {
    Ok(seal_state_snapshot(key, &pad(encoded)?, rng)?)
}

/// Opens a sealed snapshot and returns its encoding.
pub(crate) fn open(
    key: &StateKey,
    sealed: &SealedSnapshot,
) -> Result<Zeroizing<Vec<u8>>, PartiesError> {
    let opened = open_state_snapshot(key, sealed)?;
    Ok(Zeroizing::new(unpad(&opened)?.to_vec()))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use rand_chacha::ChaCha20Rng;
    use rand_core::SeedableRng as _;

    use super::*;

    #[test]
    fn snapshots_of_one_class_seal_to_one_length_and_open_again() -> Result<(), PartiesError> {
        let key = StateKey::from_bytes(Zeroizing::new([7; 32]));
        let mut rng = ChaCha20Rng::seed_from_u64(1);
        let small = seal(&key, &[1; 10], &mut rng)?;
        let larger = seal(&key, &[2; 900], &mut rng)?;
        assert_eq!(small.as_bytes().len(), larger.as_bytes().len());
        assert_eq!(open(&key, &small)?.as_slice(), [1; 10]);
        assert_eq!(open(&key, &larger)?.as_slice(), [2; 900]);
        // The next class up, and nothing past the largest.
        let next = seal(&key, &[3; 1_100], &mut rng)?;
        assert_eq!(
            next.as_bytes().len() - small.as_bytes().len(),
            SNAPSHOT_CLASSES[1] - SNAPSHOT_CLASSES[0]
        );
        assert!(seal(&key, &vec![4; 49_152], &mut rng).is_err());
        Ok(())
    }

    #[test]
    fn a_plaintext_that_is_not_padded_is_refused() {
        let not_a_class = vec![0; 1_000];
        let mut nonzero_padding = vec![0; 1_024];
        nonzero_padding.push(1);
        nonzero_padding.remove(0);
        let mut length_past_the_end = 2_000_u32.to_be_bytes().to_vec();
        length_past_the_end.resize(1_024, 0);
        for padded in [not_a_class, nonzero_padding, length_past_the_end] {
            assert!(unpad(&padded).is_err());
        }
    }
}
