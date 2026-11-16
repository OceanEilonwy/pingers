// SPDX-License-Identifier: AGPL-3.0-only
//! Negative tests: wrong key, wrong binding, truncated or tampered ciphertext, malformed
//! padding, and label confusion (a value made for one purpose must not pass for another).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_proto::{
    Epoch, GroupId, KeyPackageBytes, LeafIndex, LocationGeneration, LocationRequestId, MailboxId,
    PartyId, PushTokenPlaintext, Record, RecordRandomness, RecordTag, SealedAsk, SealedBeacon,
    SealedGrant, SealedLocationFrame, SealedPushBlob, SealedSnapshot, ServerId, SizeClass, SpotId,
    SubscriptionId,
};
use pingers_testkit::seeded_rng;
use pingers_time::{Bucket, UnixSeconds};
use zeroize::Zeroizing;

use crate::test_support::TestError;
use crate::{
    AccountSigningKey, CryptoError, GrantContext, GrantKeyPair, GrantPublicKey, GroupRequest,
    InviteSecret, KnockKey, KnockPlaintext, KnockSignedFields, LocationAad, LocationAsk,
    LocationGrant, LocationKey, LogAuthKeyPair, PresenceRoot, PushBind, PushRelayPrivateKey,
    PushTokenToOpen, SEALED_LOCATION_LENGTH, StateKey, derive_knock_key, derive_meetup_psk,
    derive_seal_key, derive_spot_key, derive_tag_key, dummy_location, open_join_request,
    open_knock, open_location, open_location_ask, open_location_grant, open_state_snapshot,
    push_bind_for_mailbox, push_bind_for_subscription, record_tag, seal_join_request, seal_knock,
    seal_location_ask, seal_location_grant, sign_group_request, sign_knock, sign_registration,
    tag_matches, verify_group_request, verify_knock, verify_registration,
};

fn root(byte: u8) -> PresenceRoot {
    PresenceRoot::from_bytes(Zeroizing::new([byte; 32]))
}

fn bucket(index: u64) -> Result<Bucket, TestError> {
    Ok(Bucket::from_index(index)?)
}

// Redaction (style guide §9).

#[test]
fn public_values_debug_as_their_type_name_only() {
    let key = AccountSigningKey::from_bytes(Zeroizing::new([0xab; 32])).public_key();
    let signature = crate::Signature::from_bytes([0xcd; 64]);
    let grant_key = GrantPublicKey::from_bytes(vec![0xef; 32]);
    assert_eq!(format!("{key:?}"), "AccountPublicKey(redacted)");
    assert_eq!(format!("{signature:?}"), "Signature(..)");
    assert_eq!(format!("{grant_key:?}"), "GrantPublicKey(redacted)");
}

// Presence key schedule.

#[test]
fn spot_keys_differ_per_spot_and_per_root() -> Result<(), TestError> {
    let spot = SpotId::from_bytes([1; 16]);
    let first = derive_spot_key(&root(1), &spot)?;
    let other_spot = derive_spot_key(&root(1), &SpotId::from_bytes([2; 16]))?;
    let other_root = derive_spot_key(&root(2), &spot)?;
    assert_ne!(first.expose_bytes(), other_spot.expose_bytes());
    assert_ne!(first.expose_bytes(), other_root.expose_bytes());
    Ok(())
}

#[test]
fn seal_key_and_meetup_psk_of_one_bucket_differ() -> Result<(), TestError> {
    // Same parent key and same be64(b) suffix: only the "seal" and "psk" labels differ.
    let spot_key = derive_spot_key(&root(1), &SpotId::from_bytes([1; 16]))?;
    let start = bucket(5)?;
    assert_ne!(
        derive_seal_key(&spot_key, start)?.expose_bytes(),
        derive_meetup_psk(&spot_key, start)?.expose_bytes()
    );
    Ok(())
}

