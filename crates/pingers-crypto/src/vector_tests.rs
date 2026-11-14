// SPDX-License-Identifier: AGPL-3.0-only
//! Known-answer tests: every file in `vectors/crypto/`, loaded with pingers-testkit's loader.
//!
//! The vectors list the randomness each computation consumes; [`FixedRng`] replays it, so the
//! public functions run unchanged. Where a public function needs a pingers-proto encoding
//! (push token padding, `LocationPlaintext`, `Subscription`), the vector starts from the
//! encoded bytes and the test calls the crate-internal step after encoding.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_proto::{
    BoardBeacon, Epoch, GroupId, KeyPackageBytes, LeafIndex, LocationGeneration, LocationRequestId,
    MailboxId, PartyId, Record, RecordRandomness, RecordTag, RevokeCommitment, SealedAsk,
    SealedBeacon, SealedGrant, SealedLocationFrame, SealedPushBlob, SecretBytes, ServerId,
    SizeClass, SpotId, SubscriptionId,
};
use pingers_testkit::{Vector, VectorFile, load_vectors};
use pingers_time::{Bucket, UnixSeconds};
use zeroize::Zeroizing;

use crate::test_support::{
    FixedRng, TestError, error_kind, expected_error, input_array, input_text,
};
use crate::{
    AccountSigningKey, BeaconHeader, GrantContext, GrantKeyPair, GrantSecretKey, GroupRequest,
    InviteSecret, KeyedHashKey, KeyedHashPurpose, KnockKey, KnockPlaintext, KnockSignedFields,
    LocationAad, LocationAsk, LocationGrant, LocationKey, LogAuthKeyPair, MailboxReadKey,
    PresenceRoot, PushBind, PushRelayPrivateKey, PushTokenToOpen, RevokeKey, SealKey, StateKey,
    account_id, derive_knock_key, derive_meetup_psk, derive_seal_key, derive_spot_key,
    derive_tag_key, key_config_hash, keyed_hash, mailbox_id, open_board_beacon, open_join_request,
    open_knock, open_location_ask, open_location_grant, open_state_snapshot, push_bind_for_mailbox,
    push_bind_for_subscription, record_tag, revoke_commitment, revoke_key_matches,
    seal_board_beacon, seal_join_request, seal_knock, seal_location_ask, seal_location_grant,
    seal_state_snapshot, segment_hash, sign_group_request, sign_knock, sign_registration,
    tag_matches, verify_group_request, verify_knock, verify_registration,
};

fn load(name: &str) -> Result<VectorFile, TestError> {
    let file = load_vectors(&format!("crypto/{name}"))?;
    assert_eq!(file.name, name, "file name and `name` field differ");
    assert!(!file.vectors.is_empty());
    Ok(file)
}

fn bucket(vector: &Vector, field: &str) -> Result<Bucket, TestError> {
    Ok(Bucket::from_index(vector.input_u64(field)?)?)
}

fn u32_input(vector: &Vector, field: &'static str) -> Result<u32, TestError> {
    u32::try_from(vector.input_u64(field)?).map_err(|_overflow| TestError::Field(field))
}

fn assert_error<T>(vector: &Vector, result: &Result<T, crate::CryptoError>) {
    assert_eq!(
        error_kind(result),
        expected_error(vector),
        "vector {}",
        vector.name
    );
}

#[test]
fn presence_key_schedule() -> Result<(), TestError> {
    for vector in load("presence-key-schedule")?.vectors {
        let root = PresenceRoot::from_bytes(Zeroizing::new(input_array(&vector, "presence_root")?));
        let start = bucket(&vector, "bucket")?;
        let randomness = RecordRandomness::from_bytes(input_array(&vector, "randomness")?);
        let spot_key =
            derive_spot_key(&root, &SpotId::from_bytes(input_array(&vector, "spot_id")?))?;
        let tag_key = derive_tag_key(&spot_key)?;
        let seal_key = derive_seal_key(&spot_key, start)?;
        let tag = record_tag(&tag_key, start, &randomness)?;

        assert_eq!(
            spot_key.expose_bytes().as_slice(),
            vector.expected_bytes("spot_key")?
        );
        assert_eq!(
            tag_key.expose_bytes().as_slice(),
            vector.expected_bytes("tag_key")?
        );
        assert_eq!(tag.as_bytes().as_slice(), vector.expected_bytes("tag")?);
        assert_eq!(
            seal_key.expose_bytes().as_slice(),
            vector.expected_bytes("seal_key")?
        );
        assert_eq!(
            derive_knock_key(&seal_key, &randomness)?
                .expose_bytes()
                .as_slice(),
            vector.expected_bytes("knock_key")?
        );
        assert_eq!(
            derive_meetup_psk(&spot_key, start)?
                .expose_bytes()
                .as_slice(),
            vector.expected_bytes("meetup_psk")?
        );

        let record = Record {
            randomness,
            tag,
            sealed: SealedBeacon::from_bytes([0; 64]),
        };
        assert!(tag_matches(&tag_key, start, &record)?);
    }
    Ok(())
}

