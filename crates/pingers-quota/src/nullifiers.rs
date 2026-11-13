// SPDX-License-Identifier: AGPL-3.0-only
//! RLN admission: verify a proof, check its bucket, and spend its nullifier (design B.4.8, D).
//!
//! The per-bucket limit (8 board syncs, 8 mailbox operations, 8 group sessions per device) is
//! enforced by the circuit: a device has exactly [`pingers_anon::MESSAGE_LIMIT`] message slots
//! per scope and bucket, each giving one nullifier. The server's part is to accept each
//! nullifier once. The set lives in memory only (design C.1) and keeps the current and the
//! previous bucket.

use core::time::Duration;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use pingers_anon::{
    NullifierOutcome, NullifierRecord, RequestDigest, VerificationContext, classify_nullifier,
    verify,
};
use pingers_proto::{RlnAuth, ServerId};
use pingers_time::{Bucket, Clock};

use crate::QuotaError;
use crate::membership::Membership;
use crate::store::MembershipStore;

/// After a bucket rollover, proofs for the previous bucket are still accepted for this long
/// (design D, `RlnAuth.bucket`).
pub const BUCKET_GRACE: Duration = Duration::from_secs(60);

/// What an accepted proof means for the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Admission {
    /// A new nullifier: process the request.
    Fresh,
    /// The same request again (same nullifier, same `x`): a retransmission, serve it again
    /// idempotently.
    Replay,
}

/// Keyed by bucket only: the external nullifier already commits to the scope, so the same
/// nullifier cannot arise in two scopes.
type Sets = BTreeMap<Bucket, HashMap<[u8; 32], NullifierRecord>>;

/// The nullifier sets of the current and previous bucket, per scope.
pub struct Nullifiers {
    clock: Arc<dyn Clock>,
    server: ServerId,
    sets: Mutex<Sets>,
}

impl Nullifiers {
    /// An empty set for proofs addressed to `server`.
    #[must_use]
    pub fn new(server: ServerId, clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            server,
            sets: Mutex::new(BTreeMap::new()),
        }
    }

    /// Checks an `Authorization: PingersRln` header for a request whose digest the server
    /// computed as `expected`: bucket, root, `x`, proof, then the nullifier.
    ///
    /// # Errors
    ///
    /// [`QuotaError::StaleBucket`]; [`QuotaError::Anon`] with [`pingers_anon::AnonError::StaleRoot`],
    /// `WrongRequest` or `BadProof`; [`QuotaError::NullifierReused`] when the nullifier was
    /// spent on a different request (403; no slashing, the share is not kept).
    pub fn admit<S: MembershipStore>(
        &self,
        membership: &Membership<S>,
        auth: &RlnAuth,
        expected: RequestDigest,
    ) -> Result<Admission, QuotaError> {
        let now = self.clock.now();
        let current = now.bucket();
        let in_grace = now.saturating_duration_since(current.start()) < BUCKET_GRACE;
        let accepted =
            auth.bucket == current || (in_grace && current.previous() == Some(auth.bucket));
        if !accepted {
            return Err(QuotaError::StaleBucket);
        }
        let proof = verify(
            auth,
            &VerificationContext {
                roots: membership.accepted_roots()?,
                server: &self.server,
                expected,
            },
        )?;
        let mut sets = self.sets.lock().map_err(|_| QuotaError::Poisoned)?;
        let oldest = current.previous().unwrap_or(current);
        sets.retain(|bucket, _| *bucket >= oldest);
        let set = sets.entry(proof.bucket).or_default();
        let key = *proof.nullifier.as_bytes();
        match classify_nullifier(set.get(&key), &proof) {
            NullifierOutcome::Fresh => {
                set.insert(key, proof.record);
                Ok(Admission::Fresh)
            }
            NullifierOutcome::Replay => Ok(Admission::Replay),
            // `NullifierOutcome` is non-exhaustive: an outcome this server doesn't know is
            // rejected like a conflict, never admitted.
            NullifierOutcome::Conflict | _ => Err(QuotaError::NullifierReused),
        }
    }

    /// How many nullifiers are held, for metrics (a count only).
    ///
    /// # Errors
    ///
    /// [`QuotaError::Poisoned`].
    pub fn tracked(&self) -> Result<usize, QuotaError> {
        let sets = self.sets.lock().map_err(|_| QuotaError::Poisoned)?;
        Ok(sets.values().map(HashMap::len).sum())
    }
}
