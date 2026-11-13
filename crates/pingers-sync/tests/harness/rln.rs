// SPDX-License-Identifier: AGPL-3.0-only
//! The main app's RLN path: the membership tree kept whole from the public leaf list, and
//! a proof the server's verifier accepts.

use pingers_anon::{
    AcceptedRoots, LeafValue, MESSAGE_LIMIT, RlnSecret, VerificationContext, request_digest, verify,
};
use pingers_proto::{
    MembershipIndex, MembershipLeaves, MembershipRoot, RlnAuth, RlnScope, WireDecode,
};
use pingers_store::{RlnIdentity, TreePosition};
use pingers_sync::{CredentialChoice, Delivered, MembershipRefresh, WakeBudget};
use pingers_testkit::seeded_rng;
use pingers_time::Clock as _;

use crate::mailbox::knock_mailbox;
use crate::support::{Device, TestError, TestResult, World, server};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Decodes padded base64url, as the server will.
fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0_u32;
    let mut count = 0_u32;
    let mut bytes = Vec::new();
    for symbol in text.trim_end_matches('=').bytes() {
        let value = ALPHABET.iter().position(|candidate| *candidate == symbol)?;
        bits = (bits << 6) | u32::try_from(value).ok()?;
        count = count.saturating_add(6);
        if count >= 8 {
            count = count.saturating_sub(8);
            bytes.push(u8::try_from((bits >> count) & 0xff).ok()?);
        }
    }
    Some(bytes)
}

fn leaves(values: &[[u8; 32]], zeroed: &[u32], next: u32) -> MembershipLeaves {
    MembershipLeaves {
        leaves: values.to_vec(),
        zeroed: zeroed.iter().copied().map(MembershipIndex::new).collect(),
        roots: Vec::<MembershipRoot>::new(),
        next: MembershipIndex::new(next),
    }
}

/// Registers this device as leaf 1 of a three-leaf tree in which leaf 0 has been zeroed.
async fn join_tree(world: &World, device: &Device) -> Result<LeafValue, TestError> {
    let secret = RlnSecret::generate(&mut seeded_rng(21))?;
    let mine = secret.id_commitment()?.leaf()?;
    let live_from = u32::try_from(world.clock.now().get() / 3_600)
        .map_err(|_| TestError::Unexpected("root epoch past u32"))?;
    let identity = RlnIdentity {
        secret,
        leaf: TreePosition::new(1),
        live_from: pingers_store::RootEpoch::new(live_from),
    };
    device.session.rln_identity().save(&identity)?;
    let other = RlnSecret::generate(&mut seeded_rng(22))?
        .id_commitment()?
        .leaf()?;
    device
        .link
        .script_leaves(leaves(&[[0; 32], *mine.as_bytes()], &[], 2));
    device
        .link
        .script_leaves(leaves(&[*other.as_bytes()], &[0], 3));
    device.link.script_leaves(leaves(&[], &[0], 3));
    let refresh = device
        .engine
        .refresh_membership(WakeBudget::UNLIMITED)
        .await?;
    assert_eq!(
        refresh,
        MembershipRefresh {
            added: 3,
            zeroed: 1
        }
    );
    Ok(mine)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_main_app_proves_and_the_proof_verifies() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let _ = join_tree(&world, &device).await?;
    assert_eq!(
        device.engine.credential_choice().await?,
        CredentialChoice::MembershipProof
    );
    let tokens = device.engine.token_count().await?;

    let registered = device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await?;
    assert!(matches!(registered, Delivered::Done(_)));
    assert_eq!(device.engine.token_count().await?, tokens, "no token spent");

    let seen = device.link.seen();
    let registration = seen
        .iter()
        .find(|seen| seen.path == "/v1/mailboxes")
        .ok_or(TestError::Unexpected("no registration seen"))?;
    let header = registration
        .authorization
        .as_deref()
        .and_then(|value| value.strip_prefix("PingersRln "))
        .ok_or(TestError::Unexpected("no RLN header"))?;
    let auth =
        RlnAuth::decode(&base64url_decode(header).ok_or(TestError::Unexpected("not base64url"))?)?;
    assert_eq!(auth.scope, RlnScope::Mailbox);
    assert_eq!(auth.bucket, world.clock.now().bucket());

    // The server's check: the local tree's root, this server, this exact request.
    let tree_leaves = device.session.rln_tree().leaves()?;
    assert_eq!(
        tree_leaves.first().map(|leaf| *leaf.value.as_bytes()),
        Some([0; 32]),
        "leaf 0 was zeroed"
    );
    let mut tree = pingers_anon::MembershipTree::empty()?;
    tree.apply(
        &tree_leaves
            .iter()
            .map(|leaf| pingers_anon::LeafUpdate::Appended(leaf.value))
            .collect::<Vec<_>>(),
    )?;
    let server = server()?;
    let verified = verify(
        &auth,
        &VerificationContext {
            roots: AcceptedRoots::first(tree.root()),
            server: &server,
            expected: request_digest("POST", "/v1/mailboxes", &registration.body)?,
        },
    )?;
    assert_eq!(verified.scope, RlnScope::Mailbox);

    // The message id was spent before sending: the bucket has seven left.
    let mut left = 0;
    while device
        .session
        .rln_counters()
        .reserve(RlnScope::Mailbox, world.clock.now().bucket())?
        .is_some()
    {
        left += 1;
    }
    assert_eq!(left, MESSAGE_LIMIT.saturating_sub(1));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spent_bucket_falls_back_to_a_token() -> TestResult {
    let world = World::start()?;
    let device = world.device()?;
    let _ = device.stock_tokens().await?;
    let _ = join_tree(&world, &device).await?;
    let bucket = world.clock.now().bucket();
    while device
        .session
        .rln_counters()
        .reserve(RlnScope::Mailbox, bucket)?
        .is_some()
    {}
    let tokens = device.engine.token_count().await?;
    let registered = device
        .engine
        .register_mailbox(&knock_mailbox()?, WakeBudget::UNLIMITED)
        .await?;
    assert!(matches!(registered, Delivered::Done(_)));
    assert_eq!(device.engine.token_count().await?, tokens.saturating_sub(1));
    let authorization = device
        .link
        .seen()
        .iter()
        .rev()
        .find_map(|seen| seen.authorization.clone());
    assert!(authorization.is_some_and(|value| value.starts_with("PrivateToken ")));
    Ok(())
}