#[test]
fn board_beacon_seal() -> Result<(), TestError> {
    for vector in load("board-beacon-seal")?.vectors {
        let seal_key = SealKey::from_bytes(Zeroizing::new(input_array(&vector, "seal_key")?));
        let start = bucket(&vector, "bucket")?;
        let randomness = RecordRandomness::from_bytes(input_array(&vector, "randomness")?);
        let tag = RecordTag::from_bytes(input_array(&vector, "tag")?);
        if expected_error(&vector).is_some() {
            let record = Record {
                randomness,
                tag,
                sealed: SealedBeacon::from_bytes(input_array(&vector, "sealed")?),
            };
            assert_error(&vector, &open_board_beacon(&seal_key, start, &record));
            continue;
        }
        let beacon = BoardBeacon::from_bytes(&input_array(&vector, "beacon")?)?;
        let header = BeaconHeader {
            start,
            randomness: &randomness,
            tag: &tag,
        };
        let sealed = seal_board_beacon(&seal_key, &header, &beacon)?;
        assert_eq!(
            sealed.as_bytes().as_slice(),
            vector.expected_bytes("sealed")?
        );
        let record = Record {
            randomness,
            tag,
            sealed,
        };
        assert_eq!(open_board_beacon(&seal_key, start, &record)?, beacon);
    }
    Ok(())
}

#[test]
fn knock_seal() -> Result<(), TestError> {
    for vector in load("knock-seal")?.vectors {
        let knock_key = KnockKey::from_bytes(Zeroizing::new(input_array(&vector, "knock_key")?));
        if vector.inputs.contains_key("post") {
            assert_error(
                &vector,
                &open_knock(&knock_key, &vector.input_bytes("post")?),
            );
            continue;
        }
        let class_size = usize::try_from(vector.input_u64("class_size")?)
            .map_err(|_overflow| TestError::Field("class_size"))?;
        let class = SizeClass::from_size(class_size).ok_or(TestError::Field("class_size"))?;
        let encoded = vector.input_bytes("encoded")?;
        let nonce = vector.input_bytes("nonce")?;
        let mut rng = FixedRng::new(&nonce);
        let result = seal_knock(
            &knock_key,
            &KnockPlaintext {
                encoded: &encoded,
                class,
            },
            &mut rng,
        );
        if expected_error(&vector).is_some() {
            assert_error(&vector, &result);
            continue;
        }
        let post = result?;
        assert!(rng.exhausted_exactly());
        assert_eq!(post, vector.expected_bytes("post")?);
        assert_eq!(open_knock(&knock_key, &post)?, encoded);
    }
    Ok(())
}

#[test]
fn knock_signature() -> Result<(), TestError> {
    for vector in load("knock-signature")?.vectors {
        let signing_key =
            AccountSigningKey::from_bytes(Zeroizing::new(input_array(&vector, "signing_key")?));
        let key_package = KeyPackageBytes::from_bytes(vector.input_bytes("key_package")?);
        let fields = KnockSignedFields {
            party: &PartyId::from_bytes(input_array(&vector, "party_id")?),
            epoch: Epoch::new(vector.input_u64("epoch")?),
            spot: &SpotId::from_bytes(input_array(&vector, "spot_id")?),
            start: bucket(&vector, "bucket")?,
            matched: &RecordRandomness::from_bytes(input_array(&vector, "matched_randomness")?),
            key_package: &key_package,
            reply_mailbox: &MailboxId::from_bytes(input_array(&vector, "reply_mailbox")?),
            meetup_subscription: None,
        };
        let subscription_hash: [u8; 32] = input_array(&vector, "subscription_hash")?;
        let message = crate::presence::knock_message(&fields, &subscription_hash);
        let signature = crate::primitives::ed25519_sign(signing_key.expose_bytes(), &message);
        let public_key = signing_key.public_key();

        assert_eq!(
            public_key.as_bytes().as_slice(),
            vector.expected_bytes("public_key")?
        );
        assert_eq!(message, vector.expected_bytes("message")?);
        assert_eq!(signature.as_slice(), vector.expected_bytes("signature")?);
        if subscription_hash == [0; 32] {
            // Without a subscription the public functions produce and accept the same bytes.
            let public = sign_knock(&signing_key, &fields)?;
            assert_eq!(public.as_bytes(), &signature);
            verify_knock(&public_key, &fields, &public)?;
        }
    }
    Ok(())
}

