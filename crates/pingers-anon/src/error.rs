// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

/// Why an authorisation step failed. Never carries secrets, nullifiers or tokens.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AnonError {
    /// A message id must be below the message limit (8).
    #[error("message id out of range")]
    MessageIdOutOfRange,
    /// Bytes are not a canonical BN254 scalar field element.
    #[error("not a field element")]
    NotAFieldElement,
    /// The proof does not verify.
    #[error("proof does not verify")]
    BadProof,
    /// The proof is against a root the server does not accept (older than the previous hourly
    /// root, design B.4.8).
    #[error("proof is against an unaccepted root")]
    StaleRoot,
    /// The proof's `x` does not match the request body.
    #[error("proof does not bind this request")]
    WrongRequest,
    /// A tree update does not follow the local tree (a gap, or a position out of range).
    #[error("membership tree update out of order")]
    TreeOutOfOrder,
    /// The tree is full (depth 20).
    #[error("membership tree is full")]
    TreeFull,
    /// No leaf at this position.
    #[error("no such leaf")]
    NoSuchLeaf,
    /// The Privacy Pass issuer's response does not match the request or its proof fails.
    #[error("token issuance response is invalid")]
    BadIssuance,
    /// A token does not verify under any current issuer key.
    #[error("token does not verify")]
    BadToken,
    /// Proving or verification parameters failed to load.
    #[error("zero-knowledge parameters unavailable")]
    Parameters,
    /// A primitive reported an error the types should have ruled out.
    #[error("internal error")]
    Internal,
    /// This part of the contract has no implementation yet.
    #[error("not implemented")]
    NotImplemented,
}
