// SPDX-License-Identifier: AGPL-3.0-only
//! RLN proof generation, main app only (design B.4.8).

use pingers_proto::{RlnAuth, RlnScope, ServerId};
use pingers_time::Bucket;
use rand_core::CryptoRng;

use crate::stub::not_implemented;
use crate::{AnonError, MembershipRoot, MerklePath, MessageId, RequestDigest, RlnSecret};

/// Everything one proof needs.
pub struct ProofRequest<'request> {
    /// The device's secret.
    pub secret: &'request RlnSecret,
    /// Its leaf's authentication path in the local tree.
    pub path: &'request MerklePath,
    /// The root the path leads to; must be one the server accepts.
    pub root: MembershipRoot,
    /// The server the request goes to.
    pub server: &'request ServerId,
    /// Which limit the request spends.
    pub scope: RlnScope,
    /// The current bucket.
    pub bucket: Bucket,
    /// The slot spent, already persisted as used.
    pub message_id: MessageId,
    /// `request_digest(method, path, body)` of the request being authorised.
    pub digest: RequestDigest,
}

/// Generates a proof and the `Authorization: PingersRln` header contents. Takes up to about a
/// second on a mid-range phone; run it off the async executor.
///
/// # Errors
///
/// [`AnonError::Parameters`] if the proving key fails to load; [`AnonError::NoSuchLeaf`] if
/// the path does not match the root; [`AnonError::Internal`].
pub fn prove(request: &ProofRequest<'_>, rng: &mut impl CryptoRng) -> Result<RlnAuth, AnonError> {
    not_implemented((request, rng))
}