#[test]
fn account_registration() -> Result<(), TestError> {
    for vector in load("account-registration")?.vectors {
        let signing_key =
            AccountSigningKey::from_bytes(Zeroizing::new(input_array(&vector, "signing_key")?));
        let server = ServerId::new(input_text(&vector, "server")?.to_owned())?;
        let public_key = signing_key.public_key();
        let signature = sign_registration(&signing_key, &server)?;

        assert_eq!(
            public_key.as_bytes().as_slice(),
            vector.expected_bytes("public_key")?
        );
        assert_eq!(
            account_id(&public_key).as_bytes().as_slice(),
            vector.expected_bytes("account_id")?
        );
        assert_eq!(
            crate::identity::registration_message(&public_key, &server),
            vector.expected_bytes("message")?
        );
        assert_eq!(
            signature.as_bytes().as_slice(),
            vector.expected_bytes("signature")?
        );
        verify_registration(&public_key, &server, &signature)?;
    }
    Ok(())
}

#[test]
fn group_request_signature() -> Result<(), TestError> {
    for vector in load("group-request-signature")?.vectors {
        let key =
            LogAuthKeyPair::from_bytes(Zeroizing::new(input_array(&vector, "log_auth_seed")?));
        let body = vector.input_bytes("body")?;
        let request = GroupRequest {
            method: input_text(&vector, "method")?,
            path: input_text(&vector, "path")?,
            body: &body,
            timestamp: UnixSeconds::new(vector.input_u64("timestamp")?),
        };
        let signature = sign_group_request(&key, &request)?;

        assert_eq!(
            key.public_key().as_bytes().as_slice(),
            vector.expected_bytes("public_key")?
        );
        assert_eq!(
            crate::group::group_request_message(&request)?,
            vector.expected_bytes("message")?
        );
        assert_eq!(
            signature.as_bytes().as_slice(),
            vector.expected_bytes("signature")?
        );
        verify_group_request(&key.public_key(), &request, &signature)?;
    }
    Ok(())
}

#[test]
fn mailbox_and_push_bind() -> Result<(), TestError> {
    for vector in load("mailbox-and-push-bind")?.vectors {
        let bind = if vector.inputs.contains_key("read_key") {
            let id = mailbox_id(&MailboxReadKey::from_bytes(Zeroizing::new(input_array(
                &vector, "read_key",
            )?)));
            assert_eq!(
                id.as_bytes().as_slice(),
                vector.expected_bytes("mailbox_id")?
            );
            push_bind_for_mailbox(&id)
        } else {
            push_bind_for_subscription(&SubscriptionId::from_bytes(input_array(
                &vector,
                "subscription_id",
            )?))
        };
        assert_eq!(
            bind.as_bytes().as_slice(),
            vector.expected_bytes("push_bind")?
        );
    }
    Ok(())
}

#[test]
fn keyed_hashes() -> Result<(), TestError> {
    for vector in load("keyed-hash")?.vectors {
        let key = KeyedHashKey::from_bytes(Zeroizing::new(input_array(&vector, "key")?));
        let purpose = match input_text(&vector, "purpose")? {
            "ServerCredential" => KeyedHashPurpose::ServerCredential,
            "PushTokenRateLimit" => KeyedHashPurpose::PushTokenRateLimit,
            _ => return Err(TestError::Field("purpose")),
        };
        let hash = keyed_hash(&key, purpose, &vector.input_bytes("input")?)?;
        assert_eq!(
            hash.as_bytes().as_slice(),
            vector.expected_bytes("keyed_hash")?
        );
    }
    Ok(())
}

