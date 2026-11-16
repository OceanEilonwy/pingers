// SPDX-License-Identifier: AGPL-3.0-only
//! Which anonymous credential a request carries, and making it (design B.4.8).
//!
//! RLN membership proofs are made only in the main app: proving needs memory and CPU a
//! background extension does not have, and proving in one process only removes the race on
//! the message-id counter. The main app proves once its leaf is in a root the server accepts,
//! its tree copy is fresh enough, the prover self-test has passed, nothing has switched proofs
//! off, and (in a wake) there is time left for a proof. Everything else spends a pre-fetched
//! Privacy Pass token.

use core::time::Duration;

use pingers_anon::{LeafUpdate, MembershipTree, MerklePath, ProofRequest, prove, request_digest};
use pingers_ohttp::Header;
use pingers_proto::{
    AnonymousCredential, MembershipIndex, RlnAuth, RlnScope, ServerId, WireEncode,
};
use pingers_store::{HeldToken, RlnIdentity, Session, TreeLeaf, TreeState};
use pingers_time::{Bucket, UnixSeconds};
use rand_core::CryptoRng;

use crate::membership::root_accepted;
use crate::prover::{SelfTestStanding, standing};
use crate::request::AUTHORIZATION;
use crate::{MIN_REQUEST_TIME, SyncError, base64url};

/// Length of an RLN root epoch (design B.4.8: roots are hourly), as pingers-quota counts it.
const ROOT_EPOCH_SECONDS: u64 = 3_600;

/// Which process the engine runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProcessRole {
    /// The main app: may prove.
    MainApp,
    /// A background extension or short-lived handler (iOS Location Push or Notification
    /// Service Extension, an Android push handler with a few seconds): never proves.
    Extension,
}

/// Whether the app allows its prover (design B.4.8 fallback trigger 3, the app's kill switch).
/// The engine also requires a passed self-test (trigger 2) before proving.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProverStatus {
    /// The app does not stop the prover.
    Enabled,
    /// The app's kill switch is on.
    Disabled,
}

/// What the server and this app allow for anonymous requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialPolicy {
    /// The credentials the server accepts (`Params.anonymous_credentials`; replaced each time
    /// the parameters are fetched).
    pub accepted: Vec<AnonymousCredential>,
    /// Whether the app allows the prover.
    pub prover: ProverStatus,
}

/// Whether this device can make an RLN proof the server would accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RlnReadiness {
    /// An identity exists, its leaf is in the tree copy, and the copy's root is accepted.
    Ready,
    /// No identity has been registered (`POST /v1/rln/leaf` not done).
    NoIdentity,
    /// The leaf is registered but not yet in an accepted root (up to an hour, design B.4.8
    /// fallback trigger 1).
    NotLiveYet,
    /// The tree copy is missing, older than the previous hourly root, or older than the leaf:
    /// refresh it before proving.
    TreeStale,
    /// A fresh copy no longer holds this device's leaf (zeroed: the account was deleted or
    /// banned, or the leaf replaced): register a new one.
    LeafRemoved,
}

/// The credential a request will carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CredentialChoice {
    /// An RLN membership proof, made now.
    MembershipProof,
    /// A pre-fetched Privacy Pass token.
    PrivacyPass,
}

/// Chooses the credential for one anonymous request: a proof when this is the main app, the
/// server accepts proofs, the prover is enabled and the leaf is live; otherwise a Privacy Pass
/// token if the server accepts them.
///
/// # Errors
///
/// [`SyncError::NoCredential`] if the server accepts nothing this process can make.
pub fn choose_credential(
    role: ProcessRole,
    policy: &CredentialPolicy,
    readiness: RlnReadiness,
) -> Result<CredentialChoice, SyncError> {
    let proofs = role == ProcessRole::MainApp
        && policy.prover == ProverStatus::Enabled
        && readiness == RlnReadiness::Ready
        && policy
            .accepted
            .contains(&AnonymousCredential::MembershipProof);
    if proofs {
        Ok(CredentialChoice::MembershipProof)
    } else if policy.accepted.contains(&AnonymousCredential::PrivacyPass) {
        Ok(CredentialChoice::PrivacyPass)
    } else {
        Err(SyncError::NoCredential)
    }
}

/// The root epoch containing `now`.
pub(crate) const fn root_epoch(now: UnixSeconds) -> u64 {
    now.get() / ROOT_EPOCH_SECONDS
}

/// Whether `identity` (if any) can prove at `now` against the tree copy `tree`.
pub(crate) fn readiness(
    identity: Option<&RlnIdentity>,
    tree: Option<&TreeState>,
    now: UnixSeconds,
) -> RlnReadiness {
    let Some(identity) = identity else {
        return RlnReadiness::NoIdentity;
    };
    if u64::from(identity.live_from.get()) > root_epoch(now) {
        return RlnReadiness::NotLiveYet;
    }
    let Some(tree) = tree.filter(|tree| root_accepted(tree, now)) else {
        return RlnReadiness::TreeStale;
    };
    if tree.root_epoch < identity.live_from {
        return RlnReadiness::TreeStale;
    }
    match tree.path {
        Some(path) if path.position == identity.leaf => RlnReadiness::Ready,
        Some(_) | None => RlnReadiness::LeafRemoved,
    }
}

/// What the store says about proving now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProvingState {
    pub(crate) readiness: RlnReadiness,
    pub(crate) self_test: SelfTestStanding,
}

impl ProvingState {
    /// Reads it in one store step.
    pub(crate) fn load(session: &Session, now: UnixSeconds) -> Result<Self, SyncError> {
        Ok(Self {
            readiness: readiness(
                session.rln_identity().load()?.as_ref(),
                session.rln_tree_state().load()?.as_ref(),
                now,
            ),
            self_test: standing(session.prover_self_test().load()?.as_ref(), now),
        })
    }

