// SPDX-License-Identifier: AGPL-3.0-only
//! RLN end to end: a client proves against its local tree, the server verifies (design B.4.8).
#![cfg(all(feature = "prover", feature = "verifier"))]
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::sync::LazyLock;

use pingers_anon::{
    AcceptedRoots, AnonError, LeafPosition, LeafUpdate, LeafValue, MembershipRoot, MembershipTree,
    MessageId, NullifierOutcome, ProofRequest, RequestDigest, RlnSecret, VerificationContext,
    VerifiedProof, classify_nullifier, classify_request, prove, request_digest, verify,
};
use pingers_proto::{RlnAuth, RlnScope, ServerId};
use pingers_testkit::seeded_rng;
use pingers_time::Bucket;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Anon(#[from] AnonError),
    #[error("fixture: {0}")]
    Fixture(&'static str),
}

/// Where our device sits among the other leaves.
const OUR_POSITION: u32 = 3;

/// A client with a registered device, its local tree, and the server it talks to.
struct World {
    secret: RlnSecret,
    tree: MembershipTree,
    server: ServerId,
    bucket: Bucket,
}

impl World {
    fn new() -> Result<Self, TestError> {
        let mut rng = seeded_rng(7);
        let secret = RlnSecret::generate(&mut rng)?;
        let our_leaf = secret.id_commitment()?.leaf()?;
        let mut tree = MembershipTree::empty()?;
        let mut updates = Vec::new();
        for _ in 0..OUR_POSITION {
            let other = RlnSecret::generate(&mut rng)?;
            updates.push(LeafUpdate::Appended(other.id_commitment()?.leaf()?));
        }
        updates.push(LeafUpdate::Appended(our_leaf));
        updates.push(LeafUpdate::Appended(LeafValue::from_bytes([5; 32])));
        tree.apply(&updates)?;
        Ok(Self {
            secret,
            tree,
            server: ServerId::new("https://pingers.example".to_owned())
                .map_err(|_| TestError::Fixture("server id"))?,
            bucket: Bucket::from_index(1_900_000).map_err(|_| TestError::Fixture("bucket"))?,
        })
    }

    fn prove(&self, body: &[u8], message_id: u8, seed: u64) -> Result<Proved, TestError> {
        let path = self.tree.path(LeafPosition::new(OUR_POSITION))?;
        let digest = request_digest("POST", "/v1/board/sync", body)?;
        let auth = prove(
            &ProofRequest {
                secret: &self.secret,
                path: &path,
                root: self.tree.root(),
                server: &self.server,
                scope: RlnScope::Board,
                bucket: self.bucket,
                message_id: MessageId::new(message_id)?,
                digest,
            },
            &mut seeded_rng(seed),
        )?;
        Ok(Proved { auth, digest })
    }

    fn context(&self, roots: AcceptedRoots, expected: RequestDigest) -> VerificationContext<'_> {
        VerificationContext {
            roots,
            server: &self.server,
            expected,
        }
    }

    fn accepts(&self, proved: &Proved) -> Result<VerifiedProof, AnonError> {
        verify(
            &proved.auth,
            &self.context(AcceptedRoots::first(self.tree.root()), proved.digest),
        )
    }
}

/// A proof and the digest of the request it authorises.
#[derive(Clone, Copy)]
struct Proved {
    auth: RlnAuth,
    digest: RequestDigest,
}

/// Proving takes a while in a debug build, so the proofs most tests share are made once.
struct Shared {
    world: World,
    /// Body "a", message id 0.
    first: Proved,
    /// Body "a", message id 0 again, with different blinding: a retransmission.
    retransmitted: Proved,
    /// Body "b", message id 0: a reused slot.
    reused_slot: Proved,
    /// Body "b", message id 1.
    next_slot: Proved,
}

static SHARED: LazyLock<Result<Shared, String>> = LazyLock::new(|| {
    let build = || -> Result<Shared, TestError> {
        let world = World::new()?;
        Ok(Shared {
            first: world.prove(b"a", 0, 1)?,
            retransmitted: world.prove(b"a", 0, 2)?,
            reused_slot: world.prove(b"b", 0, 3)?,
            next_slot: world.prove(b"b", 1, 4)?,
            world,
        })
    };
    build().map_err(|error| error.to_string())
});

