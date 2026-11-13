// SPDX-License-Identifier: AGPL-3.0-only
//! Server API requests and responses not covered by a more specific module (design D).
//!
//! The implementer adds the remaining API structs (Privacy Pass issuance, membership tree
//! leaves, mailbox reads, group state) here, following the same pattern.

use pingers_time::UnixSeconds;

use crate::stub::not_implemented;
use crate::{
    AccountId, BoardPage, KeyConfigHash, Lease, LeaseId, ProtoError, Redacted, RelayId, RelayUrl,
    WireDecode, WireEncode,
};

/// A push relay the server advertises (design D, `PushRelayInfo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRelayInfo {
    /// The id mailboxes refer to it by.
    pub relay_id: RelayId,
    /// Where it is.
    pub url: RelayUrl,
    /// Hash of its key config, to pin.
    pub key_config_hash: KeyConfigHash,
    /// Who runs it, as declared; shown in "What your server sees".
    pub operator: String,
}

/// An anonymous credential a server accepts (design B.4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AnonymousCredential {
    /// RLN membership proofs.
    MembershipProof,
    /// Privacy Pass tokens.
    PrivacyPass,
}

/// Whether the server slashes a device that reuses a nullifier with a different body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slashing {
    /// Duplicates are rejected and the leaked share is used to ban the account.
    On,
    /// Duplicates are rejected; shares are ignored (the launch default).
    Off,
}

/// Whether devices may register their own push relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CustomPushRelays {
    /// `RelayRef::Custom` is accepted.
    Allowed,
    /// Only listed relays are accepted.
    NotAllowed,
}

/// Probability that a routine cover upload also reads the board (design B.4.3): 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct CoverReadRatio(f32);

impl CoverReadRatio {
    /// Every upload reads (the default).
    pub const ALWAYS: Self = Self(1.0);

    /// Checks the range.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] unless `ratio` is a number from 0 to 1.
    pub fn new(ratio: f32) -> Result<Self, ProtoError> {
        if (0.0..=1.0).contains(&ratio) {
            Ok(Self(ratio))
        } else {
            Err(ProtoError::OutOfRange {
                field: "cover read ratio",
            })
        }
    }

    /// The probability.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// `GET /v1/params` (design D). Server configuration every client needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// The credentials the server accepts for anonymous requests.
    pub anonymous_credentials: Vec<AnonymousCredential>,
    /// Whether the server slashes.
    pub rln_slashing: Slashing,
    /// How often cover uploads read the board.
    pub cover_read_ratio: CoverReadRatio,
    /// Records per board page (a count at the wire boundary).
    pub board_page_records: u32,
    /// The OHTTP relays to use; never empty for a usable server.
    pub ohttp_relays: Vec<RelayUrl>,
    /// The push relays the server can forward to.
    pub push_relays: Vec<PushRelayInfo>,
    /// Whether devices may register their own push relay.
    pub custom_push_relays: CustomPushRelays,
    /// Who runs the server, as declared.
    pub server_operator: String,
    /// The server's clock, for skew correction (design F.1).
    pub server_time: UnixSeconds,
}

/// `POST /v1/accounts` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterRequest {
    /// The account's Ed25519 public key.
    pub signature_key: [u8; 32],
    /// Signature proving possession of the key.
    pub proof: [u8; 64],
    /// The operator-issued registration code, if the server requires one.
    pub registration_code: Option<Redacted<String>>,
}

/// `POST /v1/auth` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthRequest {
    /// Who is authenticating.
    pub account_id: AccountId,
    /// Signature over the server's nonce.
    pub nonce_signature: [u8; 64],
}

/// The lease a board sync replaces, proven by revealing its revoke key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseReplacement {
    /// The lease being replaced.
    pub lease_id: LeaseId,
    /// The revoke key whose SHA-256 is that lease's commitment.
    pub revoke_key: Redacted<[u8; 32]>,
}

/// Whether a board sync also returns the first page of the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadBoard {
    /// Return the first page (taken after the append).
    Read,
    /// Upload only (cover upload under `cover_read_ratio`, or "Low data").
    Skip,
}

/// `POST /v1/board/sync` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardSyncRequest {
    /// The new lease.
    pub lease: Lease,
    /// The lease it replaces, if any.
    pub replaces: Option<LeaseReplacement>,
    /// Whether to read the board.
    pub read: ReadBoard,
}

/// The answer to a board sync (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardSyncResponse {
    /// The id of the lease just stored.
    pub lease_id: LeaseId,
    /// The server's clock.
    pub server_time: UnixSeconds,
    /// The first page, if a read was asked for.
    pub page: Option<BoardPage>,
}

/// Implements the CBOR codec contract with stub bodies for each listed type.
macro_rules! cbor_stub {
    ($($name:ty),* $(,)?) => {
        $(
            impl WireEncode for $name {
                fn encode(&self) -> Result<Vec<u8>, ProtoError> {
                    not_implemented(self)
                }
            }

            impl WireDecode for $name {
                fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
                    not_implemented(bytes)
                }
            }
        )*
    };
}

cbor_stub!(
    Params,
    RegisterRequest,
    AuthRequest,
    BoardSyncRequest,
    BoardSyncResponse
);
