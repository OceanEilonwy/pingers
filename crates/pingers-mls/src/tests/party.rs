// SPDX-License-Identifier: AGPL-3.0-only
//! Party groups: create, invite, join, remove, leave, exporters.

use openmls::prelude::tls_codec::Serialize as _;
use openmls::prelude::{MIXED_PLAINTEXT_WIRE_FORMAT_POLICY, MlsGroup};
use pingers_proto::{Epoch, LeafIndex, MlsMessageBytes, PartyId, WelcomeBytes};
use rand_chacha::ChaCha20Rng;

use super::{
    Device, TestError, TestResult, application, commit_summary, group_id, padded, rng, tampered,
};
use crate::provider::{InjectedRand, Provider, credential, leaf_signer};
use crate::{
    CipherSuitePolicy, Group, MemoryStorage, MlsError, Party, ProcessedMessage, SelfRemoval,
    StorageError,
};

type PartyGroup<'device> = Group<'device, MemoryStorage, Party>;

struct Trio<'device> {
    a: PartyGroup<'device>,
    b: PartyGroup<'device>,
    c: PartyGroup<'device>,
}

fn welcome(commit: &crate::Commit) -> Result<&WelcomeBytes, TestError> {
    commit
        .welcome
        .as_ref()
        .ok_or(TestError::Missing("a Welcome"))
}

/// A creates the Party, adds B, then C; everyone has processed everything.
fn party_of_three<'device>(
    a: &'device Device,
    b: &'device Device,
    c: &'device Device,
    rng: &mut ChaCha20Rng,
) -> Result<Trio<'device>, TestError> {
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, rng)?;
    let add_b = group_a.add_members(&[b.party_key_package(rng)?], rng)?;
    group_a.merge_pending_commit()?;
    let mut group_b = b.context().join_party_group(welcome(&add_b)?)?;
    let add_c = group_a.add_members(&[c.party_key_package(rng)?], rng)?;
    group_a.merge_pending_commit()?;
    group_b.process(&add_c.commit)?;
    let group_c = c.context().join_party_group(welcome(&add_c)?)?;
    Ok(Trio {
        a: group_a,
        b: group_b,
        c: group_c,
    })
}

#[test]
fn three_members_share_messages_roster_and_exports() -> TestResult {
    let mut rng = rng(1);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        c: mut group_c,
    } = party_of_three(&a, &b, &c, &mut rng)?;

    assert_eq!(group_a.epoch(), Epoch::new(2));
    assert_eq!(group_c.epoch(), Epoch::new(2));
    assert_eq!(group_b.own_leaf(), LeafIndex::new(1));
    assert_eq!(group_c.own_leaf(), LeafIndex::new(2));
    assert_eq!(group_c.group_id(), group_id(1));
    let roster = group_c.roster();
    assert_eq!(roster, group_a.roster());
    assert_eq!(roster.entries().len(), 3);
    assert_eq!(
        roster.signature_key(LeafIndex::new(0)),
        Some(&a.public_key())
    );
    assert_eq!(
        roster.signature_key(LeafIndex::new(2)),
        Some(&c.public_key())
    );

    let sealed = group_b.seal_application(&padded(b"spot created")?, &mut rng)?;
    for receiver in [&mut group_a, &mut group_c] {
        let (sender, plaintext) = application(receiver.process(&sealed)?)?;
        assert_eq!(sender, LeafIndex::new(1));
        assert_eq!(plaintext, padded(b"spot created")?.as_bytes());
    }

    let party = PartyId::from_bytes([9; 16]);
    let root_a = *group_a.presence_root(&party)?.expose_bytes();
    let root_b = *group_b.presence_root(&party)?.expose_bytes();
    let root_c = *group_c.presence_root(&party)?.expose_bytes();
    assert_eq!(root_a, root_b);
    assert_eq!(root_b, root_c);
    // The context matters: another Party id gives another root.
    let other = group_a.presence_root(&PartyId::from_bytes([8; 16]))?;
    assert_ne!(*other.expose_bytes(), root_a);

    assert_eq!(
        group_a.state_key()?.expose_bytes(),
        group_c.state_key()?.expose_bytes()
    );
    assert_eq!(
        group_a.log_auth_key_pair()?.public_key(),
        group_b.log_auth_key_pair()?.public_key()
    );
    // Each exporter label gives its own secret.
    assert_ne!(group_a.state_key()?.expose_bytes(), &root_a);
    assert_ne!(
        group_a.state_key()?.expose_bytes(),
        group_a.log_auth_key_pair()?.expose_bytes()
    );
    Ok(())
}

