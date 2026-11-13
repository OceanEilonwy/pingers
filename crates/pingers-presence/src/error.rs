// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

/// Why a presence protocol step failed. Never carries identifiers, keys or coordinates.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PresenceError {
    /// Spot keys may only be derived while the Spot is `Inside` (design B.4.3).
    #[error("spot is not Inside")]
    NotInside,
    /// A lease holds at most four real slots; choose them with `choose_slots`.
    #[error("more than four real slots")]
    TooManySlots,
    /// A knock answers a record that is not one of this device's live records.
    #[error("knock does not answer one of our records")]
    KnockForOtherRecord,
    /// The knocker's leaf is not in the Party roster at the knock's epoch.
    #[error("knocker is not in the roster")]
    UnknownKnocker,
    /// A lease's start bucket is not acceptable now (server side).
    #[error("lease start bucket is not current")]
    LeaseNotCurrent,
    /// A mailbox registration or post breaks the posting rules (server side).
    #[error("mailbox request breaks the posting rules")]
    MailboxRule,
    /// A cryptographic step failed (including signature checks and failed opens).
    #[error("cryptographic step failed")]
    Crypto(#[from] pingers_crypto::CryptoError),
    /// Bytes could not be encoded or decoded.
    #[error("encoding failed")]
    Proto(#[from] pingers_proto::ProtoError),
    /// This part of the contract has no implementation yet.
    #[error("not implemented")]
    NotImplemented,
}
