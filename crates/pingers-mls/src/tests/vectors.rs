// SPDX-License-Identifier: AGPL-3.0-only
//! Known-answer tests for the wrapper's own encodings (`vectors/mls/`).

use openmls::prelude::tls_codec::Serialize as _;
use pingers_crypto::GrantPublicKey;
use pingers_proto::{Epoch, PartyId, SpotId};
use pingers_testkit::load_vectors;
use pingers_time::Bucket;

use super::{TestError, TestResult};
use crate::MeetupPskId;
use crate::policy::{GRANT_KEY_EXTENSION, grant_key_extensions, leaf_grant_key, psk_id_bytes};

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
        let extensions = grant_key_extensions(&grant_key)?;
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