fn shared() -> Result<&'static Shared, TestError> {
    SHARED
        .as_ref()
        .map_err(|_| TestError::Fixture("shared proofs failed"))
}

/// A copy of `auth` with one byte of `field` flipped.
fn tampered(auth: &RlnAuth, field: fn(&mut RlnAuth) -> &mut [u8], index: usize) -> RlnAuth {
    let mut copy = *auth;
    if let Some(byte) = field(&mut copy).get_mut(index) {
        *byte ^= 0x01;
    }
    copy
}

#[test]
fn a_proof_verifies_and_names_its_nullifier() -> Result<(), TestError> {
    let shared = shared()?;
    let verified = shared.world.accepts(&shared.first)?;
    assert_eq!(verified.scope, RlnScope::Board);
    assert_eq!(verified.bucket, shared.world.bucket);
    assert_eq!(verified.nullifier.as_bytes(), &shared.first.auth.nullifier);
    assert_eq!(verified.record.x, shared.first.digest);
    assert_eq!(verified.record.y, shared.first.auth.y);
    assert_eq!(shared.first.auth.root, *shared.world.tree.root().as_bytes());
    Ok(())
}

#[test]
fn proofs_are_rerandomised_but_bind_the_same_values() -> Result<(), TestError> {
    let shared = shared()?;
    let (first, again) = (shared.first.auth, shared.retransmitted.auth);
    assert_ne!(first.proof, again.proof);
    assert_eq!(first.nullifier, again.nullifier);
    assert_eq!(first.y, again.y);
    shared.world.accepts(&shared.retransmitted)?;
    Ok(())
}

#[test]
fn a_tampered_proof_is_rejected() -> Result<(), TestError> {
    let shared = shared()?;
    for index in [0, 40, 100, 127] {
        let auth = tampered(&shared.first.auth, |auth| &mut auth.proof, index);
        let result = shared.world.accepts(&Proved {
            auth,
            ..shared.first
        });
        assert!(
            matches!(result, Err(AnonError::BadProof)),
            "proof byte {index}"
        );
    }
    Ok(())
}

#[test]
fn tampered_public_inputs_are_rejected() -> Result<(), TestError> {
    let shared = shared()?;
    let y = tampered(&shared.first.auth, |auth| &mut auth.y, 0);
    let nullifier = tampered(&shared.first.auth, |auth| &mut auth.nullifier, 0);
    let mut scope = shared.first.auth;
    scope.scope = RlnScope::Mailbox;
    let mut bucket = shared.first.auth;
    bucket.bucket = Bucket::from_index(1_900_001).map_err(|_| TestError::Fixture("bucket"))?;
    for auth in [y, nullifier, scope, bucket] {
        let result = shared.world.accepts(&Proved {
            auth,
            ..shared.first
        });
        assert!(matches!(result, Err(AnonError::BadProof)));
    }
    Ok(())
}

#[test]
fn a_non_canonical_public_input_is_a_bad_proof() -> Result<(), TestError> {
    let shared = shared()?;
    let mut auth = shared.first.auth;
    auth.y = [0xff; 32];
    let result = shared.world.accepts(&Proved {
        auth,
        ..shared.first
    });
    assert!(matches!(result, Err(AnonError::BadProof)));
    Ok(())
}

#[test]
fn a_proof_for_another_server_is_rejected() -> Result<(), TestError> {
    let shared = shared()?;
    let other = ServerId::new("https://other.example".to_owned())
        .map_err(|_| TestError::Fixture("server id"))?;
    let context = VerificationContext {
        server: &other,
        ..shared.world.context(
            AcceptedRoots::first(shared.world.tree.root()),
            shared.first.digest,
        )
    };
    assert!(matches!(
        verify(&shared.first.auth, &context),
        Err(AnonError::BadProof)
    ));
    Ok(())
}

