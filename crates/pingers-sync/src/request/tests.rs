// SPDX-License-Identifier: AGPL-3.0-only
//! The request builders: classes, credential scopes and the wire encodings pingers-server
//! reads (`wire.rs`).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_crypto::{
    GroupRequest, LogAuthKeyPair, MailboxReadKey, Signature, verify_group_request,
};
use pingers_proto::{
    AppMessage, BoardCursor, CommitRequest, Delivery, Epoch, GroupAuthKey, GroupId, LeaseId,
    MailboxId, MembershipIndex, MlsMessageBytes, Padded, ProtoError, SecretBytes, SecretText,
    SequenceNumber, SizeClass, SubscriptionId, Urgency, WireDecode,
};
use pingers_time::UnixSeconds;
use pingers_transport::PrivacyClass;
use zeroize::Zeroizing;

use super::*;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Crypto(#[from] pingers_crypto::CryptoError),
    #[error("missing header {0}")]
    MissingHeader(&'static str),
    #[error("bad header {0}")]
    BadHeader(&'static str),
}

const ALL: [Route; 28] = [
    Route::Params,
    Route::CreateAccount,
    Route::Auth,
    Route::DeleteAccount,
    Route::RlnLeaf,
    Route::RlnLeaves,
    Route::PrivacyPassIssue,
    Route::PrivacyPassKeys,
    Route::BoardSync,
    Route::BoardPage,
    Route::BoardRevokeLease,
    Route::CreateMailbox,
    Route::PostToMailbox,
    Route::ReadMailbox,
    Route::DeleteMailbox,
    Route::CreateGroup,
    Route::Commit,
    Route::PutSubscription,
    Route::DeleteSubscription,
    Route::PostGroupMessage,
    Route::ReadGroupMessages,
    Route::GroupState,
    Route::DeleteGroup,
    Route::Live,
    Route::OhttpConfig,
    Route::PushRelayConfig,
    Route::PushRelayNextConfig,
    Route::ReplaceMailboxPush,
];

fn header<'request>(
    request: &'request ApiRequest,
    name: &'static str,
) -> Result<&'request [u8], TestError> {
    request
        .headers()
        .iter()
        .find(|header| header.name == name)
        .map(|header| header.value.as_slice())
        .ok_or(TestError::MissingHeader(name))
}

fn key() -> LogAuthKeyPair {
    LogAuthKeyPair::from_bytes(Zeroizing::new([7; 32]))
}

const NOW: UnixSeconds = UnixSeconds::new(1_800_000_000);

fn group() -> GroupId {
    GroupId::from_bytes([0xab; 32])
}

#[test]
fn only_account_routes_are_identified_and_only_the_live_channel_is_a_stream() {
    for route in ALL {
        let expected = match route {
            Route::CreateAccount
            | Route::Auth
            | Route::DeleteAccount
            | Route::RlnLeaf
            | Route::PrivacyPassIssue => PrivacyClass::Identified,
            Route::Live => PrivacyClass::Stream,
            Route::Params
            | Route::OhttpConfig
            | Route::PushRelayConfig
            | Route::PushRelayNextConfig
            | Route::RlnLeaves
            | Route::PrivacyPassKeys
            | Route::BoardSync
            | Route::BoardPage
            | Route::BoardRevokeLease
            | Route::CreateMailbox
            | Route::PostToMailbox
            | Route::ReadMailbox
            | Route::DeleteMailbox
            | Route::ReplaceMailboxPush
            | Route::CreateGroup
            | Route::Commit
            | Route::PutSubscription
            | Route::DeleteSubscription
            | Route::PostGroupMessage
            | Route::ReadGroupMessages
            | Route::GroupState
            | Route::DeleteGroup => PrivacyClass::Anonymous,
        };
        assert_eq!(route.class(), expected, "{route:?}");
    }
}

#[test]
fn writes_carry_a_credential_of_their_scope_and_capability_requests_none() {
    for route in ALL {
        let expected = match route {
            Route::BoardSync => Some(RlnScope::Board),
            Route::CreateMailbox | Route::PostToMailbox => Some(RlnScope::Mailbox),
            Route::CreateGroup
            | Route::Commit
            | Route::PutSubscription
            | Route::PostGroupMessage => Some(RlnScope::Group),
            Route::Params
            | Route::CreateAccount
            | Route::Auth
            | Route::DeleteAccount
            | Route::RlnLeaf
            | Route::RlnLeaves
            | Route::PrivacyPassIssue
            | Route::PrivacyPassKeys
            | Route::BoardPage
            | Route::BoardRevokeLease
            | Route::ReadMailbox
            | Route::DeleteMailbox
            | Route::ReplaceMailboxPush
            | Route::DeleteSubscription
            | Route::ReadGroupMessages
            | Route::GroupState
            | Route::OhttpConfig
            | Route::PushRelayConfig
            | Route::PushRelayNextConfig
            | Route::DeleteGroup
            | Route::Live => None,
        };
        assert_eq!(route.credential(), expected, "{route:?}");
    }
}