#[test]
fn hashes() -> Result<(), TestError> {
    for vector in load("hashes")?.vectors {
        if vector.inputs.contains_key("revoke_key") {
            let key = RevokeKey::from_bytes(Zeroizing::new(input_array(&vector, "revoke_key")?));
            assert_eq!(
                revoke_commitment(&key).as_bytes().as_slice(),
                vector.expected_bytes("revoke_commitment")?
            );
            let commitment = RevokeCommitment::from_bytes(
                vector
                    .expected_bytes("revoke_commitment")?
                    .try_into()
                    .map_err(|_length| TestError::Field("revoke_commitment"))?,
            );
            assert!(revoke_key_matches(
                &SecretBytes::new(*key.expose_bytes()),
                &commitment
            ));
        } else if vector.inputs.contains_key("key_config") {
            assert_eq!(
                key_config_hash(&vector.input_bytes("key_config")?)
                    .as_bytes()
                    .as_slice(),
                vector.expected_bytes("key_config_hash")?
            );
        } else {
            let records = Record::parse_concatenated(&vector.input_bytes("records")?)?;
            assert_eq!(
                segment_hash(&records).as_bytes().as_slice(),
                vector.expected_bytes("segment_hash")?
            );
        }
    }
    Ok(())
}

#[test]
fn push_token_seal() -> Result<(), TestError> {
    for vector in load("push-token-seal")?.vectors {
        let private_key = PushRelayPrivateKey::from_bytes(Zeroizing::new(input_array(
            &vector,
            "relay_private_key",
        )?));
        let bind = PushBind::from_bytes(input_array(&vector, "bind")?);
        let key_id = u8::try_from(vector.input_u64("key_id")?)
            .map_err(|_overflow| TestError::Field("key_id"))?;
        if expected_error(&vector).is_some() {
            let blob = SealedPushBlob::from_bytes(input_array(&vector, "blob")?);
            assert_error(
                &vector,
                &crate::mailbox::open_push_padded(
                    &private_key,
                    &PushTokenToOpen {
                        key_id,
                        bind,
                        blob: &blob,
                    },
                ),
            );
            continue;
        }
        let public_key = private_key.public_key(key_id)?;
        let padded = input_array(&vector, "padded_plaintext")?;
        let mut rng = FixedRng::new(&vector.input_bytes("randomness")?);
        let blob = crate::mailbox::seal_push_padded(&public_key, &bind, &padded, &mut rng)?;

        assert!(rng.exhausted_exactly());
        assert_eq!(
            public_key.public_key().as_slice(),
            vector.expected_bytes("relay_public_key")?
        );
        assert_eq!(blob.as_bytes().as_slice(), vector.expected_bytes("blob")?);
        assert_eq!(
            crate::mailbox::push_info(&bind, key_id),
            vector.expected_bytes("info")?
        );
        let opened = crate::mailbox::open_push_padded(
            &private_key,
            &PushTokenToOpen {
                key_id,
                bind,
                blob: &blob,
            },
        )?;
        assert_eq!(*opened, padded);
    }
    Ok(())
}

#[test]
fn location_seal() -> Result<(), TestError> {
    for vector in load("location-seal")?.vectors {
        let key = LocationKey::from_bytes(Zeroizing::new(input_array(&vector, "location_key")?));
        let group = GroupId::from_bytes(input_array(&vector, "group_id")?);
        let aad = LocationAad {
            group: &group,
            epoch: Epoch::new(vector.input_u64("epoch")?),
            sender: LeafIndex::new(u32_input(&vector, "sender_leaf")?),
            generation: LocationGeneration::new(u32_input(&vector, "generation")?),
        };
        let nonce = input_array(&vector, "nonce")?;
        if expected_error(&vector).is_some() {
            let frame = SealedLocationFrame {
                generation: aad.generation,
                nonce,
                ciphertext: vector.input_bytes("ciphertext")?,
            };
            assert_error(
                &vector,
                &crate::location::open_padded_location(&key, &aad, &frame),
            );
            continue;
        }
        let encoded = vector.input_bytes("encoded")?;
        let padded = crate::location::pad_location(&encoded)?;
        let frame = crate::location::seal_padded_location(&key, &aad, &padded, nonce)?;

        assert_eq!(padded.as_slice(), vector.expected_bytes("padded")?);
        assert_eq!(frame.ciphertext, vector.expected_bytes("ciphertext")?);
        assert_eq!(frame.generation, aad.generation);
        let opened = crate::location::open_padded_location(&key, &aad, &frame)?;
        assert_eq!(crate::location::unpad_location(&opened)?, encoded);
    }
    Ok(())
}

