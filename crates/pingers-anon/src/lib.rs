// SPDX-License-Identifier: AGPL-3.0-only
//! RLN proofs and Privacy Pass.
//!
//! Anonymous authorisation, both sides (architecture §3, design B.4.8):
//! - RLN-v2 (Groth16 over BN254, Poseidon tree of depth 20, message limit 8): identity,
//!   membership tree, witness and proof generation (feature `prover`), verification and
//!   nullifier classification (feature `verifier`). Slashing is off by default.
//! - Privacy Pass, token type 0x0001 (VOPRF, P-384, privately verifiable): token requests and
//!   finalisation (feature `prover`), issuance and redemption checks (feature `issuer`).
//!
//! Each binary enables only its side. Pure: the server's nullifier set and spent-token set live
//! in pingers-quota; the client's counters live in pingers-store.
#![forbid(unsafe_code)]

#[cfg(any(feature = "prover", feature = "verifier"))]
mod circuit;
mod error;
mod field;
mod identity;
#[cfg(feature = "issuer")]
mod issuer;
#[cfg(any(feature = "prover", feature = "issuer"))]
mod pp;
#[cfg(feature = "prover")]
mod prove;
#[cfg(feature = "prover")]
mod token;
#[cfg(any(feature = "prover", feature = "verifier"))]
mod tree;
#[cfg(feature = "verifier")]
mod verify;

pub use error::AnonError;
pub use identity::{
    ExternalNullifier, IdCommitment, LeafValue, MESSAGE_LIMIT, MembershipRoot, MessageId,
    Nullifier, RequestDigest, RlnSecret, external_nullifier, request_digest,
};
#[cfg(feature = "issuer")]
pub use issuer::{IssuerKey, batch_token_count, check_redemption, issue_tokens};
#[cfg(any(feature = "prover", feature = "issuer"))]
pub use pp::TokenCount;
#[cfg(feature = "prover")]
pub use prove::{ProofRequest, prove};
#[cfg(feature = "prover")]
pub use token::{PendingIssuance, PrivacyPassToken, begin_token_issuance};
#[cfg(any(feature = "prover", feature = "verifier"))]
pub use tree::{LeafPosition, LeafUpdate, MembershipTree, MerklePath};
#[cfg(feature = "verifier")]
pub use verify::{
    AcceptedRoots, NullifierOutcome, NullifierRecord, VerificationContext, VerifiedProof,
    classify_nullifier, classify_request, verify,
};