#[test]
fn a_new_group_names_its_lifetime_and_epoch() -> Result<(), TestError> {
    let create = pingers_proto::CreateGroup {
        group_id: group(),
        auth_key: key().public_key(),
        subscriptions: Vec::new(),
        state: None,
    };
    for (lifetime, epoch, path) in [
        (GroupLifetime::Party, 0, "/v1/groups?lifetime=party&epoch=0"),
        (
            GroupLifetime::Meetup,
            1,
            "/v1/groups?lifetime=meetup&epoch=1",
        ),
    ] {
        let start = GroupStart {
            lifetime,
            epoch: Epoch::new(epoch),
        };
        assert_eq!(ApiRequest::create_group(&create, start)?.path(), path);
    }
    Ok(())
}

#[test]
fn identifiers_travel_as_hex_in_the_path() -> Result<(), TestError> {
    let mailbox = MailboxId::from_bytes([0x0f; 16]);
    let body = Padded::pad(b"knock", SizeClass::Bytes1024)?;
    let post = ApiRequest::post_to_mailbox(&mailbox, &body);
    assert_eq!(post.method(), "POST");
    assert_eq!(
        post.path(),
        format!("/v1/mailboxes/{}/msgs", "0f".repeat(16))
    );
    assert_eq!(post.body(), body.as_bytes());
    assert_eq!(post.route(), Route::PostToMailbox);

    let read_key = MailboxReadKey::from_bytes(Zeroizing::new([0xa5; 32]));
    let read = ApiRequest::read_mailbox(&mailbox, &read_key, Some(SequenceNumber::new(9)));
    assert_eq!(read.method(), "GET");
    assert_eq!(
        read.path(),
        format!("/v1/mailboxes/{}/msgs?after=9", "0f".repeat(16))
    );
    assert_eq!(header(&read, READ_KEY_HEADER)?, "a5".repeat(32).as_bytes());
    let first = ApiRequest::read_mailbox(&mailbox, &read_key, None);
    assert_eq!(
        first.path(),
        format!("/v1/mailboxes/{}/msgs", "0f".repeat(16))
    );

    let delete = ApiRequest::delete_mailbox(&mailbox, &read_key);
    assert_eq!(delete.method(), "DELETE");
    assert_eq!(delete.path(), format!("/v1/mailboxes/{}", "0f".repeat(16)));
    assert_eq!(
        header(&delete, READ_KEY_HEADER)?,
        "a5".repeat(32).as_bytes()
    );
    Ok(())
}

#[test]
fn a_push_relay_config_is_asked_by_its_listed_id() {
    let request = ApiRequest::push_relay_config(pingers_proto::RelayId::new(513));
    assert_eq!(request.method(), "GET");
    assert_eq!(request.path(), "/v1/push-relays/513/config");
    assert_eq!(request.route(), Route::PushRelayConfig);
    assert!(request.body().is_empty());
}

#[test]
fn the_next_push_relay_key_is_asked_by_its_listed_id() {
    let request = ApiRequest::push_relay_next_config(pingers_proto::RelayId::new(513));
    assert_eq!(request.method(), "GET");
    assert_eq!(request.path(), "/v1/push-relays/513/config/next");
    assert_eq!(request.route(), Route::PushRelayNextConfig);
    assert!(request.body().is_empty());
}

#[test]
fn a_mailbox_push_is_replaced_by_its_read_key() -> Result<(), TestError> {
    let mailbox = MailboxId::from_bytes([0x0f; 16]);
    let read_key = MailboxReadKey::from_bytes(Zeroizing::new([0xa5; 32]));
    let update = pingers_proto::MailboxPush { push: None };
    let request = ApiRequest::replace_mailbox_push(&mailbox, &read_key, &update)?;
    assert_eq!(request.method(), "PUT");
    assert_eq!(
        request.path(),
        format!("/v1/mailboxes/{}/push", "0f".repeat(16))
    );
    assert_eq!(
        header(&request, READ_KEY_HEADER)?,
        "a5".repeat(32).as_bytes()
    );
    assert_eq!(request.body(), update.encode()?.as_slice());
    Ok(())
}