#[test]
fn state_key_changes_with_the_epoch() -> TestResult {
    let mut rng = rng(2);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        ..
    } = party_of_three(&a, &b, &c, &mut rng)?;
    let before = *group_a.state_key()?.expose_bytes();
    let log_auth_before = group_a.log_auth_key_pair()?.public_key();
    let update = group_b.self_update(&mut rng)?;
    group_b.merge_pending_commit()?;
    let summary = commit_summary(group_a.process(&update.commit)?)?;
    assert_eq!(summary.epoch, Epoch::new(3));
    assert!(summary.added.is_empty());
    assert!(summary.removed.is_empty());
    assert_eq!(summary.self_removal, SelfRemoval::StillMember);
    assert_ne!(*group_a.state_key()?.expose_bytes(), before);
    assert_ne!(group_a.log_auth_key_pair()?.public_key(), log_auth_before);
    Ok(())
}

#[test]
fn committer_knows_the_next_epochs_keys_before_merging() -> TestResult {
    let mut rng = rng(3);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        ..
    } = party_of_three(&a, &b, &c, &mut rng)?;
    assert!(matches!(
        group_a.pending_state_key(),
        Err(MlsError::Internal)
    ));
    assert!(matches!(
        group_a.pending_log_auth_key_pair(),
        Err(MlsError::Internal)
    ));
    let update = group_a.self_update(&mut rng)?;
    let next_state = *group_a.pending_state_key()?.expose_bytes();
    let next_log_auth = group_a.pending_log_auth_key_pair()?.public_key();
    assert_ne!(next_state, *group_a.state_key()?.expose_bytes());
    group_b.process(&update.commit)?;
    assert_eq!(*group_b.state_key()?.expose_bytes(), next_state);
    assert_eq!(group_b.log_auth_key_pair()?.public_key(), next_log_auth);
    group_a.merge_pending_commit()?;
    assert_eq!(*group_a.state_key()?.expose_bytes(), next_state);
    Ok(())
}

#[test]
fn late_joiner_cannot_read_earlier_epochs() -> TestResult {
    let mut rng = rng(4);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    let add_b = group_a.add_members(&[b.party_key_package(&mut rng)?], &mut rng)?;
    group_a.merge_pending_commit()?;
    let mut group_b = b.context().join_party_group(welcome(&add_b)?)?;
    let early = group_a.seal_application(&padded(b"before C")?, &mut rng)?;
    let early_state_key = *group_a.state_key()?.expose_bytes();

    let add_c = group_a.add_members(&[c.party_key_package(&mut rng)?], &mut rng)?;
    group_a.merge_pending_commit()?;
    group_b.process(&add_c.commit)?;
    let mut group_c = c.context().join_party_group(welcome(&add_c)?)?;

    assert!(matches!(
        group_c.process(&early),
        Err(MlsError::WrongEpoch { message, group })
            if message == Epoch::new(1) && group == Epoch::new(2)
    ));
    assert_ne!(*group_c.state_key()?.expose_bytes(), early_state_key);
    // Messages of C's own epoch work.
    let current = group_a.seal_application(&padded(b"after C")?, &mut rng)?;
    assert_eq!(
        application(group_c.process(&current)?)?.1,
        padded(b"after C")?.as_bytes()
    );
    Ok(())
}

