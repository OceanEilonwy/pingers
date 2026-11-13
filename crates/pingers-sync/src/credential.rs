// SPDX-License-Identifier: AGPL-3.0-only
//! Which anonymous credential a request carries, and making it (design B.4.8).
//!
//! RLN membership proofs are made only in the main app: proving needs memory and CPU a
//! background extension does not have, and proving in one process only removes the race on
//! the message-id counter. Everything else spends a pre-fetched Privacy Pass token.

use pingers_anon::{
    LeafPosition, LeafUpdate, MembershipTree, PrivacyPassToken, ProofRequest, prove, request_digest,
};
use pingers_ohttp::Header;
use pingers_proto::{AnonymousCredential, RlnAuth, RlnScope, ServerId, WireEncode};
use pingers_store::{HeldToken, RlnIdentity, Session, TreeLeaf};
use pingers_time::{Bucket, UnixSeconds};

use crate::request::AUTHORIZATION;
use crate::{SyncError, SyncRng, base64url};

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

/// Whether this device's prover may be used (design B.4.8 fallback triggers 2 and 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProverStatus {
    /// The self-test passed and nothing has switched proofs off.
    Enabled,
    /// The self-test failed, or the app's kill switch is on.
    Disabled,
}

/// What the server and this device allow for anonymous requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialPolicy {
    /// The credentials the server accepts (`Params.anonymous_credentials`).
    pub accepted: Vec<AnonymousCredential>,
    /// Whether the prover may be used.
    pub prover: ProverStatus,
}

/// Whether this device can make an RLN proof the server would accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RlnReadiness {
    /// An identity exists and its leaf is in an accepted root.
    Ready,
    /// No identity has been registered (`POST /v1/rln/leaf` not done).
    NoIdentity,
    /// The leaf is registered but not yet in an accepted root (up to an hour, design B.4.8
    /// fallback trigger 1).
    NotLiveYet,
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

/// Whether `identity` (if any) can prove at `now`.
pub(crate) fn readiness(identity: Option<&RlnIdentity>, now: UnixSeconds) -> RlnReadiness {
    match identity {
        None => RlnReadiness::NoIdentity,
        Some(identity) if identity.live_from.get() > root_epoch(now) => RlnReadiness::NotLiveYet,
        Some(_) => RlnReadiness::Ready,
    }
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
    let spendable = PrivacyPassToken::from_bytes(token.token.as_bytes().to_vec());
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
pub(crate) struct ProofInput<'input> {
    pub(crate) server: &'input ServerId,
    pub(crate) scope: RlnScope,
    pub(crate) bucket: Bucket,
    pub(crate) method: &'input str,
    pub(crate) path: &'input str,
    pub(crate) body: &'input [u8],
}

/// Makes a proof for one request, on the blocking pool: reserves (and persists) the next
/// message id for the scope and bucket first, so a crash between proving and sending can
/// never reuse it (design B.4.8, self-slashing risk). `None` if there is no identity or the
/// bucket's message ids are spent.
pub(crate) fn prove_request(
    session: &Session,
    rng: &mut SyncRng,
    input: &ProofInput<'_>,
) -> Result<Option<RlnAuth>, SyncError> {
    let Some(identity) = session.rln_identity().load()? else {
        return Ok(None);
    };
    let Some(message_id) = session.rln_counters().reserve(input.scope, input.bucket)? else {
        return Ok(None);
    };
    let tree = tree_from(session.rln_tree().leaves()?)?;
    let path = tree.path(LeafPosition::new(identity.leaf.get()))?;
    let digest = request_digest(input.method, input.path, input.body)?;
    let auth = prove(
        &ProofRequest {
            secret: &identity.secret,
            path: &path,
            root: tree.root(),
            server: input.server,
            scope: input.scope,
            bucket: input.bucket,
            message_id,
            digest,
        },
        rng,
    )?;
    Ok(Some(auth))
}

#[cfg(test)]
mod tests;