fn grant_context<'group>(
    vector: &Vector,
    group: &'group GroupId,
) -> Result<GrantContext<'group>, TestError> {
    Ok(GrantContext {
        group,
        sender: LeafIndex::new(u32_input(vector, "sender_leaf")?),
    })
}

fn check_grant_vector(vector: &Vector) -> Result<(), TestError> {
    let group = GroupId::from_bytes(input_array(vector, "group_id")?);
    let context = grant_context(vector, &group)?;
    if expected_error(vector).is_some() {
        let secret = GrantSecretKey::from_bytes(Zeroizing::new(input_array(
            vector,
            "recipient_secret_key",
        )?));
        let sealed = SealedGrant::from_bytes(vector.input_bytes("sealed")?);
        assert_error(vector, &open_location_grant(&secret, &context, &sealed));
        return Ok(());
    }
    let mut ikm = FixedRng::new(&vector.input_bytes("recipient_ikm")?);
    let pair = if vector.expected_bytes("recipient_public_key")?.len() == 32 {
        GrantKeyPair::generate(&mut ikm)?
    } else {
        generate_x_wing(&mut ikm)?
    };
    assert!(ikm.exhausted_exactly());
    let location_key =
        LocationKey::from_bytes(Zeroizing::new(input_array(vector, "location_key")?));
    let grant = LocationGrant {
        context,
        generation: LocationGeneration::new(u32_input(vector, "generation")?),
        key: &location_key,
        expires_at: UnixSeconds::new(vector.input_u64("expires_at")?),
    };
    let mut rng = FixedRng::new(&vector.input_bytes("randomness")?);
    let sealed = seal_location_grant(&pair.public, &grant, &mut rng)?;

    assert!(rng.exhausted_exactly());
    assert_eq!(
        pair.secret.expose_bytes().as_slice(),
        vector.expected_bytes("recipient_secret_key")?
    );
    assert_eq!(
        pair.public.as_bytes(),
        vector.expected_bytes("recipient_public_key")?
    );
    assert_eq!(sealed.as_bytes(), vector.expected_bytes("sealed")?);
    let opened = open_location_grant(&pair.secret, &context, &sealed)?;
    assert_eq!(opened.generation, grant.generation);
    assert_eq!(opened.key.expose_bytes(), location_key.expose_bytes());
    assert_eq!(opened.expires_at, grant.expires_at);
    Ok(())
}

#[cfg(feature = "x-wing")]
fn generate_x_wing(rng: &mut FixedRng) -> Result<GrantKeyPair, TestError> {
    Ok(GrantKeyPair::generate_x_wing(rng)?)
}

#[cfg(not(feature = "x-wing"))]
fn generate_x_wing(_rng: &mut FixedRng) -> Result<GrantKeyPair, TestError> {
    Err(TestError::Field("recipient_public_key"))
}

#[test]
fn location_grant() -> Result<(), TestError> {
    for vector in load("location-grant")?.vectors {
        check_grant_vector(&vector)?;
    }
    Ok(())
}

#[cfg(feature = "x-wing")]
#[test]
fn location_grant_x_wing() -> Result<(), TestError> {
    for vector in load("location-grant-x-wing")?.vectors {
        check_grant_vector(&vector)?;
    }
    Ok(())
}

