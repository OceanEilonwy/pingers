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
    // The joiner learns the Party's suite from the group it joined.
    assert_eq!(group_b.suite()?, CipherSuitePolicy::QuantumResistant);
    assert_eq!(group_a.suite()?, CipherSuitePolicy::QuantumResistant);
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

/// MLS variable-length integer (RFC 9420 §2.1.2), minimal form, for lengths below 16384.
fn varint(length: usize) -> Result<Vec<u8>, TestError> {
    let length = u16::try_from(length).map_err(|_| TestError::Missing("a short length"))?;
    if length < 64 {
        Ok(vec![
            u8::try_from(length).map_err(|_| TestError::Missing("a short length"))?,
        ])
    } else if length < 0x4000 {
        Ok((0x4000 | length).to_be_bytes().to_vec())
    } else {
        Err(TestError::Missing("a length below 16384"))
    }
}

/// A validly signed X-Wing KeyPackage whose init key is cut short: the member re-signs it, so
/// only a length check stands between it and the KEM decoder (RUSTSEC-2026-0331, fixed in the
/// vendored libcrux-kem).
fn key_package_with_short_init_key(
    device: &Device,
    rng: &mut rand_chacha::ChaCha20Rng,
) -> Result<pingers_proto::KeyPackageBytes, TestError> {
    use openmls_traits::signatures::Signer as _;

    let bytes = device
        .context()
        .create_key_package(
            KeyPackagePurpose::Party,
            CipherSuitePolicy::QuantumResistant,
            rng,
        )?
        .as_bytes()
        .to_vec();
    // version (2) || suite (2) || init key <V> || leaf node || extensions || signature <V>
    let init_prefix = varint(1216)?;
    assert_eq!(bytes.get(4..6), Some(init_prefix.as_slice()));
    let signature_start = bytes
        .len()
        .checked_sub(66)
        .ok_or(TestError::Missing("a signature"))?;
    assert_eq!(
        bytes.get(signature_start..).and_then(|tail| tail.get(..2)),
        Some([0x40, 0x40].as_slice())
    );
    let middle = bytes
        .get(6 + 1216..signature_start)
        .ok_or(TestError::Missing("a leaf node"))?;
    let mut tbs = bytes
        .get(..4)
        .ok_or(TestError::Missing("a header"))?
        .to_vec();
    tbs.extend(varint(100)?);
    tbs.extend(bytes.get(6..106).ok_or(TestError::Missing("an init key"))?);
    tbs.extend_from_slice(middle);
    let label = b"MLS 1.0 KeyPackageTBS";
    let mut content = varint(label.len())?;
    content.extend_from_slice(label);
    content.extend(varint(tbs.len())?);
    content.extend_from_slice(&tbs);
    let signature = crate::provider::leaf_signer(&device.key)
        .sign(&content)
        .map_err(|_| TestError::Missing("a signature"))?;
    tbs.extend(varint(signature.len())?);
    tbs.extend(signature);
    Ok(pingers_proto::KeyPackageBytes::from_bytes(tbs))
}

#[test]
fn a_short_x_wing_init_key_is_refused_without_a_panic() -> TestResult {
    let mut rng = rng(63);
    let (a, b) = (Device::new(&mut rng), Device::new(&mut rng));
    let mut group_a = a.context().create_party_group(
        group_id(1),
        CipherSuitePolicy::QuantumResistant,
        &mut rng,
    )?;
    let short = key_package_with_short_init_key(&b, &mut rng)?;
    assert!(group_a.add_members(&[short], &mut rng).is_err());
    // Nothing was staged: a well-formed member can still be added.
    let good = b.context().create_key_package(
        KeyPackagePurpose::Party,
        CipherSuitePolicy::QuantumResistant,
        &mut rng,
    )?;
    group_a.add_members(&[good], &mut rng)?;
    Ok(())
}
