// SPDX-License-Identifier: AGPL-3.0-only
//! The X-Wing suite (feature `x-wing`, design B.8).

use pingers_crypto::GrantPublicKey;
use pingers_proto::{Epoch, LeafIndex};

use super::{
    Device, TestError, TestResult, application, group_id, meetup_psk, padded, psk_id, rng,
    roster_of,
};
use crate::{CipherSuitePolicy, KeyPackagePurpose, MeetupCreation, MeetupPskBinding};

#[test]
fn quantum_resistant_party_works_end_to_end() -> TestResult {
    let mut rng = rng(60);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let mut group_a = a.context().create_party_group(
        group_id(1),
        CipherSuitePolicy::QuantumResistant,
        &mut rng,
    )?;
    let key_package = b.context().create_key_package(
        KeyPackagePurpose::Party,
        CipherSuitePolicy::QuantumResistant,
        &mut rng,
    )?;
    // An X-Wing init key makes the KeyPackage much larger than a classical one (design B.8).
    assert!(key_package.as_bytes().len() > b.party_key_package(&mut rng)?.as_bytes().len() + 1000);
    let add = group_a.add_members(&[key_package], &mut rng)?;
    group_a.merge_pending_commit()?;
    let mut group_b = b.context().join_party_group(
        add.welcome
            .as_ref()
            .ok_or(TestError::Missing("a Welcome"))?,
    )?;
    assert_eq!(group_b.epoch(), Epoch::new(1));
    let sealed = group_a.seal_application(&padded(b"pq")?, &mut rng)?;
    assert_eq!(application(group_b.process(&sealed)?)?.0, LeafIndex::new(0));
    Ok(())
}

#[test]
fn classical_key_package_cannot_join_a_quantum_resistant_party() -> TestResult {
    let mut rng = rng(61);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let mut group_a = a.context().create_party_group(
        group_id(1),
        CipherSuitePolicy::QuantumResistant,
        &mut rng,
    )?;
    let classical = b.party_key_package(&mut rng)?;
    assert!(group_a.add_members(&[classical], &mut rng).is_err());
    Ok(())
}

#[test]
fn quantum_resistant_meetup_carries_x_wing_grant_keys() -> TestResult {
    let mut rng = rng(62);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let party = roster_of(&[&a, &b]);
    // An X-Wing grant public key is 1216 bytes.
    let grant_b = GrantPublicKey::from_bytes(vec![0xB0; 1216]);
    let grant_a = GrantPublicKey::from_bytes(vec![0xA0; 1216]);
    let first_member = b.context().create_key_package(
        KeyPackagePurpose::Meetup {
            grant_key: &grant_b,
        },
        CipherSuitePolicy::QuantumResistant,
        &mut rng,
    )?;
    let psk = meetup_psk(7);
    let created = a.context().create_meetup_group(
        &MeetupCreation {
            group_id: group_id(0xA0),
            psk: MeetupPskBinding {
                id: psk_id(4)?,
                value: &psk,
            },
            party_roster: &party,
            first_member: &first_member,
            own_grant_key: &grant_a,
            suite: CipherSuitePolicy::QuantumResistant,
        },
        &mut rng,
    )?;
    let mut group_a = created.group;
    group_a.merge_pending_commit()?;
    let welcome = created
        .commit
        .welcome
        .ok_or(TestError::Missing("a Welcome"))?;
    let group_b = b.context().join_meetup_group(
        &welcome,
        &MeetupPskBinding {
            id: psk_id(4)?,
            value: &psk,
        },
        &party,
    )?;
    let keys: Vec<GrantPublicKey> = group_b
        .grant_keys()?
        .into_iter()
        .map(|member| member.grant_key)
        .collect();
    assert_eq!(keys, vec![grant_a, grant_b]);
    Ok(())
}
