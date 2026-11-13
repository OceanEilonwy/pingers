// SPDX-License-Identifier: AGPL-3.0-only
//! RLN identity and the field elements both sides share (design B.4.8).

use pingers_proto::{RlnScope, ServerId};
use pingers_time::Bucket;
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::AnonError;
use crate::field;

/// Domain label of the external nullifier preimage (design B.4.8).
const EXTERNAL_NULLIFIER_LABEL: &[u8] = b"pingers/v1/rln";
/// Domain label of the request digest preimage.
const REQUEST_DIGEST_LABEL: &[u8] = b"pingers/v1/rln/x";

/// Requests per scope per bucket per device: the RLN message limit `k` (design B.4.8).
pub const MESSAGE_LIMIT: u8 = 8;

/// Defines a public 32-byte BN254 field element in its canonical little-endian encoding.
macro_rules! field_element {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Wraps an encoded field element (from the wire or the tree). Canonicity is
            /// checked where the value is used in a proof.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// The encoding.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }
    };
}

field_element!(
    /// `id_commitment = Poseidon(sk)`, registered once per account (`POST /v1/rln/leaf`).
    IdCommitment
);
field_element!(
    /// A tree leaf: `rate_commitment = Poseidon(id_commitment, k)`, or zero for a removed
    /// device.
    LeafValue
);
field_element!(
    /// A membership tree root. The server accepts the current and previous hourly roots.
    MembershipRoot
);
field_element!(
    /// `nullifier = Poseidon(a1)`: one per spent message slot per bucket and scope.
    Nullifier
);
field_element!(
    /// `x = H(method, path, body)` reduced into the scalar field: binds a proof to its request.
    RequestDigest
);
field_element!(
    /// `Poseidon(H("pingers/v1/rln" || server_id || scope || bucket))`.
    ExternalNullifier
);

impl IdCommitment {
    /// The tree leaf for this identity: `rate_commitment = Poseidon(id_commitment, k)` with
    /// `k = MESSAGE_LIMIT`. The server appends it on `POST /v1/rln/leaf`.
    ///
    /// # Errors
    ///
    /// [`AnonError::NotAFieldElement`] if the commitment is not canonical.
    pub fn leaf(&self) -> Result<LeafValue, AnonError> {
        let commitment = field::decode(&self.0)?;
        let limit = rln::circuit::Fr::from(MESSAGE_LIMIT);
        Ok(LeafValue(field::encode(field::poseidon(&[
            commitment, limit,
        ]))))
    }
}

/// Which of a bucket's [`MESSAGE_LIMIT`] slots a request spends. Persist the next id before
/// sending, and skip ahead on any doubt: reusing one with a different body leaks the secret
/// to a slashing server (design B.4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(u8);

impl MessageId {
    /// Checks the range.
    ///
    /// # Errors
    ///
    /// [`AnonError::MessageIdOutOfRange`] if `id >= MESSAGE_LIMIT`.
    pub const fn new(id: u8) -> Result<Self, AnonError> {
        if id < MESSAGE_LIMIT {
            Ok(Self(id))
        } else {
            Err(AnonError::MessageIdOutOfRange)
        }
    }

    /// The id.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The next id in the bucket, or `None` when the bucket's slots are spent.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match Self::new(self.0.saturating_add(1)) {
            Ok(next) => Some(next),
            Err(_) => None,
        }
    }
}

/// The device's RLN secret `sk`, a BN254 scalar. Stored in the encrypted database; never
/// restored from a backup (re-register instead, design B.4.8). No `Debug` or `Clone`.
pub struct RlnSecret(Zeroizing<[u8; 32]>);

impl RlnSecret {
    /// A fresh uniformly random scalar.
    ///
    /// # Errors
    ///
    /// [`AnonError::Internal`] if sampling fails.
    pub fn generate(rng: &mut impl CryptoRng) -> Result<Self, AnonError> {
        // 64 bytes reduced modulo a 254-bit order: the bias is below 2^-250.
        let mut wide = Zeroizing::new([0_u8; 64]);
        rng.fill_bytes(wide.as_mut_slice());
        let mut scalar = field::reduce(wide.as_slice());
        let secret = Self(Zeroizing::new(field::encode(scalar)));
        zeroize::Zeroize::zeroize(&mut scalar);
        Ok(secret)
    }

    /// Loads a stored secret.
    ///
    /// # Errors
    ///
    /// [`AnonError::NotAFieldElement`] if the bytes are not a canonical scalar.
    pub fn from_bytes(bytes: Zeroizing<[u8; 32]>) -> Result<Self, AnonError> {
        let mut scalar = field::decode(&bytes)?;
        zeroize::Zeroize::zeroize(&mut scalar);
        Ok(Self(bytes))
    }

    /// The encoding, for pingers-store only.
    #[must_use]
    pub fn expose_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// `id_commitment = Poseidon(sk)`.
    ///
    /// # Errors
    ///
    /// [`AnonError::Internal`] if hashing fails.
    pub fn id_commitment(&self) -> Result<IdCommitment, AnonError> {
        let secret = self.scalar()?;
        Ok(IdCommitment(field::encode(field::poseidon(&[*secret]))))
    }

    /// The secret as a field element, wiped on drop.
    pub(crate) fn scalar(&self) -> Result<rln::circuit::SecretFr, AnonError> {
        let mut scalar = field::decode(&self.0)?;
        Ok(rln::circuit::SecretFr::from(&mut scalar))
    }
}

/// The external nullifier for a server, scope and bucket.
///
/// # Errors
///
/// [`AnonError::Internal`] if hashing fails.
pub fn external_nullifier(
    server: &ServerId,
    scope: RlnScope,
    bucket: Bucket,
) -> Result<ExternalNullifier, AnonError> {
    let server = server.as_str().as_bytes();
    let preimage = field::sha256_to_field(&[
        EXTERNAL_NULLIFIER_LABEL,
        &length_prefix(server)?,
        server,
        &[scope.to_wire()],
        &bucket.index().to_be_bytes(),
    ]);
    Ok(ExternalNullifier(field::encode(field::poseidon(&[
        preimage,
    ]))))
}

/// `x = H(method, path, body)` reduced into the scalar field. Client and server compute it the
/// same way; the server compares it with the proof's `x`.
///
/// # Errors
///
/// [`AnonError::Internal`] if hashing fails.
pub fn request_digest(method: &str, path: &str, body: &[u8]) -> Result<RequestDigest, AnonError> {
    let digest = field::sha256_to_field(&[
        REQUEST_DIGEST_LABEL,
        &length_prefix(method.as_bytes())?,
        method.as_bytes(),
        &length_prefix(path.as_bytes())?,
        path.as_bytes(),
        body,
    ]);
    Ok(RequestDigest(field::encode(digest)))
}

/// The length of a variable-length preimage field, as 8 bytes big-endian.
fn length_prefix(bytes: &[u8]) -> Result<[u8; 8], AnonError> {
    u64::try_from(bytes.len())
        .map(u64::to_be_bytes)
        .map_err(|_| AnonError::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_stop_at_the_limit() {
        assert!(MessageId::new(MESSAGE_LIMIT).is_err());
        let last = MessageId::new(MESSAGE_LIMIT.saturating_sub(1));
        assert_eq!(last.ok().and_then(MessageId::next), None);
    }
}