#[test]
fn removed_member_cannot_read_later_epochs() -> TestResult {
    let mut rng = rng(5);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        c: mut group_c,
    } = party_of_three(&a, &b, &c, &mut rng)?;
    let stale_c_epoch = group_c.epoch();

    let removal = group_a.remove_member(LeafIndex::new(2), &mut rng)?;
    assert_eq!(removal.welcome, None);
    let own = group_a.merge_pending_commit()?;
    assert_eq!(own.removed, vec![LeafIndex::new(2)]);
    assert!(own.added.is_empty());
    let seen_by_b = commit_summary(group_b.process(&removal.commit)?)?;
    assert_eq!(seen_by_b.removed, vec![LeafIndex::new(2)]);
    assert_eq!(seen_by_b.self_removal, SelfRemoval::StillMember);
    assert_eq!(group_b.roster().entries().len(), 2);

    let later = group_a.seal_application(&padded(b"without C")?, &mut rng)?;
    // C, still at the old epoch, cannot open the new epoch's traffic.
    assert!(matches!(
        group_c.process(&later),
        Err(MlsError::WrongEpoch { message, group })
            if message == Epoch::new(3) && group == stale_c_epoch
    ));
    // Once C processes its removal it learns it is out, and the group no longer loads.
    let seen_by_c = commit_summary(group_c.process(&removal.commit)?)?;
    assert_eq!(seen_by_c.self_removal, SelfRemoval::Removed);
    assert!(group_c.process(&later).is_err());
    assert!(matches!(
        c.context().party_group(&group_id(1)),
        Err(MlsError::NotAMember)
    ));
    c.context().delete_group(&group_id(1))?;
    assert_eq!(c.storage.key_names(), Vec::<String>::new());
    Ok(())
}

#[test]
fn leaver_proposes_and_another_member_commits() -> TestResult {
    let mut rng = rng(6);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        c: mut group_c,
    } = party_of_three(&a, &b, &c, &mut rng)?;

    let leave = group_b.propose_leave(&mut rng)?;
    b.context().delete_group(&group_id(1))?;
    assert!(b.storage.is_empty());
    assert!(matches!(
        b.context().party_group(&group_id(1)),
        Err(MlsError::NotAMember)
    ));

    for group in [&mut group_a, &mut group_c] {
        assert!(matches!(
            group.process(&leave)?,
            ProcessedMessage::Proposal { sender } if sender == LeafIndex::new(1)
        ));
    }
    // Nothing can be sealed until the proposal is committed.
    assert!(matches!(
        group_a.seal_application(&padded(b"wait")?, &mut rng),
        Err(MlsError::CommitPending)
    ));
    let commit = group_a.commit_pending_proposals(&mut rng)?;
    assert_eq!(
        group_a.merge_pending_commit()?.removed,
        vec![LeafIndex::new(1)]
    );
    let summary = commit_summary(group_c.process(&commit.commit)?)?;
    assert_eq!(summary.removed, vec![LeafIndex::new(1)]);
    assert_eq!(group_c.roster(), group_a.roster());
    assert!(!group_c.roster().contains_key(&b.public_key()));
    let sealed = group_c.seal_application(&padded(b"bye B")?, &mut rng)?;
    assert_eq!(application(group_a.process(&sealed)?)?.0, LeafIndex::new(2));
    // The committed proposal is gone from storage too: reloaded groups can seal.
    drop((group_a, group_c));
    for device in [&a, &c] {
        let mut reloaded = device.context().party_group(&group_id(1))?;
        reloaded.seal_application(&padded(b"after reload")?, &mut rng)?;
    }
    Ok(())
}

#[test]
fn received_proposal_survives_a_reload_until_committed() -> TestResult {
    let mut rng = rng(20);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        c: mut group_c,
    } = party_of_three(&a, &b, &c, &mut rng)?;
    let leave = group_b.propose_leave(&mut rng)?;
    group_a.process(&leave)?;
    group_c.process(&leave)?;
    // A restarts before committing: the proposal comes back from storage.
    drop(group_a);
    let mut group_a = a.context().party_group(&group_id(1))?;
    let commit = group_a.commit_pending_proposals(&mut rng)?;
    assert_eq!(
        group_a.merge_pending_commit()?.removed,
        vec![LeafIndex::new(1)]
    );
    let summary = commit_summary(group_c.process(&commit.commit)?)?;
    assert_eq!(summary.removed, vec![LeafIndex::new(1)]);
    Ok(())
}

