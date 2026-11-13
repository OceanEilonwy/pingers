// SPDX-License-Identifier: AGPL-3.0-only
//! Anonymous authorisation on the wire: the RLN membership proof header (design B.4.8, D).

use pingers_time::Bucket;

use crate::{ProtoError, WireDecode, WireEncode};

/// Which per-bucket limit a request spends (design B.4.8): 8 of each per device per bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RlnScope {
    /// Board syncs. Wire value 0.
    Board,
    /// Mailbox registrations and posts. Wire value 1.
    Mailbox,
    /// Group sessions. Wire value 2.
    Group,
}

impl RlnScope {
    /// The wire value.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Board => 0,
            Self::Mailbox => 1,
            Self::Group => 2,
        }
    }

    /// Parses the wire value.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] for any value other than 0, 1 or 2.
    pub const fn from_wire(value: u8) -> Result<Self, ProtoError> {
        match value {
            0 => Ok(Self::Board),
            1 => Ok(Self::Mailbox),
            2 => Ok(Self::Group),
            _ => Err(ProtoError::OutOfRange { field: "rln scope" }),
        }
    }
}

/// The server's identity in RLN external nullifiers: its canonical `https://` origin, as
/// advertised in its parameters (design B.4.8 names it `server_id`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServerId(String);

impl ServerId {
    /// Checks the scheme.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] unless the origin starts with `https://`.
    pub fn new(origin: String) -> Result<Self, ProtoError> {
        if origin.starts_with("https://") {
            Ok(Self(origin))
        } else {
            Err(ProtoError::OutOfRange { field: "server id" })
        }
    }

    /// The origin.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `Authorization: PingersRln <base64url(RlnAuth)>`: 265 bytes in this field order
/// (design D). Field elements are 32-byte encodings in the BN254 scalar field.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RlnAuth {
    /// The membership tree root; must be the current or previous hourly root.
    pub root: [u8; 32],
    /// Which limit this request spends.
    pub scope: RlnScope,
    /// Must be the current bucket, or the previous one within 60 s of rollover.
    pub bucket: Bucket,
    /// `H(method, path, body)` reduced into the scalar field.
    pub x: [u8; 32],
    /// `sk + a1 · x`.
    pub y: [u8; 32],
    /// `Poseidon(a1)`.
    pub nullifier: [u8; 32],
    /// Compressed Groth16 proof `(A, B, C)` over BN254.
    pub proof: [u8; 128],
}

impl RlnAuth {
    /// Encoded size in bytes.
    pub const SIZE: usize = 265;
}

impl core::fmt::Debug for RlnAuth {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RlnAuth")
            .field("scope", &self.scope)
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

/// The fixed 265-byte layout: `root (32) || scope (1) || be64(bucket) || x (32) || y (32) ||
/// nullifier (32) || proof (128)`.
impl WireEncode for RlnAuth {
    fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        let mut bytes = Vec::with_capacity(Self::SIZE);
        bytes.extend_from_slice(&self.root);
        bytes.push(self.scope.to_wire());
        bytes.extend_from_slice(&self.bucket.to_be_bytes());
        bytes.extend_from_slice(&self.x);
        bytes.extend_from_slice(&self.y);
        bytes.extend_from_slice(&self.nullifier);
        bytes.extend_from_slice(&self.proof);
        Ok(bytes)
    }
}

/// Splits the next `LENGTH` bytes off `bytes`.
fn take<'bytes, const LENGTH: usize>(
    bytes: &mut &'bytes [u8],
) -> Result<&'bytes [u8; LENGTH], ProtoError> {
    let (head, rest) = bytes
        .split_first_chunk::<LENGTH>()
        .ok_or(ProtoError::WrongLength {
            expected: RlnAuth::SIZE,
            actual: bytes.len(),
        })?;
    *bytes = rest;
    Ok(head)
}

impl WireDecode for RlnAuth {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        if bytes.len() != Self::SIZE {
            return Err(ProtoError::WrongLength {
                expected: Self::SIZE,
                actual: bytes.len(),
            });
        }
        let mut rest = bytes;
        let root = *take::<32>(&mut rest)?;
        let [scope] = *take::<1>(&mut rest)?;
        let bucket = u64::from_be_bytes(*take::<8>(&mut rest)?);
        Ok(Self {
            root,
            scope: RlnScope::from_wire(scope)?,
            bucket: Bucket::from_index(bucket).map_err(ProtoError::Bucket)?,
            x: *take::<32>(&mut rest)?,
            y: *take::<32>(&mut rest)?,
            nullifier: *take::<32>(&mut rest)?,
            proof: *take::<128>(&mut rest)?,
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn sample() -> Result<RlnAuth, ProtoError> {
        Ok(RlnAuth {
            root: [1; 32],
            scope: RlnScope::Group,
            bucket: Bucket::from_index(0x0102_0304).map_err(ProtoError::Bucket)?,
            x: [3; 32],
            y: [4; 32],
            nullifier: [5; 32],
            proof: [6; 128],
        })
    }

    #[test]
    fn rln_auth_fields_sit_at_their_offsets() -> Result<(), ProtoError> {
        let bytes = sample()?.encode()?;
        assert_eq!(bytes.len(), RlnAuth::SIZE);
        assert_eq!(bytes.get(31..33), Some(&[1, 2][..]));
        assert_eq!(bytes.get(33..41), Some(&[0, 0, 0, 0, 1, 2, 3, 4][..]));
        assert_eq!(bytes.get(41), Some(&3));
        assert_eq!(bytes.get(73), Some(&4));
        assert_eq!(bytes.get(105), Some(&5));
        assert_eq!(bytes.get(137), Some(&6));
        assert_eq!(bytes.last(), Some(&6));
        Ok(())
    }

    #[test]
    fn rln_scope_wire_values_round_trip() -> Result<(), ProtoError> {
        for scope in [RlnScope::Board, RlnScope::Mailbox, RlnScope::Group] {
            assert_eq!(RlnScope::from_wire(scope.to_wire())?, scope);
        }
        assert_eq!(RlnScope::Mailbox.to_wire(), 1);
        assert!(RlnScope::from_wire(3).is_err());
        Ok(())
    }

    #[test]
    fn rln_auth_with_unknown_scope_is_rejected() -> Result<(), ProtoError> {
        let mut bytes = sample()?.encode()?;
        if let Some(scope) = bytes.get_mut(32) {
            *scope = 3;
        }
        assert!(matches!(
            RlnAuth::decode(&bytes),
            Err(ProtoError::OutOfRange { field: "rln scope" })
        ));
        Ok(())
    }

    #[test]
    fn rln_auth_with_bucket_beyond_range_is_rejected() -> Result<(), ProtoError> {
        let mut bytes = sample()?.encode()?;
        bytes.splice(33..41, u64::MAX.to_be_bytes());
        assert!(matches!(
            RlnAuth::decode(&bytes),
            Err(ProtoError::Bucket(_))
        ));
        Ok(())
    }

    #[test]
    fn rln_auth_of_wrong_length_is_rejected() -> Result<(), ProtoError> {
        let mut bytes = sample()?.encode()?;
        bytes.push(0);
        assert!(matches!(
            RlnAuth::decode(&bytes),
            Err(ProtoError::WrongLength {
                expected: 265,
                actual: 266
            })
        ));
        Ok(())
    }

    #[test]
    fn server_id_must_be_https() {
        assert!(ServerId::new(String::from("http://pingers.example")).is_err());
        assert!(ServerId::new(String::from("https://pingers.example")).is_ok());
    }
}
