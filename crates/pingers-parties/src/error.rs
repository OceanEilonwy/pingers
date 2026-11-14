// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

use pingers_crypto::CryptoError;
use pingers_geo::GeoError;
use pingers_mls::{MlsError, StorageError};
use pingers_proto::ProtoError;
use pingers_store::StoreError;

/// What went wrong with a Party operation. Never carries names, coordinates, identifiers or
/// secrets (style guide §9).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PartiesError {
    /// The store failed.
    #[error("storage failed")]
    Store(#[from] StoreError),
    /// This crate's own records in the store's key-value space failed.
    #[error("party record storage failed")]
    Records(#[from] StorageError),
    /// The MLS group operation failed.
    #[error("MLS operation failed")]
    Mls(#[from] MlsError),
    /// A seal or open failed (for example a join request not sealed with the invite's secret).
    #[error("cryptographic operation failed")]
    Crypto(#[from] CryptoError),
    /// A wire value did not encode or decode.
    #[error("wire encoding failed")]
    Proto(#[from] ProtoError),
    /// A Spot's geometry is invalid.
    #[error("invalid Spot geometry")]
    Geo(#[from] GeoError),
    /// No account exists on this device yet.
    #[error("no account on this device")]
    NoAccount,
    /// The Party's server has no key pins on this device, so an invite could not tell a
    /// joiner which keys to expect.
    #[error("the Party's server has no key pins")]
    NoServerPins,
    /// No such Party on this device.
    #[error("unknown Party")]
    UnknownParty,
    /// No such invite.
    #[error("unknown invite")]
    UnknownInvite,
    /// No join request waits under this id.
    #[error("unknown join request")]
    UnknownJoin,
    /// No such Spot in the Party.
    #[error("unknown Spot")]
    UnknownSpot,
    /// No such member in the Party.
    #[error("unknown member")]
    UnknownMember,
    /// The mailbox pool has no mailbox for this purpose; pingers-sync must register more.
    #[error("no pooled mailbox available")]
    NoPooledMailbox,
    /// The invite link has expired.
    #[error("the invite has expired")]
    InviteExpired,
    /// The invite link is not a Pingers invite link.
    #[error("not a Pingers invite link")]
    InvalidLink,
    /// The Welcome's group contains no member whose key matches the link's inviter
    /// fingerprint: someone other than the inviter answered (design D, invite flow).
    #[error("the group does not contain the inviter")]
    InviterNotInGroup,
    /// The safety code entered does not match the one computed from both keys.
    #[error("the safety code does not match")]
    SafetyCodeMismatch,
    /// A circular Spot is smaller than `pingers_geo::MINIMUM_SPOT_RADIUS`.
    #[error("the Spot is smaller than the minimum radius")]
    SpotTooSmall,
    /// The Spot has been deleted; deleted Spots cannot be edited.
    #[error("the Spot has been deleted")]
    SpotDeleted,
    /// This device already has a commit waiting for the server's answer in this Party.
    #[error("a commit is already waiting for the server")]
    CommitInFlight,
    /// No commit of this device waits for the server in this Party.
    #[error("no commit is waiting for the server")]
    NoPendingCommit,
    /// The state snapshot is missing a field, or an operation from another member does not
    /// decode.
    #[error("malformed {what}")]
    Malformed {
        /// What did not decode.
        what: &'static str,
    },
    /// A value this crate computes itself failed (for example a counter overflow).
    #[error("internal error")]
    Internal,
}