    /// The policy with the app's switch combined with the self-test: the prover is enabled
    /// only once a self-test has passed.
    pub(crate) fn effective(self, policy: &CredentialPolicy) -> CredentialPolicy {
        let tested = matches!(self.self_test, SelfTestStanding::Passed { .. });
        CredentialPolicy {
            accepted: policy.accepted.clone(),
            prover: if tested {
                policy.prover
            } else {
                ProverStatus::Disabled
            },
        }
    }

    /// How long a proof may take here (the self-test's 95th percentile), if the prover passed.
    pub(crate) const fn proof_time(self) -> Option<Duration> {
        match self.self_test {
            SelfTestStanding::Passed { p95 } => Some(p95),
            SelfTestStanding::Failed | SelfTestStanding::Due => None,
        }
    }
}

/// Whether a proof still fits a wake with `remaining` left: the proof (at the self-test's 95th
/// percentile) and then the request itself. Without a deadline there is always time.
pub(crate) fn proof_fits(remaining: Option<Duration>, proof_time: Duration) -> bool {
    remaining.is_none_or(|remaining| remaining >= proof_time.saturating_add(MIN_REQUEST_TIME))
}

/// An attached credential, and what to undo if the request is then not sent.
pub(crate) enum Attached {
    /// The route needs no credential.
    Nothing,
    /// A Privacy Pass token, already removed from the store; put back if nothing was sent.
    Token { header: Header, token: HeldToken },
    /// An RLN proof; its message id is spent either way.
    Proof { header: Header },
}

impl Attached {
    pub(crate) fn header(&self) -> Option<Header> {
        match self {
            Self::Nothing => None,
            Self::Token { header, .. } | Self::Proof { header } => Some(header.clone()),
        }
    }
}

/// The authorization header spending `token`.
pub(crate) fn token_header(token: &HeldToken) -> Header {
    let spendable = pingers_anon::PrivacyPassToken::from_bytes(token.token.as_bytes().to_vec());
    Header {
        name: AUTHORIZATION.to_owned(),
        value: spendable.authorization().as_bytes().to_vec(),
    }
}

/// The authorization header carrying `auth`.
pub(crate) fn proof_header(auth: &RlnAuth) -> Result<Header, SyncError> {
    let encoded = auth.encode().map_err(SyncError::Encode)?;
    Ok(Header {
        name: AUTHORIZATION.to_owned(),
        value: format!("PingersRln {}", base64url::encode(&encoded)).into_bytes(),
    })
}

/// The membership tree from the stored leaf log, which must be contiguous from position 0.
pub(crate) fn tree_from(mut leaves: Vec<TreeLeaf>) -> Result<MembershipTree, SyncError> {
    leaves.sort_by_key(|leaf| leaf.position.get());
    let contiguous = leaves
        .iter()
        .zip(0_u32..)
        .all(|(leaf, expected)| leaf.position.get() == expected);
    if !contiguous {
        return Err(SyncError::Anon(pingers_anon::AnonError::TreeOutOfOrder));
    }
    let updates: Vec<LeafUpdate> = leaves
        .iter()
        .map(|leaf| LeafUpdate::Appended(leaf.value))
        .collect();
    let mut tree = MembershipTree::empty()?;
    tree.apply(&updates)?;
    Ok(tree)
}

/// What a proof is made over.
pub(crate) struct ProofInput {
    pub(crate) server: ServerId,
    pub(crate) scope: RlnScope,
    pub(crate) bucket: Bucket,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) body: Vec<u8>,
}

/// A message id reserved for one proof, with the identity, path and root to prove with.
pub(crate) struct Reservation {
    identity: RlnIdentity,
    path: MerklePath,
    root: pingers_anon::MembershipRoot,
    message_id: pingers_anon::MessageId,
}

/// Reserves (and persists) the next message id for the input's scope and bucket, so a crash
/// between proving and sending can never reuse it (design B.4.8, self-slashing risk). `None` if
/// the device cannot prove now or the bucket's message ids are spent.
pub(crate) fn reserve_proof(
    session: &Session,
    input: &ProofInput,
    now: UnixSeconds,
) -> Result<Option<Reservation>, SyncError> {
    let Some(identity) = session.rln_identity().load()? else {
        return Ok(None);
    };
    let Some(tree) = session.rln_tree_state().load()? else {
        return Ok(None);
    };
    if readiness(Some(&identity), Some(&tree), now) != RlnReadiness::Ready {
        return Ok(None);
    }
    let Some(stored) = tree.path else {
        return Ok(None);
    };
    let Some(message_id) = session.rln_counters().reserve(input.scope, input.bucket)? else {
        return Ok(None);
    };
    let path = MerklePath::from_sibling_bytes(
        MembershipIndex::new(stored.position.get()),
        &stored.siblings,
    )?;
    Ok(Some(Reservation {
        identity,
        path,
        root: tree.root,
        message_id,
    }))
}

/// Makes the proof for a reservation. Takes up to about a second on a mid-range phone: call it
/// on the blocking pool, without a store session open.
pub(crate) fn prove_reserved(
    reservation: &Reservation,
    input: &ProofInput,
    rng: &mut impl CryptoRng,
) -> Result<RlnAuth, SyncError> {
    let digest = request_digest(&input.method, &input.path, &input.body)?;
    Ok(prove(
        &ProofRequest {
            secret: &reservation.identity.secret,
            path: &reservation.path,
            root: reservation.root,
            server: &input.server,
            scope: input.scope,
            bucket: input.bucket,
            message_id: reservation.message_id,
            digest,
        },
        rng,
    )?)
}

#[cfg(test)]
mod tests;
