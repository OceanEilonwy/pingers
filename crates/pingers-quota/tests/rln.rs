// SPDX-License-Identifier: AGPL-3.0-only
//! RLN admission end to end: a client builds its tree from the public leaf list and proves;
//! the server admits by bucket, root, digest, proof and nullifier (design B.4.8, D).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;
use std::sync::Arc;

use pingers_anon::{
    AnonError, LeafPosition, LeafUpdate, LeafValue, MembershipTree, MessageId, ProofRequest,
    RequestDigest, RlnSecret, prove, request_digest,
};
use pingers_proto::{AccountId, MembershipIndex, MembershipLeaves, RlnAuth, RlnScope, ServerId};
use pingers_quota::{
    Admission, BUCKET_GRACE, Membership, MemoryQuotaStore, Nullifiers, QuotaError, root_epoch,
};
use pingers_testkit::{ManualClock, seeded_rng, test_clock};
use pingers_time::{Bucket, Clock as _, UnixSeconds};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Quota(#[from] QuotaError),
    #[error(transparent)]
    Anon(#[from] AnonError),
    #[error("fixture: {0}")]
    Fixture(&'static str),
}

/// A server with two registered devices, ours at position 1, one hour after registration.
struct World {
    clock: ManualClock,
    membership: Membership<MemoryQuotaStore>,
    nullifiers: Nullifiers,
    server: ServerId,
    secret: RlnSecret,
}

struct Proved {
    auth: RlnAuth,
    digest: RequestDigest,
}

const OURS: u32 = 1;

impl World {
    fn new() -> Result<Self, TestError> {
        let clock = test_clock();
        let membership = Membership::new(MemoryQuotaStore::new(), Arc::new(clock.clone()))?;
        let server = ServerId::new("https://pingers.example".to_owned())
            .map_err(|_| TestError::Fixture("server id"))?;
        let nullifiers = Nullifiers::new(server.clone(), Arc::new(clock.clone()));
        let mut rng = seeded_rng(11);
        let other = RlnSecret::generate(&mut rng)?;
        let secret = RlnSecret::generate(&mut rng)?;
        membership.register(AccountId::from_bytes([1; 16]), &other.id_commitment()?)?;
        let ours = membership.register(AccountId::from_bytes([2; 16]), &secret.id_commitment()?)?;
        assert_eq!(ours.index, MembershipIndex::new(OURS));
        let world = Self {
            clock,
            membership,
            nullifiers,
            server,
            secret,
        };
        world.next_hour()?;
        Ok(world)
    }

    fn next_hour(&self) -> Result<(), TestError> {
        let epoch = root_epoch(self.clock.now())?;
        let start = u64::from(epoch.get())
            .saturating_add(1)
            .saturating_mul(3600);
        self.clock
            .advance(UnixSeconds::new(start).saturating_duration_since(self.clock.now()));
        Ok(())
    }

    fn list(&self) -> Result<MembershipLeaves, TestError> {
        Ok(self.membership.leaves(MembershipIndex::new(0))?)
    }

    /// Proves for the current bucket against the tree built from `list`.
    fn prove(&self, list: &MembershipLeaves, body: &[u8], slot: u8) -> Result<Proved, TestError> {
        let mut tree = MembershipTree::empty()?;
        let updates: Vec<LeafUpdate> = list
            .leaves
            .iter()
            .map(|leaf| LeafUpdate::Appended(LeafValue::from_bytes(*leaf)))
            .collect();
        tree.apply(&updates)?;
        let path = tree.path(LeafPosition::new(OURS))?;
        let digest = request_digest("POST", "/v1/board/sync", body)?;
        let auth = prove(
            &ProofRequest {
                secret: &self.secret,
                path: &path,
                root: tree.root(),
                server: &self.server,
                scope: RlnScope::Board,
                bucket: self.clock.now().bucket(),
                message_id: MessageId::new(slot)?,
                digest,
            },
            &mut seeded_rng(u64::from(slot)),
        )?;
        Ok(Proved { auth, digest })
    }

    fn admit(&self, proved: &Proved) -> Result<Admission, QuotaError> {
        self.nullifiers
            .admit(&self.membership, &proved.auth, proved.digest)
    }
}

#[test]
fn a_fresh_proof_is_admitted_and_a_retransmission_is_a_replay() -> Result<(), TestError> {
    let world = World::new()?;
    let proved = world.prove(&world.list()?, b"lease", 0)?;
    assert_eq!(world.admit(&proved)?, Admission::Fresh);
    assert_eq!(world.admit(&proved)?, Admission::Replay);
    assert_eq!(world.nullifiers.tracked()?, 1);
    Ok(())
}

#[test]
fn a_nullifier_reused_for_another_request_in_the_bucket_is_rejected() -> Result<(), TestError> {
    let world = World::new()?;
    let list = world.list()?;
    assert_eq!(
        world.admit(&world.prove(&list, b"first", 0)?)?,
        Admission::Fresh
    );
    let reused = world.prove(&list, b"second", 0)?;
    assert!(matches!(
        world.admit(&reused),
        Err(QuotaError::NullifierReused)
    ));
    // The rejected share is not kept: the first request still replays.
    assert_eq!(world.nullifiers.tracked()?, 1);
    let next_slot = world.prove(&list, b"second", 1)?;
    assert_eq!(world.admit(&next_slot)?, Admission::Fresh);
    Ok(())
}

#[test]
fn the_same_slot_is_fresh_again_in_the_next_bucket() -> Result<(), TestError> {
    let world = World::new()?;
    let list = world.list()?;
    assert_eq!(
        world.admit(&world.prove(&list, b"lease", 0)?)?,
        Admission::Fresh
    );
    world.clock.advance(Bucket::LENGTH);
    assert_eq!(
        world.admit(&world.prove(&list, b"lease", 0)?)?,
        Admission::Fresh
    );
    world.clock.advance(Bucket::LENGTH);
    assert_eq!(
        world.admit(&world.prove(&list, b"lease", 0)?)?,
        Admission::Fresh
    );
    assert_eq!(
        world.nullifiers.tracked()?,
        2,
        "buckets before the previous one are forgotten"
    );
    Ok(())
}

#[test]
fn a_proof_against_the_previous_root_is_accepted_and_one_older_is_stale() -> Result<(), TestError> {
    let world = World::new()?;
    let old_list = world.list()?;
    world.membership.register(
        AccountId::from_bytes([3; 16]),
        &RlnSecret::generate(&mut seeded_rng(3))?.id_commitment()?,
    )?;
    world.next_hour()?;
    assert_eq!(
        world.admit(&world.prove(&old_list, b"one", 0)?)?,
        Admission::Fresh
    );
    world.membership.register(
        AccountId::from_bytes([4; 16]),
        &RlnSecret::generate(&mut seeded_rng(4))?.id_commitment()?,
    )?;
    world.next_hour()?;
    let stale = world.prove(&old_list, b"two", 1)?;
    assert!(matches!(
        world.admit(&stale),
        Err(QuotaError::Anon(AnonError::StaleRoot))
    ));
    let fresh = world.prove(&world.list()?, b"two", 1)?;
    assert_eq!(world.admit(&fresh)?, Admission::Fresh);
    Ok(())
}

#[test]
fn the_previous_bucket_is_accepted_only_just_after_the_rollover() -> Result<(), TestError> {
    let world = World::new()?;
    let list = world.list()?;
    let early = world.prove(&list, b"early", 0)?;
    let late = world.prove(&list, b"late", 1)?;
    world.clock.advance(
        Bucket::LENGTH
            .saturating_add(BUCKET_GRACE)
            .saturating_sub(Duration::from_secs(1)),
    );
    assert_eq!(world.admit(&early)?, Admission::Fresh);
    world.clock.advance(Duration::from_secs(1));
    assert!(matches!(world.admit(&late), Err(QuotaError::StaleBucket)));
    Ok(())
}

#[test]
fn a_proof_for_a_future_bucket_is_refused() -> Result<(), TestError> {
    let world = World::new()?;
    world.clock.advance(Bucket::LENGTH);
    let ahead = world.prove(&world.list()?, b"ahead", 0)?;
    world.clock.set_wall(
        world
            .clock
            .now()
            .checked_sub(Bucket::LENGTH)
            .unwrap_or(UnixSeconds::EPOCH),
    );
    assert!(matches!(world.admit(&ahead), Err(QuotaError::StaleBucket)));
    Ok(())
}

#[test]
fn a_proof_for_another_request_is_refused() -> Result<(), TestError> {
    let world = World::new()?;
    let proved = world.prove(&world.list()?, b"lease", 0)?;
    let other = Proved {
        auth: proved.auth,
        digest: request_digest("POST", "/v1/board/sync", b"other")?,
    };
    assert!(matches!(
        world.admit(&other),
        Err(QuotaError::Anon(AnonError::WrongRequest))
    ));
    assert_eq!(world.nullifiers.tracked()?, 0);
    Ok(())
}