#[test]
fn tag_does_not_match_another_bucket_or_spot() -> Result<(), TestError> {
    let spot_key = derive_spot_key(&root(1), &SpotId::from_bytes([1; 16]))?;
    let tag_key = derive_tag_key(&spot_key)?;
    let randomness = RecordRandomness::from_bytes([3; 16]);
    let record = Record {
        randomness,
        tag: record_tag(&tag_key, bucket(10)?, &randomness)?,
        sealed: SealedBeacon::from_bytes([0; 64]),
    };
    assert!(tag_matches(&tag_key, bucket(10)?, &record)?);
    assert!(!tag_matches(&tag_key, bucket(11)?, &record)?);
    let other_spot = derive_tag_key(&derive_spot_key(&root(1), &SpotId::from_bytes([2; 16]))?)?;
    assert!(!tag_matches(&other_spot, bucket(10)?, &record)?);
    Ok(())
}

#[test]
fn tag_with_one_flipped_bit_does_not_match() -> Result<(), TestError> {
    let tag_key = derive_tag_key(&derive_spot_key(&root(1), &SpotId::from_bytes([1; 16]))?)?;
    let randomness = RecordRandomness::from_bytes([3; 16]);
    let mut tag = *record_tag(&tag_key, bucket(10)?, &randomness)?.as_bytes();
    if let Some(last) = tag.last_mut() {
        *last ^= 1;
    }
    let record = Record {
        randomness,
        tag: RecordTag::from_bytes(tag),
        sealed: SealedBeacon::from_bytes([0; 64]),
    };
    assert!(!tag_matches(&tag_key, bucket(10)?, &record)?);
    Ok(())
}

#[test]
fn knock_keys_differ_per_record() -> Result<(), TestError> {
    let seal_key = derive_seal_key(
        &derive_spot_key(&root(1), &SpotId::from_bytes([1; 16]))?,
        bucket(1)?,
    )?;
    let first = derive_knock_key(&seal_key, &RecordRandomness::from_bytes([1; 16]))?;
    let second = derive_knock_key(&seal_key, &RecordRandomness::from_bytes([2; 16]))?;
    assert_ne!(first.expose_bytes(), second.expose_bytes());
    Ok(())
}

// Knocks.

fn knock_key(byte: u8) -> KnockKey {
    KnockKey::from_bytes(Zeroizing::new([byte; 32]))
}

#[test]
fn knock_fills_its_size_class_exactly() -> Result<(), TestError> {
    for class in [SizeClass::Bytes1024, SizeClass::Bytes4096] {
        let post = seal_knock(
            &knock_key(1),
            &KnockPlaintext {
                encoded: b"knock",
                class,
            },
            &mut seeded_rng(1),
        )?;
        assert_eq!(post.len(), class.size());
    }
    Ok(())
}