#[test]
fn location_ask() -> Result<(), TestError> {
    for vector in load("location-ask")?.vectors {
        let group = GroupId::from_bytes(input_array(&vector, "group_id")?);
        if expected_error(&vector).is_some() {
            let secret = GrantSecretKey::from_bytes(Zeroizing::new(input_array(
                &vector,
                "target_secret_key",
            )?));
            let sealed = SealedAsk::from_bytes(vector.input_bytes("sealed")?);
            assert_error(&vector, &open_location_ask(&secret, &group, &sealed));
            continue;
        }
        let mut ikm = FixedRng::new(&vector.input_bytes("target_ikm")?);
        let pair = GrantKeyPair::generate(&mut ikm)?;
        let ask = LocationAsk {
            group,
            requester: LeafIndex::new(u32_input(&vector, "requester_leaf")?),
            request: LocationRequestId::from_bytes(input_array(&vector, "request_id")?),
            asked_at: UnixSeconds::new(vector.input_u64("asked_at")?),
        };
        let mut rng = FixedRng::new(&vector.input_bytes("randomness")?);
        let sealed = seal_location_ask(&pair.public, &ask, &mut rng)?;

        assert!(rng.exhausted_exactly());
        assert_eq!(
            pair.secret.expose_bytes().as_slice(),
            vector.expected_bytes("target_secret_key")?
        );
        assert_eq!(
            pair.public.as_bytes(),
            vector.expected_bytes("target_public_key")?
        );
        assert_eq!(sealed.as_bytes(), vector.expected_bytes("sealed")?);
        assert_eq!(open_location_ask(&pair.secret, &group, &sealed)?, ask);
    }
    Ok(())
}

#[test]
fn snapshot_and_join_request() -> Result<(), TestError> {
    for vector in load("snapshot-and-join-request")?.vectors {
        if vector.inputs.contains_key("state_key") {
            let key = StateKey::from_bytes(Zeroizing::new(input_array(&vector, "state_key")?));
            let snapshot = vector.input_bytes("snapshot")?;
            let mut rng = FixedRng::new(&vector.input_bytes("nonce")?);
            let sealed = seal_state_snapshot(&key, &snapshot, &mut rng)?;
            assert!(rng.exhausted_exactly());
            assert_eq!(sealed.as_bytes(), vector.expected_bytes("sealed")?);
            assert_eq!(*open_state_snapshot(&key, &sealed)?, snapshot);
            continue;
        }
        let secret =
            InviteSecret::from_bytes(Zeroizing::new(input_array(&vector, "invite_secret")?));
        if expected_error(&vector).is_some() {
            assert_error(
                &vector,
                &open_join_request(&secret, &vector.input_bytes("sealed")?),
            );
            continue;
        }
        let request = vector.input_bytes("request")?;
        let mut rng = FixedRng::new(&vector.input_bytes("nonce")?);
        let sealed = seal_join_request(&secret, &request, &mut rng)?;
        assert!(rng.exhausted_exactly());
        assert_eq!(
            crate::invite::join_key(&secret)?.as_slice(),
            vector.expected_bytes("join_key")?
        );
        assert_eq!(sealed, vector.expected_bytes("sealed")?);
        assert_eq!(*open_join_request(&secret, &sealed)?, request);
    }
    Ok(())
}

#[test]
fn board_cursor() -> Result<(), TestError> {
    for vector in load("board-cursor")?.vectors {
        let key = crate::BoardCursorKey::from_bytes(Zeroizing::new(input_array(&vector, "key")?));
        if expected_error(&vector).is_some() {
            assert_error(
                &vector,
                &crate::open_board_cursor(
                    &key,
                    &pingers_proto::BoardCursor::from_bytes(vector.input_bytes("cursor")?),
                ),
            );
            continue;
        }
        let position = vector.input_bytes("position")?;
        let mut rng = FixedRng::new(&vector.input_bytes("nonce")?);
        let cursor = crate::seal_board_cursor(&key, &position, &mut rng)?;
        assert!(rng.exhausted_exactly());
        assert_eq!(cursor.as_bytes(), vector.expected_bytes("cursor")?);
        assert_eq!(*crate::open_board_cursor(&key, &cursor)?, position);
    }
    Ok(())
}

#[test]
fn join_code_vectors() -> Result<(), TestError> {
    for vector in load("join-code")?.vectors {
        let secret =
            InviteSecret::from_bytes(Zeroizing::new(input_array(&vector, "invite_secret")?));
        let key_package = vector.input_bytes("key_package")?;
        assert_eq!(
            crate::invite::join_code_key(&secret)?.as_slice(),
            vector.expected_bytes("join_code_key")?,
            "vector {}",
            vector.name
        );
        let key = crate::invite::join_code_key(&secret)?;
        assert_eq!(
            crate::primitives::hmac_sha256(&key, &[&key_package])?.as_slice(),
            vector.expected_bytes("mac")?
        );
        let code = crate::join_code(&secret, &key_package)?;
        assert_eq!(code.value(), vector.expected_u64("code_value")?);
        assert_eq!(code.to_string(), vector.expected_str("code")?);
    }
    Ok(())
}
