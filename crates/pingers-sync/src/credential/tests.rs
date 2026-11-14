// SPDX-License-Identifier: AGPL-3.0-only
//! Credential choice (design B.4.8) and the pieces of making one.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_anon::{AnonError, LeafValue, RlnSecret};
use pingers_proto::{AnonymousCredential, RlnAuth, RlnScope};
use pingers_store::{
    HeldToken, PATH_LENGTH, RlnIdentity, RootEpoch, StoredPath, TokenKeyId, TreeLeaf, TreePosition,
    UnspentToken,
};
use pingers_testkit::seeded_rng;
use pingers_time::{Bucket, UnixSeconds};

use super::*;

const BOTH: [AnonymousCredential; 2] = [
    AnonymousCredential::MembershipProof,
    AnonymousCredential::PrivacyPass,
];

fn policy(accepted: &[AnonymousCredential], prover: ProverStatus) -> CredentialPolicy {
    CredentialPolicy {
        accepted: accepted.to_vec(),
        prover,
    }
}

#[test]
fn the_main_app_proves_when_everything_allows_it() -> Result<(), SyncError> {
    let choice = choose_credential(
        ProcessRole::MainApp,
        &policy(&BOTH, ProverStatus::Enabled),
        RlnReadiness::Ready,
    )?;
    assert_eq!(choice, CredentialChoice::MembershipProof);
    Ok(())
}

#[test]
fn an_extension_never_proves() -> Result<(), SyncError> {
    let choice = choose_credential(
        ProcessRole::Extension,
        &policy(&BOTH, ProverStatus::Enabled),
        RlnReadiness::Ready,
    )?;
    assert_eq!(choice, CredentialChoice::PrivacyPass);
    let only_proofs = policy(
        &[AnonymousCredential::MembershipProof],
        ProverStatus::Enabled,
    );
    assert!(matches!(
        choose_credential(ProcessRole::Extension, &only_proofs, RlnReadiness::Ready),
        Err(SyncError::NoCredential)
    ));
    Ok(())
}

#[test]
fn each_fallback_trigger_alone_switches_to_tokens() -> Result<(), SyncError> {
    let cases = [
        (policy(&BOTH, ProverStatus::Disabled), RlnReadiness::Ready),
        (
            policy(&BOTH, ProverStatus::Enabled),
            RlnReadiness::NotLiveYet,
        ),
        (
            policy(&BOTH, ProverStatus::Enabled),
            RlnReadiness::NoIdentity,
        ),
        (
            policy(&[AnonymousCredential::PrivacyPass], ProverStatus::Enabled),
            RlnReadiness::Ready,
        ),
    ];
    for (policy, readiness) in cases {
        assert_eq!(
            choose_credential(ProcessRole::MainApp, &policy, readiness)?,
            CredentialChoice::PrivacyPass,
            "{policy:?} {readiness:?}"
        );
    }
    Ok(())
}

#[test]
fn nothing_is_possible_when_the_server_accepts_nothing_usable() {
    let only_proofs = policy(
        &[AnonymousCredential::MembershipProof],
        ProverStatus::Disabled,
    );
    assert!(matches!(
        choose_credential(ProcessRole::MainApp, &only_proofs, RlnReadiness::Ready),
        Err(SyncError::NoCredential)
    ));
    assert!(matches!(
        choose_credential(
            ProcessRole::MainApp,
            &policy(&[], ProverStatus::Enabled),
            RlnReadiness::Ready
        ),
        Err(SyncError::NoCredential)
    ));
}

#[test]
fn root_epochs_are_whole_hours() {
    assert_eq!(root_epoch(UnixSeconds::new(3_599)), 0);
    assert_eq!(root_epoch(UnixSeconds::new(3_600)), 1);
    assert_eq!(root_epoch(UnixSeconds::new(7_201)), 2);
}

fn identity(live_from: u32) -> Result<RlnIdentity, AnonError> {
    Ok(RlnIdentity {
        secret: RlnSecret::generate(&mut seeded_rng(1))?,
        leaf: TreePosition::new(0),
        live_from: RootEpoch::new(live_from),
    })
}

/// A tree copy at `epoch` holding (or not) a path for leaf `position`.
fn tree(epoch: u32, path_for: Option<u32>) -> TreeState {
    TreeState {
        root_epoch: RootEpoch::new(epoch),
        root: pingers_anon::MembershipRoot::from_bytes([1; 32]),
        leaf_count: 4,
        path: path_for.map(|position| StoredPath {
            position: TreePosition::new(position),
            siblings: [[0; 32]; PATH_LENGTH],
        }),
    }
}

const TEN_O_CLOCK: UnixSeconds = UnixSeconds::new(10 * 3_600);

#[test]
fn a_leaf_proves_from_the_root_epoch_it_goes_live_in() -> Result<(), AnonError> {
    let now = TEN_O_CLOCK;
    let fresh = tree(10, Some(0));
    assert_eq!(readiness(None, Some(&fresh), now), RlnReadiness::NoIdentity);
    assert_eq!(
        readiness(Some(&identity(11)?), Some(&fresh), now),
        RlnReadiness::NotLiveYet
    );
    assert_eq!(
        readiness(Some(&identity(10)?), Some(&fresh), now),
        RlnReadiness::Ready
    );
    assert_eq!(
        readiness(Some(&identity(9)?), Some(&fresh), now),
        RlnReadiness::Ready
    );
    Ok(())
}