#[test]
fn knock_under_another_key_does_not_open() -> Result<(), TestError> {
    let post = seal_knock(
        &knock_key(1),
        &KnockPlaintext {
            encoded: b"knock",
            class: SizeClass::Bytes1024,
        },
        &mut seeded_rng(1),
    )?;
    assert!(matches!(
        open_knock(&knock_key(2), &post),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn post_shorter_than_a_nonce_does_not_open() {
    assert!(matches!(
        open_knock(&knock_key(1), &[0; 23]),
        Err(CryptoError::OpenFailed)
    ));
}

#[test]
fn knock_with_nonzero_padding_is_rejected() -> Result<(), TestError> {
    // A correctly sealed plaintext whose padding is not zero: be16(1) || "k" || 0x01 || 0...
    let mut padded = vec![0; 1024 - 40];
    padded.splice(..4, [0, 1, b'k', 1]);
    let nonce = [5; 24];
    let sealed = crate::group::seal_with_nonce(knock_key(1).expose_bytes(), &nonce, &padded)?;
    assert!(matches!(
        open_knock(&knock_key(1), &sealed),
        Err(CryptoError::WrongLength { .. })
    ));
    Ok(())
}

#[test]
fn knock_length_prefix_beyond_the_plaintext_is_rejected() -> Result<(), TestError> {
    let padded = [0xff, 0xff, 0, 0];
    let sealed = crate::group::seal_with_nonce(knock_key(1).expose_bytes(), &[5; 24], &padded)?;
    assert!(matches!(
        open_knock(&knock_key(1), &sealed),
        Err(CryptoError::WrongLength { .. })
    ));
    Ok(())
}

#[test]
fn empty_knock_plaintext_is_rejected() -> Result<(), TestError> {
    // Shorter than the two-byte length prefix.
    let sealed = crate::group::seal_with_nonce(knock_key(1).expose_bytes(), &[5; 24], &[0])?;
    assert!(matches!(
        open_knock(&knock_key(1), &sealed),
        Err(CryptoError::WrongLength { .. })
    ));
    Ok(())
}

struct KnockFixture {
    party: PartyId,
    spot: SpotId,
    matched: RecordRandomness,
    key_package: KeyPackageBytes,
    reply: MailboxId,
}

impl KnockFixture {
    fn new() -> Self {
        Self {
            party: PartyId::from_bytes([1; 16]),
            spot: SpotId::from_bytes([2; 16]),
            matched: RecordRandomness::from_bytes([3; 16]),
            key_package: KeyPackageBytes::from_bytes(vec![4; 64]),
            reply: MailboxId::from_bytes([5; 16]),
        }
    }

    fn fields(&self, epoch: u64, start: Bucket) -> KnockSignedFields<'_> {
        KnockSignedFields {
            party: &self.party,
            epoch: Epoch::new(epoch),
            spot: &self.spot,
            start,
            matched: &self.matched,
            key_package: &self.key_package,
            reply_mailbox: &self.reply,
            meetup_subscription: None,
        }
    }
}

#[test]
fn knock_signature_does_not_cover_another_epoch_or_bucket() -> Result<(), TestError> {
    let key = AccountSigningKey::generate(&mut seeded_rng(1));
    let fixture = KnockFixture::new();
    let signature = sign_knock(&key, &fixture.fields(7, bucket(9)?))?;
    verify_knock(
        &key.public_key(),
        &fixture.fields(7, bucket(9)?),
        &signature,
    )?;
    assert!(matches!(
        verify_knock(
            &key.public_key(),
            &fixture.fields(8, bucket(9)?),
            &signature
        ),
        Err(CryptoError::BadSignature)
    ));
    assert!(matches!(
        verify_knock(
            &key.public_key(),
            &fixture.fields(7, bucket(10)?),
            &signature
        ),
        Err(CryptoError::BadSignature)
    ));
    Ok(())
}

#[test]
fn knock_signature_covers_the_key_package() -> Result<(), TestError> {
    let key = AccountSigningKey::generate(&mut seeded_rng(1));
    let fixture = KnockFixture::new();
    let signature = sign_knock(&key, &fixture.fields(7, bucket(9)?))?;
    let other = KnockFixture {
        key_package: KeyPackageBytes::from_bytes(vec![4; 65]),
        ..KnockFixture::new()
    };
    assert!(matches!(
        verify_knock(&key.public_key(), &other.fields(7, bucket(9)?), &signature),
        Err(CryptoError::BadSignature)
    ));
    Ok(())
}

#[test]
fn knock_signature_without_subscription_does_not_cover_one_with() -> Result<(), TestError> {
    // Fails with Encoding until pingers-proto encodes subscriptions, BadSignature after.
    let key = AccountSigningKey::generate(&mut seeded_rng(1));
    let fixture = KnockFixture::new();
    let signature = sign_knock(&key, &fixture.fields(7, bucket(9)?))?;
    let subscription = pingers_proto::Subscription {
        id: pingers_proto::SubscriptionId::from_bytes([6; 16]),
        push: pingers_proto::SealedPush {
            relay: pingers_proto::RelayRef::Listed(pingers_proto::RelayId::new(1)),
            key_id: pingers_proto::PushRelayKeyId::new(1),
            blob: pingers_proto::SealedPushBlob::from_bytes([7; 320]),
            urgency: pingers_proto::Urgency::Alert,
            handle: pingers_proto::PushHandle::from_bytes([8; 8]),
        },
        time_to_live: pingers_proto::TimeToLive::new(core::time::Duration::from_secs(600))?,
        owner: pingers_proto::SubscriptionOwner::from_bytes([9; 32]),
    };
    let with_subscription = KnockSignedFields {
        meetup_subscription: Some(&subscription),
        ..fixture.fields(7, bucket(9)?)
    };
    assert!(verify_knock(&key.public_key(), &with_subscription, &signature).is_err());
    Ok(())
}

#[test]
fn knock_signature_by_another_member_is_rejected() -> Result<(), TestError> {
    let fixture = KnockFixture::new();
    let signer = AccountSigningKey::generate(&mut seeded_rng(1));
    let other = AccountSigningKey::generate(&mut seeded_rng(2));
    let signature = sign_knock(&signer, &fixture.fields(7, bucket(9)?))?;
    assert!(matches!(
        verify_knock(
            &other.public_key(),
            &fixture.fields(7, bucket(9)?),
            &signature
        ),
        Err(CryptoError::BadSignature)
    ));
    Ok(())
}

// Identity and group requests.

#[test]
fn registration_proof_is_bound_to_the_server() -> Result<(), TestError> {
    let key = AccountSigningKey::generate(&mut seeded_rng(1));
    let server = ServerId::new(String::from("https://one.example"))?;
    let other = ServerId::new(String::from("https://two.example"))?;
    let signature = sign_registration(&key, &server)?;
    verify_registration(&key.public_key(), &server, &signature)?;
    assert!(matches!(
        verify_registration(&key.public_key(), &other, &signature),
        Err(CryptoError::BadSignature)
    ));
    Ok(())
}

#[test]
fn registration_signature_is_not_a_group_request_signature() -> Result<(), TestError> {
    // Label confusion: an account key that doubled as a log-auth seed must not let one
    // signature pass for the other.
    let seed = [9; 32];
    let account = AccountSigningKey::from_bytes(Zeroizing::new(seed));
    let log_auth = LogAuthKeyPair::from_bytes(Zeroizing::new(seed));
    let server = ServerId::new(String::from("https://one.example"))?;
    let signature = sign_registration(&account, &server)?;
    let request = GroupRequest {
        method: "POST",
        path: "/v1/accounts",
        body: &[],
        timestamp: UnixSeconds::new(0),
    };
    assert!(verify_group_request(&log_auth.public_key(), &request, &signature).is_err());
    Ok(())
}

#[test]
fn method_and_path_boundary_is_signed() -> Result<(), TestError> {
    let key = LogAuthKeyPair::from_bytes(Zeroizing::new([3; 32]));
    let request = |method, path| GroupRequest {
        method,
        path,
        body: b"body",
        timestamp: UnixSeconds::new(1_000),
    };
    let signature = sign_group_request(&key, &request("GET", "/x"))?;
    verify_group_request(&key.public_key(), &request("GET", "/x"), &signature)?;
    assert!(matches!(
        verify_group_request(&key.public_key(), &request("GE", "T/x"), &signature),
        Err(CryptoError::BadSignature)
    ));
    Ok(())
}

#[test]
fn group_request_signature_covers_body_and_time() -> Result<(), TestError> {
    let key = LogAuthKeyPair::from_bytes(Zeroizing::new([3; 32]));
    let signed = GroupRequest {
        method: "POST",
        path: "/v1/groups/g/commits",
        body: b"commit",
        timestamp: UnixSeconds::new(1_000),
    };
    let signature = sign_group_request(&key, &signed)?;
    let other_body = GroupRequest {
        body: b"commiT",
        ..signed
    };
    let later = GroupRequest {
        timestamp: UnixSeconds::new(1_001),
        ..signed
    };
    assert!(verify_group_request(&key.public_key(), &other_body, &signature).is_err());
    assert!(verify_group_request(&key.public_key(), &later, &signature).is_err());
    Ok(())
}

// Snapshots and join requests.

#[test]
fn snapshot_under_another_key_does_not_open() -> Result<(), TestError> {
    let sealed = crate::seal_state_snapshot(
        &StateKey::from_bytes(Zeroizing::new([1; 32])),
        b"state",
        &mut seeded_rng(1),
    )?;
    assert!(matches!(
        open_state_snapshot(&StateKey::from_bytes(Zeroizing::new([2; 32])), &sealed),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn join_request_does_not_open_as_snapshot_under_the_same_bytes() -> Result<(), TestError> {
    // The join key is HKDF(invite_secret, "join"), not the secret itself.
    let sealed = seal_join_request(
        &InviteSecret::from_bytes(Zeroizing::new([4; 32])),
        b"join",
        &mut seeded_rng(1),
    )?;
    assert!(matches!(
        open_state_snapshot(
            &StateKey::from_bytes(Zeroizing::new([4; 32])),
            &SealedSnapshot::from_bytes(sealed)
        ),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn join_request_under_another_invite_does_not_open() -> Result<(), TestError> {
    let sealed = seal_join_request(
        &InviteSecret::from_bytes(Zeroizing::new([4; 32])),
        b"join",
        &mut seeded_rng(1),
    )?;
    assert!(matches!(
        open_join_request(&InviteSecret::from_bytes(Zeroizing::new([5; 32])), &sealed),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn two_snapshots_of_the_same_state_differ() -> Result<(), TestError> {
    let key = StateKey::from_bytes(Zeroizing::new([1; 32]));
    let mut rng = seeded_rng(1);
    let first = crate::seal_state_snapshot(&key, b"state", &mut rng)?;
    let second = crate::seal_state_snapshot(&key, b"state", &mut rng)?;
    assert_ne!(first, second);
    Ok(())
}

// Lease revocation.

#[test]
fn a_revoke_key_opens_only_its_own_commitment() {
    let key = crate::RevokeKey::from_bytes(Zeroizing::new([4; 32]));
    let commitment = crate::revoke_commitment(&key);
    assert!(crate::revoke_key_matches(
        &pingers_proto::SecretBytes::new([4; 32]),
        &commitment
    ));
    assert!(!crate::revoke_key_matches(
        &pingers_proto::SecretBytes::new([5; 32]),
        &commitment
    ));
    // The commitment itself is public; revealing it is not revealing the key.
    assert!(!crate::revoke_key_matches(
        &pingers_proto::SecretBytes::new(*commitment.as_bytes()),
        &commitment
    ));
}

// Join requests.

#[test]
fn join_requests_fill_the_smallest_mailbox_class_whatever_their_size() -> Result<(), TestError> {
    let secret = InviteSecret::from_bytes(Zeroizing::new([3; 32]));
    let mut rng = seeded_rng(1);
    for (length, class) in [
        (1, SizeClass::Bytes1024),
        (982, SizeClass::Bytes1024),
        (983, SizeClass::Bytes4096),
    ] {
        let request = vec![0x5a; length];
        let sealed = seal_join_request(&secret, &request, &mut rng)?;
        assert_eq!(sealed.len(), class.size(), "{length}");
        assert_eq!(*open_join_request(&secret, &sealed)?, request);
    }
    Ok(())
}

#[test]
fn a_join_request_too_big_for_its_class_is_refused() -> Result<(), TestError> {
    let secret = InviteSecret::from_bytes(Zeroizing::new([3; 32]));
    let sealed = crate::seal_join_request_in_class(
        &secret,
        &[1; 4054],
        SizeClass::Bytes4096,
        &mut seeded_rng(1),
    )?;
    assert_eq!(sealed.len(), 4096);
    assert!(matches!(
        crate::seal_join_request_in_class(
            &secret,
            &[1; 4055],
            SizeClass::Bytes4096,
            &mut seeded_rng(1)
        ),
        Err(CryptoError::WrongLength {
            expected: 4054,
            actual: 4055
        })
    ));
    Ok(())
}

// Push tokens.

fn open_push(
    private: &PushRelayPrivateKey,
    key_id: u8,
    bind: PushBind,
    blob: &SealedPushBlob,
) -> Result<Zeroizing<[u8; PushTokenPlaintext::PADDED_SIZE]>, CryptoError> {
    crate::mailbox::open_push_padded(private, &PushTokenToOpen { key_id, bind, blob })
}

#[test]
fn push_blob_for_one_mailbox_does_not_open_for_another() -> Result<(), TestError> {
    let private = PushRelayPrivateKey::generate(&mut seeded_rng(1));
    let public = private.public_key(1)?;
    let bind = PushBind::from_bytes([1; 32]);
    let padded = [7; PushTokenPlaintext::PADDED_SIZE];
    let blob = crate::mailbox::seal_push_padded(&public, &bind, &padded, &mut seeded_rng(2))?;
    assert_eq!(*open_push(&private, 1, bind, &blob)?, padded);
    assert!(matches!(
        open_push(&private, 1, PushBind::from_bytes([2; 32]), &blob),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn a_subscription_bind_never_equals_the_mailbox_bind_of_the_same_bytes() -> Result<(), TestError> {
    // With one shared label these were equal, so a blob registered for a subscription would
    // open when the server presented it for a mailbox with the same id bytes.
    let id = [9; 16];
    let mailbox = push_bind_for_mailbox(&MailboxId::from_bytes(id));
    let subscription = push_bind_for_subscription(&SubscriptionId::from_bytes(id));
    assert_ne!(mailbox, subscription);
    let private = PushRelayPrivateKey::generate(&mut seeded_rng(1));
    let public = private.public_key(1)?;
    let padded = [7; PushTokenPlaintext::PADDED_SIZE];
    let blob =
        crate::mailbox::seal_push_padded(&public, &subscription, &padded, &mut seeded_rng(2))?;
    assert!(matches!(
        open_push(&private, 1, mailbox, &blob),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn push_blob_does_not_open_under_another_key_id() -> Result<(), TestError> {
    // The same private key configured under two ids: without the key id in `info`, a blob
    // sealed for key 1 would open when presented as key 2.
    let private = PushRelayPrivateKey::generate(&mut seeded_rng(1));
    let bind = PushBind::from_bytes([1; 32]);
    let padded = [7; PushTokenPlaintext::PADDED_SIZE];
    let blob = crate::mailbox::seal_push_padded(
        &private.public_key(1)?,
        &bind,
        &padded,
        &mut seeded_rng(2),
    )?;
    assert_eq!(*open_push(&private, 1, bind, &blob)?, padded);
    assert!(matches!(
        open_push(&private, 2, bind, &blob),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn two_blobs_for_the_same_token_differ() -> Result<(), TestError> {
    let public = PushRelayPrivateKey::generate(&mut seeded_rng(1)).public_key(1)?;
    let bind = PushBind::from_bytes([1; 32]);
    let padded = [7; PushTokenPlaintext::PADDED_SIZE];
    let mut rng = seeded_rng(2);
    let first = crate::mailbox::seal_push_padded(&public, &bind, &padded, &mut rng)?;
    let second = crate::mailbox::seal_push_padded(&public, &bind, &padded, &mut rng)?;
    assert_ne!(first, second);
    Ok(())
}

#[test]
fn relay_public_key_carries_its_key_id() -> Result<(), TestError> {
    let public = PushRelayPrivateKey::generate(&mut seeded_rng(1)).public_key(7)?;
    assert_eq!(public.key_id(), 7);
    Ok(())
}

// Location frames.

fn location_aad(group: &GroupId, generation: u32) -> LocationAad<'_> {
    LocationAad {
        group,
        epoch: Epoch::new(3),
        sender: LeafIndex::new(1),
        generation: LocationGeneration::new(generation),
    }
}

fn sealed_location(
    key: &LocationKey,
    aad: &LocationAad<'_>,
    encoded: &[u8],
) -> Result<SealedLocationFrame, TestError> {
    let padded = crate::location::pad_location(encoded)?;
    Ok(crate::location::seal_padded_location(
        key, aad, &padded, [1; 12],
    )?)
}

#[test]
fn location_under_another_key_does_not_open() -> Result<(), TestError> {
    let group = GroupId::from_bytes([1; 32]);
    let frame = sealed_location(
        &LocationKey::generate(&mut seeded_rng(1)),
        &location_aad(&group, 0),
        b"x",
    )?;
    let other = LocationKey::generate(&mut seeded_rng(2));
    assert!(matches!(
        open_location(&other, &location_aad(&group, 0), &frame),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn location_from_another_group_does_not_open() -> Result<(), TestError> {
    let key = LocationKey::generate(&mut seeded_rng(1));
    let frame = sealed_location(&key, &location_aad(&GroupId::from_bytes([1; 32]), 0), b"x")?;
    let other = GroupId::from_bytes([2; 32]);
    assert!(matches!(
        open_location(&key, &location_aad(&other, 0), &frame),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn dummy_location_does_not_open_and_has_the_real_length() {
    let frame = dummy_location(LocationGeneration::new(9), &mut seeded_rng(1));
    assert_eq!(frame.ciphertext.len(), SEALED_LOCATION_LENGTH);
    assert_eq!(frame.generation, LocationGeneration::new(9));
    let group = GroupId::from_bytes([1; 32]);
    assert!(matches!(
        open_location(
            &LocationKey::generate(&mut seeded_rng(2)),
            &location_aad(&group, 9),
            &frame
        ),
        Err(CryptoError::OpenFailed)
    ));
}

#[test]
fn truncated_location_is_rejected_before_decryption() -> Result<(), TestError> {
    let key = LocationKey::generate(&mut seeded_rng(1));
    let group = GroupId::from_bytes([1; 32]);
    let mut frame = sealed_location(&key, &location_aad(&group, 0), b"x")?;
    frame.ciphertext.pop();
    assert!(matches!(
        open_location(&key, &location_aad(&group, 0), &frame),
        Err(CryptoError::WrongLength {
            expected: SEALED_LOCATION_LENGTH,
            actual: 79
        })
    ));
    Ok(())
}

#[test]
fn location_encoding_longer_than_the_padding_is_rejected() {
    assert!(crate::location::pad_location(&[0; 62]).is_ok());
    assert!(matches!(
        crate::location::pad_location(&[0; 63]),
        Err(CryptoError::WrongLength {
            expected: 62,
            actual: 63
        })
    ));
}

#[test]
fn location_padding_must_be_zero() {
    let mut padded = [0; 64];
    padded.splice_first(&[0, 1, 9, 1]);
    assert!(matches!(
        crate::location::unpad_location(&padded),
        Err(CryptoError::Encoding(_))
    ));
}

#[test]
fn location_length_prefix_beyond_the_padding_is_rejected() {
    let mut padded = [0; 64];
    padded.splice_first(&[0, 63]);
    assert!(matches!(
        crate::location::unpad_location(&padded),
        Err(CryptoError::Encoding(_))
    ));
    padded.splice_first(&[0, 62]);
    assert!(crate::location::unpad_location(&padded).is_ok());
}

trait SpliceFirst {
    fn splice_first(&mut self, prefix: &[u8]);
}

impl SpliceFirst for [u8; 64] {
    fn splice_first(&mut self, prefix: &[u8]) {
        for (target, source) in self.iter_mut().zip(prefix) {
            *target = *source;
        }
    }
}

// Grants and asks.

fn grant_for<'key>(
    group: &'key GroupId,
    key: &'key LocationKey,
    sender: u32,
) -> LocationGrant<'key> {
    LocationGrant {
        context: GrantContext {
            group,
            sender: LeafIndex::new(sender),
        },
        generation: LocationGeneration::new(2),
        key,
        expires_at: UnixSeconds::new(900),
    }
}

#[test]
fn grant_for_another_member_does_not_open() -> Result<(), TestError> {
    let recipient = GrantKeyPair::generate(&mut seeded_rng(1))?;
    let bystander = GrantKeyPair::generate(&mut seeded_rng(2))?;
    let group = GroupId::from_bytes([1; 32]);
    let key = LocationKey::generate(&mut seeded_rng(3));
    let grant = grant_for(&group, &key, 4);
    let sealed = seal_location_grant(&recipient.public, &grant, &mut seeded_rng(4))?;
    assert!(open_location_grant(&recipient.secret, &grant.context, &sealed).is_ok());
    assert!(matches!(
        open_location_grant(&bystander.secret, &grant.context, &sealed),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn grant_from_another_group_does_not_open() -> Result<(), TestError> {
    let recipient = GrantKeyPair::generate(&mut seeded_rng(1))?;
    let group = GroupId::from_bytes([1; 32]);
    let other = GroupId::from_bytes([2; 32]);
    let key = LocationKey::generate(&mut seeded_rng(3));
    let sealed = seal_location_grant(
        &recipient.public,
        &grant_for(&group, &key, 4),
        &mut seeded_rng(4),
    )?;
    let context = GrantContext {
        group: &other,
        sender: LeafIndex::new(4),
    };
    assert!(matches!(
        open_location_grant(&recipient.secret, &context, &sealed),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn ask_does_not_open_as_a_grant_or_grant_as_ask() -> Result<(), TestError> {
    let member = GrantKeyPair::generate(&mut seeded_rng(1))?;
    let group = GroupId::from_bytes([1; 32]);
    let ask = LocationAsk {
        group,
        requester: LeafIndex::new(4),
        request: LocationRequestId::from_bytes([5; 16]),
        asked_at: UnixSeconds::new(10),
    };
    let sealed_ask = seal_location_ask(&member.public, &ask, &mut seeded_rng(2))?;
    let context = GrantContext {
        group: &group,
        sender: LeafIndex::new(4),
    };
    assert!(matches!(
        open_location_grant(
            &member.secret,
            &context,
            &SealedGrant::from_bytes(sealed_ask.as_bytes().to_vec())
        ),
        Err(CryptoError::OpenFailed)
    ));
    let key = LocationKey::generate(&mut seeded_rng(3));
    let sealed_grant = seal_location_grant(
        &member.public,
        &grant_for(&group, &key, 4),
        &mut seeded_rng(4),
    )?;
    assert!(matches!(
        open_location_ask(
            &member.secret,
            &group,
            &SealedAsk::from_bytes(sealed_grant.as_bytes().to_vec())
        ),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn ask_for_another_member_does_not_open() -> Result<(), TestError> {
    let target = GrantKeyPair::generate(&mut seeded_rng(1))?;
    let bystander = GrantKeyPair::generate(&mut seeded_rng(2))?;
    let group = GroupId::from_bytes([1; 32]);
    let ask = LocationAsk {
        group,
        requester: LeafIndex::new(4),
        request: LocationRequestId::from_bytes([5; 16]),
        asked_at: UnixSeconds::new(10),
    };
    let sealed = seal_location_ask(&target.public, &ask, &mut seeded_rng(3))?;
    assert!(matches!(
        open_location_ask(&bystander.secret, &group, &sealed),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn grant_to_a_key_of_unknown_length_is_refused() {
    let group = GroupId::from_bytes([1; 32]);
    let key = LocationKey::generate(&mut seeded_rng(3));
    let result = seal_location_grant(
        &GrantPublicKey::from_bytes(vec![9; 33]),
        &grant_for(&group, &key, 4),
        &mut seeded_rng(4),
    );
    assert!(matches!(result, Err(CryptoError::InvalidKey)));
}

#[test]
fn ask_to_a_low_order_key_is_refused() {
    let ask = LocationAsk {
        group: GroupId::from_bytes([1; 32]),
        requester: LeafIndex::new(4),
        request: LocationRequestId::from_bytes([5; 16]),
        asked_at: UnixSeconds::new(10),
    };
    let result = seal_location_ask(
        &GrantPublicKey::from_bytes(vec![0; 32]),
        &ask,
        &mut seeded_rng(4),
    );
    assert!(matches!(result, Err(CryptoError::InvalidKey)));
}

#[test]
fn grant_key_pairs_from_different_randomness_differ() -> Result<(), TestError> {
    let first = GrantKeyPair::generate(&mut seeded_rng(1))?;
    let second = GrantKeyPair::generate(&mut seeded_rng(2))?;
    assert_ne!(first.public, second.public);
    assert_eq!(first.public.as_bytes().len(), 32);
    Ok(())
}

#[test]
fn malformed_grant_plaintext_is_rejected() {
    assert!(matches!(
        crate::location::parse_grant(&[0; 43]),
        Err(CryptoError::WrongLength {
            expected: 44,
            actual: 43
        })
    ));
    assert!(matches!(
        crate::location::parse_grant(&[0; 45]),
        Err(CryptoError::WrongLength { .. })
    ));
}

#[cfg(feature = "x-wing")]
#[test]
fn x_wing_grant_does_not_open_with_a_classical_key() -> Result<(), TestError> {
    let pq = GrantKeyPair::generate_x_wing(&mut seeded_rng(1))?;
    let classical = GrantKeyPair::generate(&mut seeded_rng(1))?;
    let group = GroupId::from_bytes([1; 32]);
    let key = LocationKey::generate(&mut seeded_rng(3));
    let grant = grant_for(&group, &key, 4);
    let sealed = seal_location_grant(&pq.public, &grant, &mut seeded_rng(4))?;
    assert_eq!(sealed.as_bytes().len(), 1_120 + 44 + 16);
    assert!(open_location_grant(&pq.secret, &grant.context, &sealed).is_ok());
    assert!(matches!(
        open_location_grant(&classical.secret, &grant.context, &sealed),
        Err(CryptoError::OpenFailed)
    ));
    Ok(())
}