#[test]
fn board_cursor_is_a_hex_query_and_the_revoke_key_is_the_raw_body() {
    let page = ApiRequest::board_page(&BoardCursor::from_bytes(vec![0x00, 0xff, 0x10]));
    assert_eq!(page.method(), "GET");
    assert_eq!(page.path(), "/v1/board/page?c=00ff10");

    let revoke = ApiRequest::revoke_lease(
        &LeaseId::from_bytes([0x11; 16]),
        &SecretBytes::new([0x22; 32]),
    );
    assert_eq!(revoke.method(), "DELETE");
    assert_eq!(
        revoke.path(),
        format!("/v1/board/lease/{}", "11".repeat(16))
    );
    assert_eq!(revoke.body(), [0x22; 32]);
}

#[test]
fn cbor_bodies_are_labelled_and_decode_back() -> Result<(), TestError> {
    let message = AppMessage {
        epoch: Epoch::new(3),
        body: MlsMessageBytes::from_bytes(vec![1, 2, 3]),
        delivery: Delivery::Stored,
        wake: Some(Urgency::Quiet),
    };
    let key = key();
    let signer = GroupSigner {
        key: &key,
        now: NOW,
    };
    let request = ApiRequest::post_group_message(&group(), &message, &signer)?;
    assert_eq!(request.method(), "POST");
    assert_eq!(header(&request, "content-type")?, b"application/cbor");
    assert_eq!(AppMessage::decode(request.body())?, message);
    assert_eq!(
        request.path(),
        format!("/v1/groups/{}/msgs", "ab".repeat(32))
    );
    Ok(())
}

fn commit_request() -> CommitRequest {
    CommitRequest {
        epoch: Epoch::new(4),
        commit: MlsMessageBytes::from_bytes(vec![9; 40]),
        next_auth_key: GroupAuthKey::from_bytes([3; 32]),
        state: None,
        subscriptions_added: Vec::new(),
        subscriptions_removed: Vec::new(),
        wake: Urgency::Quiet,
    }
}

#[test]
fn group_requests_are_signed_over_method_path_body_and_time() -> Result<(), TestError> {
    let key = key();
    let signer = GroupSigner {
        key: &key,
        now: NOW,
    };
    let request = ApiRequest::commit(&group(), &commit_request(), &signer)?;
    assert_eq!(request.method(), "POST");
    assert_eq!(
        request.path(),
        format!("/v1/groups/{}/commit", "ab".repeat(32))
    );
    assert_eq!(header(&request, GROUP_TIME_HEADER)?, b"1800000000");
    let signature: [u8; 64] = hex::decode(header(&request, GROUP_SIGNATURE_HEADER)?)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(TestError::BadHeader(GROUP_SIGNATURE_HEADER))?;
    let covered = GroupRequest {
        method: "POST",
        path: request.path(),
        body: request.body(),
        timestamp: NOW,
    };
    verify_group_request(
        &key.public_key(),
        &covered,
        &Signature::from_bytes(signature),
    )?;
    let later = GroupRequest {
        timestamp: UnixSeconds::new(NOW.get().saturating_add(1)),
        ..covered
    };
    assert!(
        verify_group_request(&key.public_key(), &later, &Signature::from_bytes(signature)).is_err()
    );
    Ok(())
}

#[test]
fn every_group_route_is_signed_with_its_own_path() -> Result<(), TestError> {
    let key = key();
    let signer = GroupSigner {
        key: &key,
        now: NOW,
    };
    let id = SubscriptionId::from_bytes([0x5a; 16]);
    let account = pingers_crypto::AccountSigningKey::from_bytes(Zeroizing::new([0x11; 32]));
    let owner = pingers_crypto::SubscriptionOwnerKey::derive(&account, &id)?;
    let subscription = OwnedSubscription { id, owner: &owner };
    let hex_group = "ab".repeat(32);
    let hex_subscription = "5a".repeat(16);
    let cases = [
        (
            ApiRequest::read_group_messages(&group(), None, &signer)?,
            "GET",
            format!("/v1/groups/{hex_group}/msgs?after=0"),
        ),
        (
            ApiRequest::read_group_messages(&group(), Some(SequenceNumber::new(12)), &signer)?,
            "GET",
            format!("/v1/groups/{hex_group}/msgs?after=12"),
        ),
        (
            ApiRequest::group_state(&group(), &signer)?,
            "GET",
            format!("/v1/groups/{hex_group}/state"),
        ),
        (
            ApiRequest::delete_group(&group(), &signer)?,
            "DELETE",
            format!("/v1/groups/{hex_group}"),
        ),
        (
            ApiRequest::delete_subscription(&group(), &subscription, &signer)?,
            "DELETE",
            format!("/v1/groups/{hex_group}/subs/{hex_subscription}"),
        ),
        (
            ApiRequest::live(&group(), Some(&subscription), &signer)?,
            "POST",
            format!("/v1/groups/{hex_group}/live?sub={hex_subscription}"),
        ),
        (
            // Wake-up pushes off: no subscription to name.
            ApiRequest::live(&group(), None, &signer)?,
            "POST",
            format!("/v1/groups/{hex_group}/live"),
        ),
    ];
    for (request, method, path) in cases {
        assert_eq!(request.method(), method);
        assert_eq!(request.path(), path);
        assert!(header(&request, GROUP_SIGNATURE_HEADER).is_ok());
        assert_eq!(header(&request, GROUP_TIME_HEADER)?, b"1800000000");
    }
    Ok(())
}