#[test]
fn commit_from_outside_the_group_is_rejected() -> TestResult {
    let mut rng = rng(7);
    let (a, outsider) = (Device::new(&mut rng), Device::new(&mut rng));
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    // Same group id, same epoch, but the outsider's own key schedule.
    let mut forged = outsider.context().create_party_group(
        group_id(1),
        CipherSuitePolicy::Classical,
        &mut rng,
    )?;
    let forged_commit = forged.self_update(&mut rng)?;
    assert!(matches!(
        group_a.process(&forged_commit.commit),
        Err(MlsError::InvalidMessage)
    ));
    assert_eq!(group_a.epoch(), Epoch::new(0));
    Ok(())
}

#[test]
fn tampered_commit_is_rejected_and_the_original_still_applies() -> TestResult {
    let mut rng = rng(8);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        ..
    } = party_of_three(&a, &b, &c, &mut rng)?;
    let update = group_b.self_update(&mut rng)?;
    assert!(matches!(
        group_a.process(&tampered(&update.commit)),
        Err(MlsError::InvalidMessage)
    ));
    assert_eq!(group_a.epoch(), Epoch::new(2));
    commit_summary(group_a.process(&update.commit)?)?;
    assert_eq!(group_a.epoch(), Epoch::new(3));
    Ok(())
}

#[test]
fn public_message_commit_is_rejected() -> TestResult {
    let mut rng = rng(9);
    let (a, member) = (Device::new(&mut rng), Device::new(&mut rng));
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    // A client that sends handshakes in the clear (as an external commit would be).
    let provider = Provider::new(&member.storage, InjectedRand::new(&mut rng))?;
    let mut plaintext_group = MlsGroup::builder()
        .with_group_id(openmls::prelude::GroupId::from_slice(
            group_id(1).as_bytes(),
        ))
        .ciphersuite(crate::policy::ciphersuite(CipherSuitePolicy::Classical)?)
        .with_wire_format_policy(MIXED_PLAINTEXT_WIRE_FORMAT_POLICY)
        .build(
            &provider,
            &leaf_signer(&member.key),
            credential(&member.key),
        )
        .map_err(|_| TestError::Missing("a plaintext group"))?;
    let bundle = plaintext_group
        .self_update(
            &provider,
            &leaf_signer(&member.key),
            openmls::prelude::LeafNodeParameters::default(),
        )
        .map_err(|_| TestError::Missing("a plaintext commit"))?;
    let bytes = bundle
        .commit()
        .tls_serialize_detached()
        .map_err(|_| TestError::Missing("serialised commit"))?;
    assert!(matches!(
        group_a.process(&MlsMessageBytes::from_bytes(bytes)),
        Err(MlsError::InvalidMessage)
    ));
    Ok(())
}

#[test]
fn proposal_other_than_leaving_is_rejected() -> TestResult {
    let mut rng = rng(10);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        mut b,
        ..
    } = party_of_three(&a, &b, &c, &mut rng)?;
    // B proposes to remove C, which Pingers never does with a standalone proposal.
    let provider = Provider::new(b.storage, InjectedRand::new(&mut rng))?;
    let (proposal, _) = b
        .inner
        .propose_remove_member_by_value(
            &provider,
            &leaf_signer(b.signer),
            openmls::prelude::LeafNodeIndex::new(2),
        )
        .map_err(|_| TestError::Missing("a remove proposal"))?;
    let bytes = proposal
        .tls_serialize_detached()
        .map_err(|_| TestError::Missing("serialised proposal"))?;
    assert!(matches!(
        group_a.process(&MlsMessageBytes::from_bytes(bytes)),
        Err(MlsError::InvalidMessage)
    ));
    Ok(())
}

