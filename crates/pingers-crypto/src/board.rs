// SPDX-License-Identifier: AGPL-3.0-only
//! Lease revocation and segment hashes (design B.4.2, B.4.3).

use pingers_proto::{Record, RevokeCommitment, SegmentHash};
use sha2::{Digest as _, Sha256};

use crate::secret::{random_secret, secret_key};

secret_key!(
    /// The key whose hash is a lease's revoke commitment; revealing it revokes or replaces the
    /// lease. `from_bytes` and `expose_bytes` are public for pingers-store (`lease` table) and
    /// for the replacement request, which reveals it.
    RevokeKey,
    32,
    pub
);
random_secret!(RevokeKey);

/// `SHA-256(revoke_key)`.
#[must_use]
pub fn revoke_commitment(key: &RevokeKey) -> RevokeCommitment {
    RevokeCommitment::from_bytes(sha256(&[key.expose_bytes()]))
}

/// SHA-256 over a closed segment's records, concatenated in board order (design B.4.3).
/// Server and clients must agree byte for byte, so this is the one definition.
#[must_use]
pub fn segment_hash(records: &[Record]) -> SegmentHash {
    let mut hasher = Sha256::new();
    for record in records {
        hasher.update(record.to_bytes());
    }
    SegmentHash::from_bytes(copy_32(&hasher.finalize()))
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    copy_32(&hasher.finalize())
}

fn copy_32(digest: &[u8]) -> [u8; 32] {
    let mut bytes = [0; 32];
    for (target, source) in bytes.iter_mut().zip(digest) {
        *target = *source;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_segment_hashes_to_sha256_of_nothing() {
        let hash = segment_hash(&[]);
        assert_eq!(
            hash.as_bytes().first_chunk::<4>(),
            Some(&[0xe3, 0xb0, 0xc4, 0x42])
        );
    }
}
