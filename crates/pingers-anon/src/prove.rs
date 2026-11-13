// SPDX-License-Identifier: AGPL-3.0-only
//! RLN proof generation, main app only (design B.4.8).

use ark_ff::Field as _;
use pingers_proto::{RlnAuth, RlnScope, ServerId};
use pingers_time::Bucket;
use rand_core::CryptoRng;
use rln::circuit::{Fr, G1Affine, G2Affine, Proof};
use rln::prelude::{CanonicalSerialize as _, RLNMerkleProof, RLNWitnessInput, RLNZkProof as _};

use crate::circuit::BACKEND;
use crate::{
    AnonError, MESSAGE_LIMIT, MembershipRoot, MerklePath, MessageId, RequestDigest, RlnSecret,
    external_nullifier, field,
};

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
/// `rln` draws the Groth16 blinding scalars from its own thread-local CSPRNG; the proof is then
/// re-randomised with scalars from `rng`, so its zero-knowledge never rests on that generator
/// alone.
///
/// # Errors
///
/// [`AnonError::Parameters`] if the proving key fails to load; [`AnonError::NoSuchLeaf`] if
/// the path does not match the root; [`AnonError::NotAFieldElement`] if the root or digest is
/// not canonical; [`AnonError::Internal`].
pub fn prove(request: &ProofRequest<'_>, rng: &mut impl CryptoRng) -> Result<RlnAuth, AnonError> {
    let secret = request.secret.scalar()?;
    let root = field::decode(request.root.as_bytes())?;
    let x = field::decode(request.digest.as_bytes())?;
    let limit = Fr::from(MESSAGE_LIMIT);
    let id_commitment = field::poseidon(&[*secret]);
    let leaf = field::poseidon(&[id_commitment, limit]);
    if request.path.root_from(leaf) != root {
        return Err(AnonError::NoSuchLeaf);
    }
    let external = external_nullifier(request.server, request.scope, request.bucket)?;
    let witness = RLNWitnessInput::new_single()
        .identity_secret(secret)
        .user_message_limit(limit)
        .merkle_proof(RLNMerkleProof::new(
            request.path.siblings().to_vec(),
            request.path.index_bits(),
        ))
        .x(x)
        .external_nullifier(field::decode(external.as_bytes())?)
        .message_id(Fr::from(request.message_id.get()))
        .build()
        .map_err(|_| AnonError::Internal)?;
    let (proof, values) = BACKEND
        .generate_proof(&witness)
        .map_err(|_| AnonError::Internal)?;
    let proof = rerandomise(&proof, rng)?;
    let mut encoded = Vec::with_capacity(RLN_PROOF_SIZE);
    proof
        .serialize_compressed(&mut encoded)
        .map_err(|_| AnonError::Internal)?;
    Ok(RlnAuth {
        root: *request.root.as_bytes(),
        scope: request.scope,
        bucket: request.bucket,
        x: *request.digest.as_bytes(),
        y: field::encode(values.y().ok_or(AnonError::Internal)?),
        nullifier: field::encode(values.nullifier().ok_or(AnonError::Internal)?),
        proof: encoded.try_into().map_err(|_| AnonError::Internal)?,
    })
}

/// The compressed size of a BN254 Groth16 proof.
const RLN_PROOF_SIZE: usize = 128;

/// Re-randomises a Groth16 proof: `A' = A / r1`, `B' = r1·B + r1·r2·δ`, `C' = C + r2·A`.
/// The result verifies exactly when the input does and is distributed independently of it.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "curve and field arithmetic is modular and cannot overflow"
)]
fn rerandomise(proof: &Proof, rng: &mut impl CryptoRng) -> Result<Proof, AnonError> {
    let r1 = random_scalar(rng);
    let r2 = random_scalar(rng);
    let r1_inverse = r1.inverse().ok_or(AnonError::Internal)?;
    let delta = rln::circuit::default_zkey_single().0.vk.delta_g2;
    Ok(Proof {
        a: G1Affine::from(proof.a * r1_inverse),
        b: G2Affine::from(proof.b * r1 + delta * (r1 * r2)),
        c: G1Affine::from(proof.a * r2 + proof.c),
    })
}

/// A uniformly random scalar (64 bytes reduced: bias below 2^-250).
fn random_scalar(rng: &mut impl CryptoRng) -> Fr {
    let mut wide = zeroize::Zeroizing::new([0_u8; 64]);
    rng.fill_bytes(wide.as_mut_slice());
    field::reduce(wide.as_slice())
}