#[test]
fn pending_commit_blocks_commits_and_sealing_until_discarded() -> TestResult {
    let mut rng = rng(11);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    group_a.add_members(&[b.party_key_package(&mut rng)?], &mut rng)?;
    assert!(matches!(
        group_a.self_update(&mut rng),
        Err(MlsError::CommitPending)
    ));
    assert!(matches!(
        group_a.remove_member(LeafIndex::new(0), &mut rng),
        Err(MlsError::CommitPending)
    ));
    assert!(matches!(
        group_a.commit_pending_proposals(&mut rng),
        Err(MlsError::CommitPending)
    ));
    assert!(matches!(
        group_a.add_members(&[b.party_key_package(&mut rng)?], &mut rng),
        Err(MlsError::CommitPending)
    ));
    assert!(matches!(
        group_a.seal_application(&padded(b"x")?, &mut rng),
        Err(MlsError::CommitPending)
    ));
    // The pending commit survives a reload.
    let mut reloaded = a.context().party_group(&group_id(1))?;
    assert!(matches!(
        reloaded.self_update(&mut rng),
        Err(MlsError::CommitPending)
    ));
    reloaded.discard_pending_commit()?;
    assert_eq!(reloaded.epoch(), Epoch::new(0));
    assert!(matches!(
        reloaded.merge_pending_commit(),
        Err(MlsError::Internal)
    ));
    reloaded.seal_application(&padded(b"x")?, &mut rng)?;
    Ok(())
}

#[test]
fn loser_of_an_epoch_discards_and_follows_the_winner() -> TestResult {
    let mut rng = rng(12);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        c: mut group_c,
    } = party_of_three(&a, &b, &c, &mut rng)?;
    let _losing = group_a.self_update(&mut rng)?;
    let winning = group_b.self_update(&mut rng)?;
    group_b.merge_pending_commit()?;
    group_a.discard_pending_commit()?;
    group_a.process(&winning.commit)?;
    group_c.process(&winning.commit)?;
    let sealed = group_a.seal_application(&padded(b"in sync")?, &mut rng)?;
    assert_eq!(
        application(group_b.process(&sealed)?)?.1,
        padded(b"in sync")?.as_bytes()
    );
    assert_eq!(
        application(group_c.process(&sealed)?)?.1,
        padded(b"in sync")?.as_bytes()
    );
    Ok(())
}

#[test]
fn messages_from_a_future_epoch_are_wrong_epoch() -> TestResult {
    let mut rng = rng(13);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        ..
    } = party_of_three(&a, &b, &c, &mut rng)?;
    let update = group_a.self_update(&mut rng)?;
    group_a.merge_pending_commit()?;
    let future = group_a.seal_application(&padded(b"too soon")?, &mut rng)?;
    assert!(matches!(
        group_b.process(&future),
        Err(MlsError::WrongEpoch { message, group })
            if message == Epoch::new(3) && group == Epoch::new(2)
    ));
    group_b.process(&update.commit)?;
    assert_eq!(
        application(group_b.process(&future)?)?.1,
        padded(b"too soon")?.as_bytes()
    );
    Ok(())
}

#[test]
fn malformed_and_foreign_messages_are_invalid() -> TestResult {
    let mut rng = rng(14);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    let mut other =
        b.context()
            .create_party_group(group_id(2), CipherSuitePolicy::Classical, &mut rng)?;
    assert!(matches!(
        group_a.process(&MlsMessageBytes::from_bytes(vec![1, 2, 3])),
        Err(MlsError::InvalidMessage)
    ));
    let foreign = other.seal_application(&padded(b"elsewhere")?, &mut rng)?;
    assert!(matches!(
        group_a.process(&foreign),
        Err(MlsError::InvalidMessage)
    ));
    // A Welcome or KeyPackage is not a group message.
    let add = group_a.add_members(&[b.party_key_package(&mut rng)?], &mut rng)?;
    let welcome_bytes = MlsMessageBytes::from_bytes(welcome(&add)?.as_bytes().to_vec());
    assert!(matches!(
        other.process(&welcome_bytes),
        Err(MlsError::InvalidMessage)
    ));
    Ok(())
}

#[test]
fn welcomes_are_checked_before_joining() -> TestResult {
    let mut rng = rng(15);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    let add_b = group_a.add_members(&[b.party_key_package(&mut rng)?], &mut rng)?;
    // C has no KeyPackage in this Welcome.
    assert!(matches!(
        c.context().join_party_group(welcome(&add_b)?),
        Err(MlsError::InvalidMessage)
    ));
    assert!(matches!(
        c.context()
            .join_party_group(&WelcomeBytes::from_bytes(vec![0; 4])),
        Err(MlsError::InvalidMessage)
    ));
    // A commit is not a Welcome.
    assert!(matches!(
        b.context()
            .join_party_group(&WelcomeBytes::from_bytes(add_b.commit.as_bytes().to_vec())),
        Err(MlsError::InvalidMessage)
    ));
    Ok(())
}