#[test]
fn a_proof_needs_the_current_or_previous_root_and_one_holding_the_leaf() -> Result<(), AnonError> {
    let now = TEN_O_CLOCK;
    let live = identity(5)?;
    assert_eq!(readiness(Some(&live), None, now), RlnReadiness::TreeStale);
    assert_eq!(
        readiness(Some(&live), Some(&tree(9, Some(0))), now),
        RlnReadiness::Ready,
        "the previous root is still accepted"
    );
    assert_eq!(
        readiness(Some(&live), Some(&tree(8, Some(0))), now),
        RlnReadiness::TreeStale,
        "two hours old: refresh first"
    );
    // A copy from before the leaf went live cannot hold it.
    assert_eq!(
        readiness(Some(&identity(10)?), Some(&tree(9, None)), now),
        RlnReadiness::TreeStale
    );
    Ok(())
}

#[test]
fn a_fresh_copy_without_the_leaf_means_it_was_removed() -> Result<(), AnonError> {
    let now = TEN_O_CLOCK;
    let live = identity(5)?;
    assert_eq!(
        readiness(Some(&live), Some(&tree(10, None)), now),
        RlnReadiness::LeafRemoved
    );
    // A path for another position is an old identity's.
    assert_eq!(
        readiness(Some(&live), Some(&tree(10, Some(3))), now),
        RlnReadiness::LeafRemoved
    );
    Ok(())
}

#[test]
fn the_prover_is_used_only_after_a_passed_self_test() {
    let policy = policy(&BOTH, ProverStatus::Enabled);
    let passed = ProvingState {
        readiness: RlnReadiness::Ready,
        self_test: SelfTestStanding::Passed {
            p95: Duration::from_millis(450),
        },
    };
    assert_eq!(passed.effective(&policy), policy);
    assert_eq!(passed.proof_time(), Some(Duration::from_millis(450)));
    for self_test in [SelfTestStanding::Failed, SelfTestStanding::Due] {
        let state = ProvingState {
            readiness: RlnReadiness::Ready,
            self_test,
        };
        assert_eq!(state.effective(&policy).prover, ProverStatus::Disabled);
        assert_eq!(state.effective(&policy).accepted, BOTH);
        assert_eq!(state.proof_time(), None);
    }
    // The app's kill switch wins over a passed test.
    let off = policy_off();
    assert_eq!(passed.effective(&off).prover, ProverStatus::Disabled);
}

fn policy_off() -> CredentialPolicy {
    policy(&BOTH, ProverStatus::Disabled)
}

#[test]
fn a_proof_is_made_only_with_time_for_it_and_the_request() {
    let proof = Duration::from_millis(500);
    assert!(proof_fits(None, proof), "no deadline in the foreground");
    let just = proof.saturating_add(MIN_REQUEST_TIME);
    assert!(proof_fits(Some(just), proof));
    assert!(!proof_fits(
        Some(just.saturating_sub(Duration::from_millis(1))),
        proof
    ));
}

fn leaf(position: u32, fill: u8) -> TreeLeaf {
    TreeLeaf {
        position: TreePosition::new(position),
        value: LeafValue::from_bytes([fill; 32]),
    }
}

#[test]
fn the_tree_is_rebuilt_in_position_order() -> Result<(), SyncError> {
    let shuffled = tree_from(vec![leaf(1, 2), leaf(0, 1), leaf(2, 3)])?;
    let mut expected = MembershipTree::empty()?;
    expected.apply(&[
        LeafUpdate::Appended(LeafValue::from_bytes([1; 32])),
        LeafUpdate::Appended(LeafValue::from_bytes([2; 32])),
        LeafUpdate::Appended(LeafValue::from_bytes([3; 32])),
    ])?;
    assert_eq!(shuffled.root(), expected.root());
    assert_eq!(shuffled.leaf_count(), 3);
    Ok(())
}

#[test]
fn a_gap_in_the_leaf_log_is_refused() {
    assert!(matches!(
        tree_from(vec![leaf(0, 1), leaf(2, 3)]),
        Err(SyncError::Anon(AnonError::TreeOutOfOrder))
    ));
    assert!(matches!(
        tree_from(vec![leaf(1, 1)]),
        Err(SyncError::Anon(AnonError::TreeOutOfOrder))
    ));
}

#[test]
fn a_proof_travels_as_pingers_rln_base64url() -> Result<(), SyncError> {
    let auth = RlnAuth {
        root: [1; 32],
        scope: RlnScope::Mailbox,
        bucket: Bucket::containing(UnixSeconds::new(1_800_000_000)),
        x: [2; 32],
        y: [3; 32],
        nullifier: [4; 32],
        proof: [5; 128],
    };
    let header = proof_header(&auth)?;
    assert_eq!(header.name, "authorization");
    let value = String::from_utf8(header.value).map_err(|_| SyncError::Internal)?;
    let encoded = value
        .strip_prefix("PingersRln ")
        .ok_or(SyncError::Internal)?;
    // 265 bytes are 354 base64url symbols and two `=`, starting with the root (0x01 bytes).
    assert_eq!(encoded.len(), 356);
    assert!(encoded.ends_with("=="));
    assert!(encoded.starts_with("AQEB"));
    Ok(())
}

#[test]
fn a_token_travels_as_private_token_and_is_carried_by_the_attachment() {
    let token = HeldToken {
        token: UnspentToken::from_bytes(vec![0xff; 4]),
        key_id: TokenKeyId::from_bytes([0; 32]),
    };
    let header = token_header(&token);
    assert_eq!(header.name, "authorization");
    assert_eq!(header.value, b"PrivateToken token=\"_____w==\"");
    let attached = Attached::Token {
        header: header.clone(),
        token,
    };
    assert_eq!(
        attached.header().map(|header| header.value),
        Some(header.value)
    );
    assert!(Attached::Nothing.header().is_none());
}
