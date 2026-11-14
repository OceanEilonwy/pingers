// SPDX-License-Identifier: AGPL-3.0-only
//! Known-answer tests for the wrapper's own encodings (`vectors/mls/`).

use openmls::prelude::tls_codec::Serialize as _;
use pingers_crypto::GrantPublicKey;
use pingers_proto::{Epoch, PartyId, SpotId, SubscriptionId};
use pingers_testkit::load_vectors;
use pingers_time::Bucket;

use super::{TestError, TestResult};
use crate::MeetupPskId;
use crate::policy::{
    GRANT_KEY_EXTENSION, SUBSCRIPTION_EXTENSION, leaf_grant_key, leaf_subscription,
    meetup_leaf_extensions, psk_id_bytes,
};

fn array<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N], TestError> {
    <[u8; N]>::try_from(bytes).map_err(|_| TestError::Missing("a field of the right length"))
}

#[test]
fn meetup_psk_id_matches_vectors() -> TestResult {
    let file = load_vectors("mls/meetup-psk-id")?;
    assert!(!file.vectors.is_empty());
    for vector in &file.vectors {
        let id = MeetupPskId {
            party: PartyId::from_bytes(array(vector.input_bytes("party_id")?)?),
            epoch: Epoch::new(vector.input_u64("epoch")?),
            spot: SpotId::from_bytes(array(vector.input_bytes("spot_id")?)?),
            bucket: Bucket::from_index(vector.input_u64("bucket")?)?,
        };
        assert_eq!(
            psk_id_bytes(&id),
            vector.expected_bytes("psk_id")?,
            "{}",
            vector.name
        );
    }
    Ok(())
}

#[test]
fn grant_key_extension_matches_vectors() -> TestResult {
    let file = load_vectors("mls/grant-key-extension")?;
    assert!(!file.vectors.is_empty());
    for vector in &file.vectors {
        let grant_key = GrantPublicKey::from_bytes(vector.input_bytes("grant_key")?);
        let expected_type = vector
            .expected
            .get("extension_type")
            .and_then(serde_json::Value::as_u64)
            .ok_or(TestError::Missing("extension_type"))?;
        assert_eq!(u64::from(GRANT_KEY_EXTENSION), expected_type);
        let extensions = meetup_leaf_extensions(&grant_key, None)?;
        let extension = extensions
            .iter()
            .next()
            .ok_or(TestError::Missing("the extension"))?;
        let encoded = extension
            .tls_serialize_detached()
            .map_err(|_| TestError::Missing("an encodable extension"))?;
        assert_eq!(
            encoded,
            vector.expected_bytes("extension")?,
            "{}",
            vector.name
        );
    }
    Ok(())
}

#[test]
fn grant_key_is_read_back_from_a_leaf() -> TestResult {
    use crate::tests::{Device, grant_key, rng};
    let mut rng = rng(50);
    let device = Device::new(&mut rng);
    let bytes = device.meetup_key_package(&grant_key(9), &mut rng)?;
    let provider = crate::provider::Provider::new(&device.storage, crate::provider::NoRand);
    let key_package = crate::group::parse_key_package(&bytes, &provider)?;
    assert_eq!(leaf_grant_key(key_package.leaf_node()), Some(grant_key(9)));
    let party = device.party_key_package(&mut rng)?;
    let party = crate::group::parse_key_package(&party, &provider)?;
    assert_eq!(leaf_grant_key(party.leaf_node()), None);
    Ok(())
}

#[test]
fn subscription_extension_matches_vectors() -> TestResult {
    let file = load_vectors("mls/meetup-subscription-extension")?;
    assert!(!file.vectors.is_empty());
    for vector in &file.vectors {
        let subscription =
            SubscriptionId::from_bytes(array(vector.input_bytes("subscription_id")?)?);
        let expected_type = vector
            .expected
            .get("extension_type")
            .and_then(serde_json::Value::as_u64)
            .ok_or(TestError::Missing("extension_type"))?;
        assert_eq!(u64::from(SUBSCRIPTION_EXTENSION), expected_type);
        let grant_key = GrantPublicKey::from_bytes(vec![7; 32]);
        let extensions = meetup_leaf_extensions(&grant_key, Some(&subscription))?;
        let mut listed = extensions.iter();
        let grant = listed.next().ok_or(TestError::Missing("the grant key"))?;
        assert_eq!(
            grant.extension_type(),
            openmls::prelude::ExtensionType::Unknown(GRANT_KEY_EXTENSION)
        );
        let extension = listed
            .next()
            .ok_or(TestError::Missing("the subscription"))?;
        let encoded = extension
            .tls_serialize_detached()
            .map_err(|_| TestError::Missing("an encodable extension"))?;
        assert_eq!(
            encoded,
            vector.expected_bytes("extension")?,
            "{}",
            vector.name
        );
    }
    Ok(())
}

#[test]
fn a_subscription_is_read_back_from_a_leaf_only_when_it_has_one() -> TestResult {
    use crate::tests::{Device, grant_key, rng};
    let mut rng = rng(51);
    let device = Device::new(&mut rng);
    let provider = crate::provider::Provider::new(&device.storage, crate::provider::NoRand);
    let subscription = SubscriptionId::from_bytes([0xa5; 16]);
    let with = device.meetup_key_package_with(&grant_key(9), Some(&subscription), &mut rng)?;
    let with = crate::group::parse_key_package(&with, &provider)?;
    assert_eq!(leaf_subscription(with.leaf_node()), Some(subscription));
    assert_eq!(leaf_grant_key(with.leaf_node()), Some(grant_key(9)));
    let without = device.meetup_key_package(&grant_key(9), &mut rng)?;
    let without = crate::group::parse_key_package(&without, &provider)?;
    assert_eq!(leaf_subscription(without.leaf_node()), None);
    Ok(())
}