#[test]
fn a_proof_for_another_request_is_rejected() -> Result<(), TestError> {
    let shared = shared()?;
    let other = request_digest("POST", "/v1/board/sync", b"other body")?;
    let context = shared
        .world
        .context(AcceptedRoots::first(shared.world.tree.root()), other);
    assert!(matches!(
        verify(&shared.first.auth, &context),
        Err(AnonError::WrongRequest)
    ));
    Ok(())
}

#[test]
fn the_previous_root_is_accepted_and_older_ones_are_stale() -> Result<(), TestError> {
    let shared = shared()?;
    let proved_against = shared.world.tree.root();
    let mut later = shared.world.tree.clone();
    later.apply(&[LeafUpdate::Appended(LeafValue::from_bytes([6; 32]))])?;
    let next_hour = AcceptedRoots::first(proved_against).advance(later.root());
    assert_eq!(next_hour.previous, Some(proved_against));
    verify(
        &shared.first.auth,
        &shared.world.context(next_hour, shared.first.digest),
    )?;
    let mut latest = later.clone();
    latest.apply(&[LeafUpdate::Appended(LeafValue::from_bytes([7; 32]))])?;
    let hour_after = next_hour.advance(latest.root());
    assert!(matches!(
        verify(
            &shared.first.auth,
            &shared.world.context(hour_after, shared.first.digest)
        ),
        Err(AnonError::StaleRoot)
    ));
    Ok(())
}

#[test]
fn a_reused_slot_in_the_same_bucket_conflicts() -> Result<(), TestError> {
    let shared = shared()?;
    let stored = shared.world.accepts(&shared.first)?;
    assert_eq!(classify_nullifier(None, &stored), NullifierOutcome::Fresh);
    let reused = shared.world.accepts(&shared.reused_slot)?;
    assert_eq!(reused.nullifier, stored.nullifier);
    assert_eq!(
        classify_nullifier(Some(&stored.record), &reused),
        NullifierOutcome::Conflict
    );
    // A server that keeps only the digest (slashing off) reaches the same answer.
    assert_eq!(classify_request(None, &stored), NullifierOutcome::Fresh);
    assert_eq!(
        classify_request(Some(&stored.record.x), &reused),
        NullifierOutcome::Conflict
    );
    Ok(())
}

#[test]
fn a_retransmission_is_a_replay() -> Result<(), TestError> {
    let shared = shared()?;
    let stored = shared.world.accepts(&shared.first)?;
    let again = shared.world.accepts(&shared.retransmitted)?;
    assert_eq!(
        classify_nullifier(Some(&stored.record), &again),
        NullifierOutcome::Replay
    );
    assert_eq!(
        classify_request(Some(&stored.record.x), &again),
        NullifierOutcome::Replay
    );
    Ok(())
}

#[test]
fn the_next_slot_has_a_fresh_nullifier() -> Result<(), TestError> {
    let shared = shared()?;
    let first = shared.world.accepts(&shared.first)?;
    let next = shared.world.accepts(&shared.next_slot)?;
    assert_ne!(first.nullifier, next.nullifier);
    Ok(())
}

#[test]
fn a_path_that_does_not_reach_the_root_is_refused() -> Result<(), TestError> {
    let world = World::new()?;
    let path = world
        .tree
        .path(LeafPosition::new(OUR_POSITION.saturating_sub(1)))?;
    let request = ProofRequest {
        secret: &world.secret,
        path: &path,
        root: world.tree.root(),
        server: &world.server,
        scope: RlnScope::Group,
        bucket: world.bucket,
        message_id: MessageId::new(0)?,
        digest: request_digest("GET", "/", b"")?,
    };
    assert!(matches!(
        prove(&request, &mut seeded_rng(9)),
        Err(AnonError::NoSuchLeaf)
    ));
    let wrong_root = ProofRequest {
        root: MembershipRoot::from_bytes([1; 32]),
        path: &world.tree.path(LeafPosition::new(OUR_POSITION))?,
        ..request
    };
    assert!(matches!(
        prove(&wrong_root, &mut seeded_rng(9)),
        Err(AnonError::NoSuchLeaf)
    ));
    Ok(())
}
