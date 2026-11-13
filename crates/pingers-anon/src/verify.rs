// SPDX-License-Identifier: AGPL-3.0-only
//! RLN verification and nullifier classification, server side (design B.4.8, D).

use pingers_proto::{RlnAuth, RlnScope, ServerId};
use pingers_time::Bucket;

use crate::stub::not_implemented;
use crate::{AnonError, MembershipRoot, Nullifier, RequestDigest};

/// The roots the server accepts: the current hourly root and the previous one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptedRoots {
    /// The current root.
    pub current: MembershipRoot,
    /// The previous root, if there has been one.
    pub previous: Option<MembershipRoot>,
}

/// A proof that verified, with what the nullifier set needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedProof {
    /// The scope spent.
    pub scope: RlnScope,
    /// The bucket spent.
    pub bucket: Bucket,
    /// The nullifier to look up.
    pub nullifier: Nullifier,
    /// What to store with it.
    pub record: NullifierRecord,
}

/// What the server keeps per nullifier: the share `(x, y)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NullifierRecord {
    /// The request digest.
    pub x: RequestDigest,
    /// `sk + a1 · x`, as a field element encoding.
    pub y: [u8; 32],
}

/// What a nullifier lookup means (design D).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum NullifierOutcome {
    /// Not seen: accept and store the record.
    Fresh,
    /// Seen with the same `x`: a retransmission; serve it again idempotently.
    Replay,
    /// Seen with a different `x`: reject with 403 (and slash, if the operator enabled it).
    Conflict,
}

/// Verifies `auth` for a request whose digest the server computed as `context.expected`.
///
/// Checks the proof, the root and `x`. Bucket acceptance (current, or previous within 60 s of rollover) is the caller's check.
///
/// # Errors
///
/// [`AnonError::BadProof`], [`AnonError::StaleRoot`], [`AnonError::WrongRequest`],
/// [`AnonError::Parameters`].
pub fn verify(
    auth: &RlnAuth,
    context: &VerificationContext<'_>,
) -> Result<VerifiedProof, AnonError> {
    not_implemented((auth, context))
}

/// What the server checks a proof against.
#[derive(Clone, Copy, Debug)]
pub struct VerificationContext<'context> {
    /// The roots it accepts now.
    pub roots: AcceptedRoots,
    /// This server's identity, which the external nullifier commits to.
    pub server: &'context ServerId,
    /// `request_digest` of the request as received.
    pub expected: RequestDigest,
}

/// Classifies a verified proof against what the nullifier set already holds for it.
pub fn classify_nullifier(
    existing: Option<&NullifierRecord>,
    proof: &VerifiedProof,
) -> NullifierOutcome {
    match existing {
        None => NullifierOutcome::Fresh,
        Some(record) if record.x == proof.record.x => NullifierOutcome::Replay,
        Some(_) => NullifierOutcome::Conflict,
    }
}
