// SPDX-License-Identifier: AGPL-3.0-only
//! The SHA-256 preimage hashes of RLN's public inputs (design B.4.8).
//!
//! pingers-anon reduces these into the BN254 scalar field and applies Poseidon; the labels and
//! the framing live here so domain separation stays in one crate (style guide §8).

use pingers_proto::{RlnScope, ServerId};
use pingers_time::Bucket;

use crate::{CryptoError, labels, primitives};

/// A 32-byte SHA-256 output that pingers-anon reduces into the scalar field. Not a secret: the
/// inputs are the public server id, scope, bucket and request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RlnPreimageHash([u8; 32]);

impl RlnPreimageHash {
    /// The hash bytes, read little-endian by the field reduction.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// `SHA-256("pingers/v1/rln" || u64be(len) || server_id || scope || u64be(bucket))`: the hash
/// behind the external nullifier for one server, scope and bucket.
///
/// # Errors
///
/// [`CryptoError::Internal`] if the server id's length does not fit in 64 bits.
pub fn rln_external_nullifier_hash(
    server: &ServerId,
    scope: RlnScope,
    bucket: Bucket,
) -> Result<RlnPreimageHash, CryptoError> {
    let server = server.as_str().as_bytes();
    Ok(RlnPreimageHash(primitives::sha256(&[
        labels::RLN_EXTERNAL_NULLIFIER,
        &length_prefix(server)?,
        server,
        &[scope.to_wire()],
        &bucket.index().to_be_bytes(),
    ])))
}

/// `SHA-256("pingers/v1/rln/x" || u64be(len) || method || u64be(len) || path || body)`: the
/// hash behind the request digest `x` that binds a proof to one request.
///
/// # Errors
///
/// [`CryptoError::Internal`] if a length does not fit in 64 bits.
pub fn rln_request_hash(
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<RlnPreimageHash, CryptoError> {
    Ok(RlnPreimageHash(primitives::sha256(&[
        labels::RLN_REQUEST_DIGEST,
        &length_prefix(method.as_bytes())?,
        method.as_bytes(),
        &length_prefix(path.as_bytes())?,
        path.as_bytes(),
        body,
    ])))
}

/// The length of a variable-length preimage field, as 8 bytes big-endian.
fn length_prefix(bytes: &[u8]) -> Result<[u8; 8], CryptoError> {
    u64::try_from(bytes.len())
        .map(u64::to_be_bytes)
        .map_err(|_| CryptoError::Internal)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn the_request_hash_frames_method_and_path_by_length() -> Result<(), CryptoError> {
        // Without the length prefixes these two would hash the same bytes.
        assert_ne!(
            rln_request_hash("GET", "/ab", b"")?,
            rln_request_hash("GET/", "ab", b"")?
        );
        let expected = primitives::sha256(&[
            b"pingers/v1/rln/x",
            &3_u64.to_be_bytes(),
            b"PUT",
            &2_u64.to_be_bytes(),
            b"/p",
            b"body",
        ]);
        assert_eq!(
            rln_request_hash("PUT", "/p", b"body")?.as_bytes(),
            &expected
        );
        Ok(())
    }

    #[test]
    fn the_external_nullifier_hash_covers_server_scope_and_bucket() -> Result<(), TestError> {
        let server = ServerId::new("https://s.example".into())?;
        let bucket = Bucket::from_index(7)?;
        let hash = rln_external_nullifier_hash(&server, RlnScope::Board, bucket)?;
        let expected = primitives::sha256(&[
            b"pingers/v1/rln",
            &17_u64.to_be_bytes(),
            b"https://s.example",
            &[RlnScope::Board.to_wire()],
            &7_u64.to_be_bytes(),
        ]);
        assert_eq!(hash.as_bytes(), &expected);
        Ok(())
    }
}
