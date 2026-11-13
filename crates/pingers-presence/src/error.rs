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
    /// The Spot keys given are not those of the Spot and epoch the record matched under.
    #[error("spot keys do not belong to the matched record")]
    SpotMismatch,
    /// A knock answers a record that is not one of this device's live records.
    #[error("knock does not answer one of our records")]
    KnockForOtherRecord,
    /// A knock names a Party epoch other than the one its record's keys belong to.
    #[error("knock is for another epoch")]
    WrongEpoch,
    /// The knocker's leaf is not in the Party roster at the knock's epoch, or has since been
    /// removed or replaced.
    #[error("knocker is not in the roster")]
    UnknownKnocker,
    /// A lease's start bucket is not acceptable now (server side).
    #[error("lease start bucket is not current")]
    LeaseNotCurrent,
    /// A lease carries the same record randomness twice (server side).
    #[error("lease repeats a record")]
    DuplicateRecord,
    /// A mailbox registration or post breaks the posting rules (server side).
    #[error("mailbox request breaks the posting rules")]
    MailboxRule,
    /// A cryptographic step failed (including signature checks and failed opens).
    #[error("cryptographic step failed")]
    Crypto(#[from] pingers_crypto::CryptoError),
    /// Bytes could not be encoded or decoded.
    #[error("encoding failed")]
    Proto(#[from] pingers_proto::ProtoError),
    /// A step the types should have ruled out failed (style guide §5).
    #[error("internal error")]
    Internal,
}
