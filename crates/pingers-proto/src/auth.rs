// SPDX-License-Identifier: AGPL-3.0-only
//! Anonymous authorisation on the wire: the RLN membership proof header (design B.4.8, D).

use pingers_time::Bucket;

use crate::stub::not_implemented;
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

impl WireEncode for RlnAuth {
    fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        not_implemented(self)
    }
}

impl WireDecode for RlnAuth {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        not_implemented(bytes)
    }
}