#[test]
fn a_welcome_with_a_group_id_of_the_wrong_length_is_refused() -> TestResult {
    let mut rng = rng(31);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    // group_id(1) followed by one more byte: cut to 32 bytes it would name group_id(1).
    let mut longer = group_id(1).as_bytes().to_vec();
    longer.push(0);
    let mut group_a =
        a.context()
            .build_party_group(&longer, CipherSuitePolicy::Classical, &mut rng)?;
    let add_b = group_a.add_members(&[b.party_key_package(&mut rng)?], &mut rng)?;
    assert!(matches!(
        b.context().join_party_group(welcome(&add_b)?),
        Err(MlsError::InvalidMessage)
    ));
    Ok(())
}

#[test]
fn unknown_leaf_and_unknown_group_are_not_members() -> TestResult {
    let mut rng = rng(16);
    let a = Device::new(&mut rng);
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    assert!(matches!(
        group_a.remove_member(LeafIndex::new(5), &mut rng),
        Err(MlsError::NotAMember)
    ));
    assert!(matches!(
        a.context().party_group(&group_id(2)),
        Err(MlsError::NotAMember)
    ));
    assert!(matches!(
        a.context().meetup_group(&group_id(1)),
        Err(MlsError::NotAMember)
    ));
    let reloaded = a.context().party_group(&group_id(1))?;
    assert_eq!(reloaded.epoch(), Epoch::new(0));
    assert_eq!(reloaded.own_leaf(), LeafIndex::new(0));
    // Deleting a group that is not there is not an error.
    a.context().delete_group(&group_id(2))?;
    Ok(())
}

#[test]
#[cfg(not(feature = "x-wing"))]
fn quantum_resistant_suite_needs_the_x_wing_feature() {
    let mut rng = rng(17);
    let a = Device::new(&mut rng);
    assert!(matches!(
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::QuantumResistant, &mut rng),
        Err(MlsError::UnsupportedSuite)
    ));
    assert!(matches!(
        a.context().create_key_package(
            crate::KeyPackagePurpose::Party,
            CipherSuitePolicy::QuantumResistant,
            &mut rng
        ),
        Err(MlsError::UnsupportedSuite)
    ));
    assert!(a.storage.is_empty());
}

#[test]
fn storage_failures_are_reported_as_storage_errors() -> TestResult {
    let mut rng = rng(18);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    let key_package = b.party_key_package(&mut rng)?;
    a.storage.fail_with(Some(StorageError::Unavailable));
    assert!(matches!(
        group_a.seal_application(&padded(b"x")?, &mut rng),
        Err(MlsError::Storage(StorageError::Unavailable))
    ));
    assert!(matches!(
        group_a.add_members(core::slice::from_ref(&key_package), &mut rng),
        Err(MlsError::Storage(StorageError::Unavailable))
    ));
    assert!(matches!(
        a.context()
            .create_party_group(group_id(2), CipherSuitePolicy::Classical, &mut rng),
        Err(MlsError::Storage(StorageError::Unavailable))
    ));
    assert!(matches!(
        a.context().party_group(&group_id(1)),
        Err(MlsError::Storage(StorageError::Unavailable))
    ));
    assert!(matches!(
        a.context().delete_group(&group_id(1)),
        Err(MlsError::Storage(StorageError::Unavailable))
    ));
    b.storage.fail_with(Some(StorageError::Corrupt));
    assert!(matches!(
        b.party_key_package(&mut rng),
        Err(MlsError::Storage(StorageError::Corrupt))
    ));
    a.storage.fail_with(None);
    // After a storage failure the handle may be ahead of the store: reload it.
    drop(group_a);
    let mut group_a = a.context().party_group(&group_id(1))?;
    group_a.add_members(&[key_package], &mut rng)?;
    Ok(())
}

