// SPDX-License-Identifier: AGPL-3.0-only
//! RLN verification and nullifier classification, server side (design B.4.8, D).

use pingers_proto::{RlnAuth, RlnScope, ServerId};
use pingers_time::Bucket;

use rln::circuit::Proof;
use rln::prelude::{CanonicalDeserialize as _, RLNProofValues, RLNZkProof as _};

use crate::circuit::BACKEND;
use crate::{AnonError, MembershipRoot, Nullifier, RequestDigest, external_nullifier, field};

/// The roots the server accepts: the current hourly root and the previous one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptedRoots {
    /// The current root.
    pub current: MembershipRoot,
    /// The previous root, if there has been one.
    pub previous: Option<MembershipRoot>,
}

impl AcceptedRoots {
    /// The roots before any rollover: only the first root.
    #[must_use]
    pub const fn first(root: MembershipRoot) -> Self {
        Self {
            current: root,
            previous: None,
        }
    }

    /// The roots after the hourly root epoch rolls over to `next`: the current root becomes
    /// the previous one and the old previous root stops being accepted (design B.4.8).
    #[must_use]
    pub const fn advance(self, next: MembershipRoot) -> Self {
        Self {
            current: next,
            previous: Some(self.current),
        }
    }

    /// Whether a proof against `root` is accepted.
    #[must_use]
    pub fn accepts(&self, root: &MembershipRoot) -> bool {
        self.current == *root || self.previous.as_ref() == Some(root)
    }
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
/// Checks the root, `x` and the proof, cheapest first. Bucket acceptance (current, or
/// previous within 60 s of rollover) is the caller's check, as is the nullifier lookup
/// ([`classify_nullifier`]).
///
/// # Errors
///
/// [`AnonError::StaleRoot`] if the root is neither the current nor the previous one;
/// [`AnonError::WrongRequest`] if `x` is not the digest of the request received;
/// [`AnonError::BadProof`] if a public input is not canonical, the proof bytes are not valid
/// curve points, or the proof does not verify; [`AnonError::Internal`].
pub fn verify(
    auth: &RlnAuth,
    context: &VerificationContext<'_>,
) -> Result<VerifiedProof, AnonError> {
    let root = MembershipRoot::from_bytes(auth.root);
    if !context.roots.accepts(&root) {
        return Err(AnonError::StaleRoot);
    }
    if auth.x != *context.expected.as_bytes() {
        return Err(AnonError::WrongRequest);
    }
    let public = |bytes: &[u8; 32]| field::decode(bytes).map_err(|_| AnonError::BadProof);
    let external = external_nullifier(context.server, auth.scope, auth.bucket)?;
    let values = RLNProofValues::new_single()
        .y(public(&auth.y)?)
        .root(public(&auth.root)?)
        .nullifier(public(&auth.nullifier)?)
        .x(public(&auth.x)?)
        .external_nullifier(field::decode(external.as_bytes())?)
        .build();
    // Compressed decoding checks that each point is on the curve and in the right subgroup.
    let proof =
        Proof::deserialize_compressed(auth.proof.as_slice()).map_err(|_| AnonError::BadProof)?;
    match BACKEND.verify(&proof, &values) {
        Ok(true) => Ok(VerifiedProof {
            scope: auth.scope,
            bucket: auth.bucket,
            nullifier: Nullifier::from_bytes(auth.nullifier),
            record: NullifierRecord {
                x: context.expected,
                y: auth.y,
            },
        }),
        Ok(false) | Err(_) => Err(AnonError::BadProof),
    }
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