/// A request that changes a subscription or speaks for it carries its owner's signature over
/// the same method, path, body and time as the group signature; others carry none.
#[test]
fn subscription_requests_are_signed_by_the_owner_too() -> Result<(), TestError> {
    let key = key();
    let signer = GroupSigner {
        key: &key,
        now: NOW,
    };
    let id = SubscriptionId::from_bytes([0x5b; 16]);
    let account = pingers_crypto::AccountSigningKey::from_bytes(Zeroizing::new([0x12; 32]));
    let owner = pingers_crypto::SubscriptionOwnerKey::derive(&account, &id)?;
    let speaking = OwnedSubscription { id, owner: &owner };
    let subscription = Subscription {
        id,
        push: pingers_proto::SealedPush {
            relay: pingers_proto::RelayRef::Listed(pingers_proto::RelayId::new(1)),
            key_id: pingers_proto::PushRelayKeyId::new(1),
            blob: pingers_proto::SealedPushBlob::from_bytes([1; 320]),
            urgency: Urgency::Alert,
            handle: pingers_proto::PushHandle::from_bytes([2; 8]),
        },
        time_to_live: pingers_proto::TimeToLive::new(core::time::Duration::from_hours(1))?,
        owner: owner.public_key(),
    };
    for request in [
        ApiRequest::put_subscription(&group(), &subscription, &signer, &owner)?,
        ApiRequest::delete_subscription(&group(), &speaking, &signer)?,
        ApiRequest::live(&group(), Some(&speaking), &signer)?,
    ] {
        let signature: [u8; 64] = hex::decode(header(&request, SUBSCRIPTION_SIGNATURE_HEADER)?)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(TestError::BadHeader("signature"))?;
        pingers_crypto::verify_subscription_request(
            &owner.public_key(),
            &GroupRequest {
                method: request.method(),
                path: request.path(),
                body: request.body(),
                timestamp: NOW,
            },
            &Signature::from_bytes(signature),
        )?;
    }
    let plain = ApiRequest::live(&group(), None, &signer)?;
    assert!(header(&plain, SUBSCRIPTION_SIGNATURE_HEADER).is_err());
    Ok(())
}

#[test]
fn identified_requests_carry_the_bearer_token() -> Result<(), TestError> {
    let bearer = SecretText::new("secret-bearer".to_owned());
    let delete = ApiRequest::delete_account(&bearer);
    assert_eq!(delete.method(), "DELETE");
    assert_eq!(delete.path(), "/v1/accounts/me");
    assert_eq!(header(&delete, AUTHORIZATION)?, b"Bearer secret-bearer");
    let issue =
        ApiRequest::privacy_pass_issue(&PrivacyPassIssueRequest::new(vec![1, 2, 3])?, &bearer)?;
    assert_eq!(issue.method(), "POST");
    assert_eq!(issue.path(), "/v1/pp/issue");
    assert_eq!(header(&issue, AUTHORIZATION)?, b"Bearer secret-bearer");
    Ok(())
}

#[test]
fn fixed_paths_match_design_d() {
    assert_eq!(ApiRequest::params().path(), "/v1/params");
    assert_eq!(ApiRequest::params().method(), "GET");
    assert_eq!(ApiRequest::privacy_pass_keys().path(), "/v1/pp/keys");
    assert_eq!(
        ApiRequest::rln_leaves(MembershipIndex::new(42)).path(),
        "/v1/rln/leaves?after=42"
    );
}

#[test]
fn debug_prints_the_route_and_nothing_else() {
    let read_key = MailboxReadKey::from_bytes(Zeroizing::new([0xa5; 32]));
    let request = ApiRequest::read_mailbox(&MailboxId::from_bytes([1; 16]), &read_key, None);
    let printed = format!("{request:?}");
    assert!(printed.contains("ReadMailbox"));
    assert!(!printed.contains("a5a5"));
    assert!(!printed.contains("0101"));
}