#[test]
fn deleting_a_group_leaves_no_state_behind() -> TestResult {
    let mut rng = rng(19);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio { a: mut group_a, .. } = party_of_three(&a, &b, &c, &mut rng)?;
    // A pending commit (with its new leaf key) is deleted too.
    group_a.self_update(&mut rng)?;
    a.context().delete_group(&group_id(1))?;
    assert!(a.storage.is_empty());
    c.context().delete_group(&group_id(1))?;
    assert_eq!(c.storage.key_names(), Vec::<String>::new());
    Ok(())
}

/// Forward secrecy for a stolen device (design A.4): once a commit is merged, nothing that
/// decrypts the previous epoch is left in storage. OpenMLS deletes the old epoch's key pairs
/// on every merge; this pins that our adapter really removes them.
#[test]
fn each_commit_deletes_the_previous_epochs_private_keys() -> TestResult {
    // Epoch secrets, message secrets and the path keys held for one epoch.
    const EPOCH_SCOPED: [&[u8]; 3] = [b"EpochKeyPairs", b"EpochSecrets", b"MessageSecrets"];
    // The committer also replaces its own leaf key.
    const COMMITTER: [&[u8]; 4] = [
        b"EpochKeyPairs",
        b"EpochSecrets",
        b"MessageSecrets",
        b"EncryptionKeyPair",
    ];
    let mut rng = rng(20);
    let (a, b, c) = (
        Device::new(&mut rng),
        Device::new(&mut rng),
        Device::new(&mut rng),
    );
    let Trio {
        a: mut group_a,
        b: mut group_b,
        c: mut group_c,
    } = party_of_three(&a, &b, &c, &mut rng)?;
    for round in 0..3 {
        let committer_before = a.storage.rows_labelled(&COMMITTER);
        let receiver_before = b.storage.rows_labelled(&EPOCH_SCOPED);
        assert!(
            committer_before
                .iter()
                .any(|(key, _)| key.windows(13).any(|part| part == b"EpochKeyPairs")),
            "round {round}: the committer holds path keys to lose"
        );
        let update = group_a.self_update(&mut rng)?;
        group_a.merge_pending_commit()?;
        group_b.process(&update.commit)?;
        group_c.process(&update.commit)?;
        let committer_after = a.storage.rows_labelled(&COMMITTER);
        let receiver_after = b.storage.rows_labelled(&EPOCH_SCOPED);
        assert!(!committer_after.is_empty() && !receiver_after.is_empty());
        let kept: Vec<String> = committer_before
            .intersection(&committer_after)
            .chain(receiver_before.intersection(&receiver_after))
            .map(|(key, _)| String::from_utf8_lossy(key).into_owned())
            .collect();
        assert_eq!(kept, Vec::<String>::new(), "round {round}");
    }
    Ok(())
}

#[test]
fn an_unused_key_package_leaves_no_private_keys_behind() -> TestResult {
    let mut rng = rng(21);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let unanswered = b.party_key_package(&mut rng)?;
    let kept = b.party_key_package(&mut rng)?;
    assert_eq!(b.storage.rows_labelled(&[b"KeyPackage"]).len(), 2);
    b.context().delete_key_package(&unanswered)?;
    assert_eq!(b.storage.rows_labelled(&[b"KeyPackage"]).len(), 1);
    // Deleting again is harmless, and garbage is refused.
    b.context().delete_key_package(&unanswered)?;
    assert!(matches!(
        b.context()
            .delete_key_package(&pingers_proto::KeyPackageBytes::from_bytes(vec![0; 8])),
        Err(MlsError::InvalidMessage)
    ));
    // A Welcome built from the deleted KeyPackage cannot be opened; the kept one still works.
    let mut group_a =
        a.context()
            .create_party_group(group_id(1), CipherSuitePolicy::Classical, &mut rng)?;
    let lost = group_a.add_members(&[unanswered], &mut rng)?;
    assert!(b.context().join_party_group(welcome(&lost)?).is_err());
    group_a.discard_pending_commit()?;
    let added = group_a.add_members(&[kept], &mut rng)?;
    b.context().join_party_group(welcome(&added)?)?;
    assert!(b.storage.rows_labelled(&[b"KeyPackage"]).is_empty());
    Ok(())
}
