// SPDX-License-Identifier: AGPL-3.0-only
//! Known-answer tests against `vectors/rln.json` and `vectors/privacy-pass.json`.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_anon::{AnonError, RlnSecret, external_nullifier, request_digest};
use pingers_proto::{RlnScope, ServerId};
use pingers_testkit::{Vector, VectorError, load_vectors};
use pingers_time::Bucket;
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Anon(#[from] AnonError),
    #[error(transparent)]
    Vector(#[from] VectorError),
    #[error("vector {0} is missing or malformed")]
    Malformed(&'static str),
}

fn vector(file: &str, name: &'static str) -> Result<Vector, TestError> {
    load_vectors(file)?
        .vectors
        .into_iter()
        .find(|vector| vector.name == name)
        .ok_or(TestError::Malformed(name))
}

fn text<'vector>(vector: &'vector Vector, field: &'static str) -> Result<&'vector str, TestError> {
    vector
        .inputs
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(TestError::Malformed(field))
}

fn array<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N], TestError> {
    bytes.try_into().map_err(|_| TestError::Malformed("length"))
}

fn expected_error(vector: &Vector) -> Option<&str> {
    vector
        .expected
        .get("error")
        .and_then(serde_json::Value::as_str)
}

fn secret(vector: &Vector) -> Result<RlnSecret, TestError> {
    Ok(RlnSecret::from_bytes(Zeroizing::new(array(
        vector.input_bytes("secret")?,
    )?))?)
}

fn server(vector: &Vector) -> Result<ServerId, TestError> {
    ServerId::new(text(vector, "server_id")?.to_owned()).map_err(|_| TestError::Malformed("server"))
}

fn scope(vector: &Vector) -> Result<RlnScope, TestError> {
    u8::try_from(vector.input_u64("scope")?)
        .ok()
        .and_then(|wire| RlnScope::from_wire(wire).ok())
        .ok_or(TestError::Malformed("scope"))
}

fn bucket(vector: &Vector) -> Result<Bucket, TestError> {
    Bucket::from_index(vector.input_u64("bucket")?).map_err(|_| TestError::Malformed("bucket"))
}

#[test]
fn identity_vector() -> Result<(), TestError> {
    let vector = vector("rln", "identity")?;
    let commitment = secret(&vector)?.id_commitment()?;
    assert_eq!(
        commitment.as_bytes().as_slice(),
        vector.expected_bytes("id_commitment")?
    );
    assert_eq!(
        commitment.leaf()?.as_bytes().as_slice(),
        vector.expected_bytes("leaf")?
    );
    Ok(())
}

#[test]
fn non_canonical_secret_vector() -> Result<(), TestError> {
    let vector = vector("rln", "secret-equal-to-the-modulus")?;
    assert_eq!(expected_error(&vector), Some("NotAFieldElement"));
    assert!(matches!(
        secret(&vector),
        Err(TestError::Anon(AnonError::NotAFieldElement))
    ));
    Ok(())
}

#[test]
fn external_nullifier_vector() -> Result<(), TestError> {
    let vector = vector("rln", "external-nullifier-mailbox")?;
    let preimage = vector.expected_bytes("preimage")?;
    assert_eq!(
        Sha256::digest(&preimage).as_slice(),
        vector.expected_bytes("preimage_sha256")?
    );
    let server = server(&vector)?;
    let mut built = b"pingers/v1/rln".to_vec();
    built.extend_from_slice(&23_u64.to_be_bytes());
    built.extend_from_slice(server.as_str().as_bytes());
    built.push(scope(&vector)?.to_wire());
    built.extend_from_slice(&bucket(&vector)?.index().to_be_bytes());
    assert_eq!(built, preimage);
    let derived = external_nullifier(&server, scope(&vector)?, bucket(&vector)?)?;
    assert_eq!(
        derived.as_bytes().as_slice(),
        vector.expected_bytes("external_nullifier")?
    );
    Ok(())
}

#[test]
fn request_digest_vectors() -> Result<(), TestError> {
    for name in [
        "request-digest-below-the-modulus",
        "request-digest-reduced-empty-body",
    ] {
        let vector = vector("rln", name)?;
        assert_eq!(
            Sha256::digest(vector.expected_bytes("preimage")?).as_slice(),
            vector.expected_bytes("preimage_sha256")?
        );
        let digest = request_digest(
            text(&vector, "method")?,
            text(&vector, "path")?,
            &vector.input_bytes("body")?,
        )?;
        assert_eq!(
            digest.as_bytes().as_slice(),
            vector.expected_bytes("request_digest")?,
            "{name}"
        );
    }
    Ok(())
}

#[cfg(any(feature = "prover", feature = "verifier"))]
mod tree {
    use pingers_anon::{LeafPosition, LeafUpdate, LeafValue, MembershipTree};

    use super::{TestError, array, vector};

    pub(super) fn from_vector(
        vector: &pingers_testkit::Vector,
    ) -> Result<MembershipTree, TestError> {
        let mut tree = MembershipTree::empty()?;
        let leaves = vector.input_bytes("leaves")?;
        let mut updates = Vec::new();
        for leaf in leaves.chunks(32) {
            updates.push(LeafUpdate::Appended(LeafValue::from_bytes(array(
                leaf.to_vec(),
            )?)));
        }
        if let Ok(zeroed) = vector.input_u64("zeroed") {
            let position = u32::try_from(zeroed).map_err(|_| TestError::Malformed("zeroed"))?;
            updates.push(LeafUpdate::Zeroed(LeafPosition::new(position)));
        }
        tree.apply(&updates)?;
        Ok(tree)
    }

    #[test]
    fn tree_root_vectors() -> Result<(), TestError> {
        for name in ["empty-tree", "three-leaves", "three-leaves-second-zeroed"] {
            let vector = vector("rln", name)?;
            let tree = from_vector(&vector)?;
            assert_eq!(
                tree.root().as_bytes().as_slice(),
                vector.expected_bytes("root")?,
                "{name}"
            );
        }
        Ok(())
    }
}

#[cfg(feature = "prover")]
#[test]
fn proof_public_value_vector() -> Result<(), TestError> {
    use pingers_anon::{LeafPosition, MessageId, ProofRequest, prove};

    let vector = vector("rln", "proof-public-values")?;
    let tree = tree::from_vector(&vector)?;
    let position = u32::try_from(vector.input_u64("position")?)
        .map_err(|_| TestError::Malformed("position"))?;
    let message_id = u8::try_from(vector.input_u64("message_id")?)
        .map_err(|_| TestError::Malformed("message_id"))?;
    let auth = prove(
        &ProofRequest {
            secret: &secret(&vector)?,
            path: &tree.path(LeafPosition::new(position))?,
            root: tree.root(),
            server: &server(&vector)?,
            scope: scope(&vector)?,
            bucket: bucket(&vector)?,
            message_id: MessageId::new(message_id)?,
            digest: request_digest(
                text(&vector, "method")?,
                text(&vector, "path")?,
                &vector.input_bytes("body")?,
            )?,
        },
        &mut pingers_testkit::seeded_rng(1),
    )?;
    assert_eq!(auth.root.as_slice(), vector.expected_bytes("root")?);
    assert_eq!(auth.x.as_slice(), vector.expected_bytes("x")?);
    assert_eq!(auth.y.as_slice(), vector.expected_bytes("y")?);
    assert_eq!(
        auth.nullifier.as_slice(),
        vector.expected_bytes("nullifier")?
    );
    Ok(())
}

#[cfg(feature = "issuer")]
mod privacy_pass {
    use pingers_anon::{AnonError, IssuerKey, check_redemption};
    use sha2::{Digest as _, Sha256};
    use zeroize::Zeroizing;

    use super::{TestError, expected_error, vector};

    fn key(vector: &pingers_testkit::Vector) -> Result<IssuerKey, TestError> {
        Ok(IssuerKey::from_bytes(Zeroizing::new(
            vector.input_bytes("secret_key")?,
        ))?)
    }

    #[test]
    fn issuer_key_vector() -> Result<(), TestError> {
        let vector = vector("privacy-pass", "issuer-key")?;
        let key = key(&vector)?;
        assert_eq!(key.public_key(), vector.expected_bytes("public_key")?);
        assert_eq!(key.key_id().as_slice(), vector.expected_bytes("key_id")?);
        assert_eq!(key.expose_bytes(), vector.input_bytes("secret_key")?);
        Ok(())
    }

    #[test]
    fn token_vectors() -> Result<(), TestError> {
        let challenge = vector("privacy-pass", "token-challenge")?;
        let digest = challenge.expected_bytes("challenge_digest")?;
        assert_eq!(
            Sha256::digest(challenge.expected_bytes("token_challenge")?).as_slice(),
            digest
        );

        let redeems = vector("privacy-pass", "token-redeems")?;
        let token = redeems.input_bytes("token")?;
        assert_eq!(token.get(34..66), Some(digest.as_slice()));
        let nonce = check_redemption(&key(&redeems)?, &token)?;
        assert_eq!(nonce.as_slice(), redeems.expected_bytes("nonce")?);

        let forged = vector("privacy-pass", "token-with-forged-authenticator")?;
        assert_eq!(expected_error(&forged), Some("BadToken"));
        assert!(matches!(
            check_redemption(&key(&forged)?, &forged.input_bytes("token")?),
            Err(AnonError::BadToken)
        ));
        Ok(())
    }
}