#[test]
fn the_binary_request_adds_the_credential_after_the_fixed_headers() {
    let read_key = MailboxReadKey::from_bytes(Zeroizing::new([0xa5; 32]));
    let request = ApiRequest::read_mailbox(&MailboxId::from_bytes([1; 16]), &read_key, None);
    let credential = Header {
        name: AUTHORIZATION.to_owned(),
        value: b"x".to_vec(),
    };
    let binary = request.to_binary(Some(credential));
    assert_eq!(binary.method, "GET");
    assert_eq!(binary.path, request.path());
    let names: Vec<&str> = binary
        .headers
        .iter()
        .map(|header| header.name.as_str())
        .collect();
    assert_eq!(names, [READ_KEY_HEADER, AUTHORIZATION]);
}

/// The two credentials an anonymous write may carry, at their real sizes: an RLN proof and a
/// Privacy Pass token.
fn credentials() -> Result<[Header; 2], TestError> {
    let proof = crate::credential::proof_header(&pingers_proto::RlnAuth {
        root: [1; 32],
        scope: pingers_proto::RlnScope::Mailbox,
        bucket: UnixSeconds::new(1_800_000_000).bucket(),
        x: [2; 32],
        y: [3; 32],
        nullifier: [4; 32],
        proof: [5; 128],
    })?;
    let token = pingers_anon::PrivacyPassToken::from_bytes(vec![6; 146]);
    let token = Header {
        name: AUTHORIZATION.to_owned(),
        value: token.authorization().as_bytes().to_vec(),
    };
    Ok([proof, token])
}

/// The encapsulated length of `request` with `credential`, as the relay sees it.
fn sealed_length(
    request: &ApiRequest,
    credential: Option<Header>,
    config: &pingers_ohttp::KeyConfig,
) -> Result<usize, TestError> {
    let mut rng = pingers_testkit::seeded_rng(9);
    let sealed = pingers_ohttp::encapsulate_request_classed(
        config,
        &request.to_binary(credential),
        &mut rng,
    )
    .map_err(|_error| TestError::BadHeader("encapsulation"))?;
    Ok(sealed.message.len())
}

/// The relay sees one length for a knock post, a mailbox read, and a group read and post,
/// whichever credential the write carries (design B.12: requests padded to size classes).
#[test]
fn anonymous_requests_of_one_class_look_alike_to_the_relay() -> Result<(), TestError> {
    let config = pingers_ohttp::GatewayKey::generate(1, &mut pingers_testkit::seeded_rng(8))
        .map_err(|_error| TestError::BadHeader("gateway key"))?
        .config();
    let key = key();
    let signer = GroupSigner {
        key: &key,
        now: UnixSeconds::new(1_800_000_000),
    };
    let mailbox = MailboxId::from_bytes([7; 16]);
    let read_key = MailboxReadKey::from_bytes(Zeroizing::new([8; 32]));
    let knock = Padded::pad(&[9; 600], SizeClass::Bytes1024)?;
    let message = |size: usize| AppMessage {
        epoch: Epoch::new(3),
        body: MlsMessageBytes::from_bytes(vec![0x0a; size]),
        delivery: Delivery::Stored,
        wake: Some(Urgency::Alert),
    };
    let mut lengths = Vec::new();
    for credential in credentials()? {
        // Writes carry a credential: a knock and a group post (a location-sized and a
        // chat-sized MLS message).
        lengths.push(sealed_length(
            &ApiRequest::post_to_mailbox(&mailbox, &knock),
            Some(credential.clone()),
            &config,
        )?);
        for size in [400, 1_300] {
            lengths.push(sealed_length(
                &ApiRequest::post_group_message(&group(), &message(size), &signer)?,
                Some(credential.clone()),
                &config,
            )?);
        }
    }
    // Reads carry none.
    lengths.push(sealed_length(
        &ApiRequest::read_mailbox(&mailbox, &read_key, None),
        None,
        &config,
    )?);
    lengths.push(sealed_length(
        &ApiRequest::read_mailbox(&mailbox, &read_key, Some(SequenceNumber::new(41))),
        None,
        &config,
    )?);
    lengths.push(sealed_length(
        &ApiRequest::read_group_messages(&group(), Some(SequenceNumber::new(12)), &signer)?,
        None,
        &config,
    )?);
    let first_class = pingers_ohttp::REQUEST_CLASSES[0];
    // 7 header, 32 encapsulated key, 16 tag.
    assert!(
        lengths.iter().all(|length| *length == first_class + 55),
        "{lengths:?}"
    );
    Ok(())
}
