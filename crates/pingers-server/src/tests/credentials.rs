// SPDX-License-Identifier: AGPL-3.0-only
//! The credential checks in front of the anonymous routes (Anon: RLN proofs and Privacy Pass
//! tokens; Grp: log-auth signatures and sessions), and the group routes and live channels on
//! pingers-delivery behind them.

use std::time::Duration;

use axum::body::Bytes;
use axum::http::StatusCode;
use pingers_anon::{
    LeafPosition, LeafUpdate, LeafValue, MembershipTree, MessageId, ProofRequest, RlnSecret, prove,
    request_digest,
};
use pingers_crypto::{GroupRequest, LogAuthKeyPair, sign_group_request};
use pingers_ohttp::{BinaryRequest, BinaryResponse, ChunkPosition, Header};
use pingers_proto::{
    AppMessage, CommitRequest, CommitResponse, CreateGroup, Delivery as Mode, Epoch, GroupId,
    GroupMessages, GroupState, LiveFrame, MembershipIndex, MembershipLeafRequest,
    MembershipLeafResponse, MembershipLeaves, MessageAccepted, MlsMessageBytes, Params, Redacted,
    RlnAuth, RlnScope, SealedSnapshot, Urgency, WireDecode as _, WireEncode as _,
};
use pingers_quota::root_epoch;
use pingers_time::{Clock as _, UnixSeconds};
use zeroize::Zeroizing;

use super::wallet::{Wallet, direct};
use super::{
    Echo, LOOPBACK, Running, STEP, Setup, TestError, TestResult, connect, oblivious,
    open_live_with, rng, server_id, start,
};
use crate::live::next_data;
use crate::wire::base64url_encode;
use crate::{DeliveryLive, LiveChannel};

/// Sends `request` through OHTTP and returns the inner response.
async fn call(server: &Running, request: &BinaryRequest) -> Result<BinaryResponse, TestError> {
    let mut client = connect(server.address, LOOPBACK).await?;
    let (status, opened) = oblivious(&mut client, &server.config, request).await?;
    assert_eq!(status, StatusCode::OK);
    opened.ok_or_else(|| "no inner response".into())
}

fn inner(method: &str, path: &str, headers: Vec<Header>, body: Vec<u8>) -> BinaryRequest {
    BinaryRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        body,
    }
}

fn header(name: &str, value: &str) -> Header {
    Header {
        name: name.to_owned(),
        value: value.as_bytes().to_vec(),
    }
}

fn response_header<'a>(response: &'a BinaryResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|header| header.name == name)
        .and_then(|header| core::str::from_utf8(&header.value).ok())
}

async fn started(live: impl LiveChannel) -> Result<Running, TestError> {
    start(Setup::default(), live).await
}

// --- Anon: Privacy Pass ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anonymous_routes_refuse_a_missing_or_malformed_credential() -> TestResult {
    let server = started(Echo).await?;
    let create = CreateGroup {
        group_id: GroupId::from_bytes([1; 32]),
        auth_key: LogAuthKeyPair::from_bytes(Zeroizing::new([1; 32])).public_key(),
        subscriptions: Vec::new(),
        state: None,
    }
    .encode()?;
    let routes = [
        ("POST", "/v1/board/sync", vec![0; 8]),
        ("POST", "/v1/mailboxes", vec![0; 8]),
        ("POST", "/v1/groups?lifetime=meetup", create),
    ];
    for (method, path, body) in &routes {
        for headers in [
            Vec::new(),
            vec![header("authorization", "Basic dXNlcg==")],
            vec![header("authorization", "PingersRln not*base64")],
            vec![header("authorization", "PingersRln AAAA")],
            vec![header("authorization", "PrivateToken token=\"not*base64\"")],
        ] {
            let answer = call(&server, &inner(method, path, headers.clone(), body.clone())).await?;
            assert_eq!(answer.status.get(), 401, "{path} {headers:?}");
        }
    }
    // A well-formed token no issuer key verifies is refused for good.
    let forged = header("authorization", "PrivateToken token=\"AAEC\"");
    let answer = call(
        &server,
        &inner("POST", "/v1/board/sync", vec![forged], vec![0; 8]),
    )
    .await?;
    assert_eq!(answer.status.get(), 403);
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_privacy_pass_token_is_spent_once_on_any_route() -> TestResult {
    let server = started(Echo).await?;
    let wallet = Wallet::fetch(&server, 1, 1).await?;
    let token = wallet.spend()?;
    // Spent on a request that fails afterwards (an undecodable body) is still spent.
    let first = call(
        &server,
        &inner("POST", "/v1/board/sync", vec![token.clone()], vec![1, 2]),
    )
    .await?;
    assert_eq!(first.status.get(), 400);
    let again = call(
        &server,
        &inner("POST", "/v1/mailboxes", vec![token], vec![1, 2]),
    )
    .await?;
    assert_eq!(again.status.get(), 403);
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issuance_needs_a_bearer_and_a_well_formed_batch() -> TestResult {
    let server = started(Echo).await?;
    let wallet = Wallet::fetch(&server, 1, 0).await?;
    for bearer in ["", "Bearer 00", "Bearer"] {
        let answer = direct(&server, "POST /v1/pp/issue", Some(bearer), Vec::new()).await?;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{bearer}");
    }
    let malformed = pingers_proto::PrivacyPassIssueRequest::new(vec![0; 54])?.encode()?;
    let refused = direct(
        &server,
        "POST /v1/pp/issue",
        Some(&wallet.bearer),
        malformed,
    )
    .await?;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(Wallet::issue(&server, &wallet.bearer, 2).await?.len(), 2);
    // The daily allowance itself is pingers-quota's (its tests issue the 300).
    server.stop().await
}

// --- Anon: RLN ---

/// A device with a leaf in the membership tree.
struct Member {
    secret: RlnSecret,
    position: u32,
}

/// Registers a leaf for `wallet`'s account and moves the clock into the root that holds it.
async fn enrol(server: &Running, wallet: &Wallet) -> Result<Member, TestError> {
    let secret = RlnSecret::generate(&mut rng(21))?;
    let request = MembershipLeafRequest {
        id_commitment: Redacted::new(*secret.id_commitment()?.as_bytes()),
    };
    let answer = direct(
        server,
        "POST /v1/rln/leaf",
        Some(&wallet.bearer),
        request.encode()?,
    )
    .await?;
    assert_eq!(answer.status, StatusCode::OK);
    let leaf = MembershipLeafResponse::decode(&answer.body)?;
    let now = server.clock.now();
    let next = u64::from(root_epoch(now)?.get())
        .saturating_add(1)
        .saturating_mul(3600);
    server
        .clock
        .advance(UnixSeconds::new(next).saturating_duration_since(now));
    Ok(Member {
        secret,
        position: leaf.index.get(),
    })
}

/// The leaf list, fetched anonymously as a device does.
async fn leaves(server: &Running) -> Result<MembershipLeaves, TestError> {
    let answer = call(
        server,
        &inner("GET", "/v1/rln/leaves?after=0", Vec::new(), Vec::new()),
    )
    .await?;
    assert_eq!(answer.status.get(), 200);
    Ok(MembershipLeaves::decode(&answer.body)?)
}

/// What one proof is for.
struct Proving<'a> {
    scope: RlnScope,
    request: &'a BinaryRequest,
    slot: u8,
}

/// The `Authorization: PingersRln` header for `proving`, proved off the async workers.
async fn rln_header(
    server: &Running,
    member: &Member,
    proving: &Proving<'_>,
) -> Result<Header, TestError> {
    let list = leaves(server).await?;
    let mut tree = MembershipTree::empty()?;
    let updates: Vec<LeafUpdate> = list
        .leaves
        .iter()
        .map(|leaf| LeafUpdate::Appended(LeafValue::from_bytes(*leaf)))
        .collect();
    tree.apply(&updates)?;
    let path = tree.path(LeafPosition::new(member.position))?;
    let digest = request_digest(
        &proving.request.method,
        &proving.request.path,
        &proving.request.body,
    )?;
    let secret = RlnSecret::from_bytes(Zeroizing::new(*member.secret.expose_bytes()))?;
    let scope = proving.scope;
    let bucket = server.clock.now().bucket();
    let slot = MessageId::new(proving.slot)?;
    let auth: RlnAuth = tokio::task::spawn_blocking(move || {
        prove(
            &ProofRequest {
                secret: &secret,
                path: &path,
                root: tree.root(),
                server: &server_id()?,
                scope,
                bucket,
                message_id: slot,
                digest,
            },
            &mut rng(31),
        )
        .map_err(TestError::from)
    })
    .await??;
    Ok(header(
        "authorization",
        &format!("PingersRln {}", base64url_encode(&auth.encode()?)),
    ))
}

fn board_sync(server: &Running, fill: u8) -> Result<BinaryRequest, TestError> {
    use pingers_crypto::{RevokeKey, revoke_commitment};
    use pingers_proto::{
        BoardSyncRequest, Lease, ReadBoard, Record, RecordRandomness, RecordTag, SealedBeacon,
    };
    let record = |fill: u8| Record {
        randomness: RecordRandomness::from_bytes([fill; 16]),
        tag: RecordTag::from_bytes([fill; 16]),
        sealed: SealedBeacon::from_bytes([fill; 64]),
    };
    let body = BoardSyncRequest {
        lease: Lease {
            start: server.clock.now().bucket(),
            records: [0, 1, 2, 3].map(|slot| record(fill.wrapping_add(slot))),
            revoke_commitment: revoke_commitment(&RevokeKey::from_bytes(Zeroizing::new(
                [fill; 32],
            ))),
        },
        replaces: None,
        read: ReadBoard::Skip,
    }
    .encode()?;
    Ok(inner("POST", "/v1/board/sync", Vec::new(), body))
}

fn with(mut request: BinaryRequest, header: Header) -> BinaryRequest {
    request.headers.push(header);
    request
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rln_proofs_are_checked_for_scope_request_bucket_and_nullifier() -> TestResult {
    let server = started(Echo).await?;
    let wallet = Wallet::fetch(&server, 1, 0).await?;
    let member = enrol(&server, &wallet).await?;
    let first = board_sync(&server, 1)?;
    let proof = rln_header(
        &server,
        &member,
        &Proving {
            scope: RlnScope::Board,
            request: &first,
            slot: 0,
        },
    )
    .await?;
    let admitted = call(&server, &with(first.clone(), proof.clone())).await?;
    assert_eq!(admitted.status.get(), 200);
    // The same request again is a retransmission, served again.
    let replayed = call(&server, &with(first.clone(), proof.clone())).await?;
    assert_eq!(replayed.status.get(), 200);
    // The proof is bound to its request: another body does not match `x`.
    let other = board_sync(&server, 2)?;
    let mismatched = call(&server, &with(other.clone(), proof.clone())).await?;
    assert_eq!(mismatched.status.get(), 403);
    // The same slot for another request reuses the nullifier.
    let reused = rln_header(
        &server,
        &member,
        &Proving {
            scope: RlnScope::Board,
            request: &other,
            slot: 0,
        },
    )
    .await?;
    let slot_reused = call(&server, &with(other.clone(), reused)).await?;
    assert_eq!(slot_reused.status.get(), 403);
    // A proof of another scope does not buy a board sync.
    let mailbox_scoped = rln_header(
        &server,
        &member,
        &Proving {
            scope: RlnScope::Mailbox,
            request: &other,
            slot: 1,
        },
    )
    .await?;
    let wrong_scope = call(&server, &with(other, mailbox_scoped)).await?;
    assert_eq!(wrong_scope.status.get(), 403);
    // Once the bucket and its grace have passed, the proof is stale: prove again.
    server
        .clock
        .advance(pingers_time::Bucket::LENGTH.saturating_add(Duration::from_secs(61)));
    let stale = call(&server, &with(first, proof)).await?;
    assert_eq!(stale.status.get(), 401);
    server.stop().await
}

// --- Grp and the group routes ---

/// A group's creator: its log-auth key for the current epoch and its session.
struct Creator {
    group: GroupId,
    key: LogAuthKeyPair,
    session: Option<String>,
    clock: pingers_time::ManualClock,
}

fn group_path(group: &GroupId, rest: &str) -> String {
    format!("/v1/groups/{}{rest}", hex::encode(group.as_bytes()))
}

/// Signs `request` with `key` at `timestamp`.
fn signed_at(
    key: &LogAuthKeyPair,
    mut request: BinaryRequest,
    timestamp: UnixSeconds,
) -> Result<BinaryRequest, TestError> {
    let signature = sign_group_request(
        key,
        &GroupRequest {
            method: &request.method,
            path: &request.path,
            body: &request.body,
            timestamp,
        },
    )?;
    request.headers.push(header(
        "pingers-group-timestamp",
        &timestamp.get().to_string(),
    ));
    request.headers.push(header(
        "pingers-group-signature",
        &hex::encode(signature.as_bytes()),
    ));
    Ok(request)
}

impl Creator {
    /// A Grp request now, with the session if there is one.
    fn request(&self, method: &str, rest: &str, body: Vec<u8>) -> Result<BinaryRequest, TestError> {
        let mut request = inner(method, &group_path(&self.group, rest), Vec::new(), body);
        if let Some(session) = &self.session {
            request
                .headers
                .push(header("pingers-group-session", session));
        }
        signed_at(&self.key, request, self.clock.now())
    }
}

async fn create_group(server: &Running, wallet: &Wallet, fill: u8) -> Result<Creator, TestError> {
    let key = LogAuthKeyPair::from_bytes(Zeroizing::new([fill; 32]));
    let group = GroupId::from_bytes([fill; 32]);
    let body = CreateGroup {
        group_id: group,
        auth_key: key.public_key(),
        subscriptions: Vec::new(),
        state: Some(SealedSnapshot::from_bytes(vec![0xE0])),
    }
    .encode()?;
    let created = call(
        server,
        &wallet.pay(inner(
            "POST",
            "/v1/groups?lifetime=meetup",
            Vec::new(),
            body,
        ))?,
    )
    .await?;
    assert_eq!(created.status.get(), 201);
    Ok(Creator {
        group,
        key,
        session: response_header(&created, "pingers-group-session").map(str::to_owned),
        clock: server.clock.clone(),
    })
}

fn commit_body(epoch: u64, next_key: &LogAuthKeyPair) -> Result<Vec<u8>, TestError> {
    Ok(CommitRequest {
        epoch: Epoch::new(epoch),
        commit: MlsMessageBytes::from_bytes(vec![0xC0; 4]),
        next_auth_key: next_key.public_key(),
        state: None,
        subscriptions_added: Vec::new(),
        subscriptions_removed: Vec::new(),
        wake: Urgency::Quiet,
    }
    .encode()?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_creation_needs_a_lifetime_class_and_a_credential() -> TestResult {
    let server = started(DeliveryLive).await?;
    let wallet = Wallet::fetch(&server, 1, 3).await?;
    let body = CreateGroup {
        group_id: GroupId::from_bytes([4; 32]),
        auth_key: LogAuthKeyPair::from_bytes(Zeroizing::new([4; 32])).public_key(),
        subscriptions: Vec::new(),
        state: None,
    }
    .encode()?;
    for path in ["/v1/groups", "/v1/groups?lifetime=forever"] {
        let refused = call(
            &server,
            &wallet.pay(inner("POST", path, Vec::new(), body.clone()))?,
        )
        .await?;
        assert_eq!(refused.status.get(), 400, "{path}");
    }
    let party = call(
        &server,
        &wallet.pay(inner("POST", "/v1/groups?lifetime=party", Vec::new(), body))?,
    )
    .await?;
    assert_eq!(party.status.get(), 201);
    assert!(response_header(&party, "pingers-group-session").is_some());
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_requests_need_a_fresh_signature_by_the_current_key() -> TestResult {
    let server = started(DeliveryLive).await?;
    let wallet = Wallet::fetch(&server, 1, 4).await?;
    let creator = create_group(&server, &wallet, 5).await?;
    let path = group_path(&creator.group, "/state");
    let session = header(
        "pingers-group-session",
        creator.session.as_deref().ok_or("no session")?,
    );
    let now = server.clock.now();
    let other_key = LogAuthKeyPair::from_bytes(Zeroizing::new([6; 32]));
    let skew = Duration::from_secs(121);
    let unsigned = inner("GET", &path, vec![session.clone()], Vec::new());
    for (request, expected) in [
        (unsigned.clone(), 401),
        (signed_at(&other_key, unsigned.clone(), now)?, 403),
        (
            signed_at(
                &creator.key,
                unsigned.clone(),
                now.checked_sub(skew).ok_or("time")?,
            )?,
            401,
        ),
        (
            signed_at(
                &creator.key,
                unsigned.clone(),
                now.checked_add(skew).ok_or("time")?,
            )?,
            401,
        ),
        (
            signed_at(
                &creator.key,
                unsigned.clone(),
                now.checked_add(Duration::from_secs(120)).ok_or("time")?,
            )?,
            200,
        ),
        (signed_at(&creator.key, unsigned, now)?, 200),
    ] {
        let answer = call(&server, &request).await?;
        assert_eq!(answer.status.get(), expected);
    }
    // Without the session (or with another group's), the request needs a credential again.
    let other = create_group(&server, &wallet, 7).await?;
    let foreign = header(
        "pingers-group-session",
        other.session.as_deref().ok_or("no session")?,
    );
    for headers in [Vec::new(), vec![foreign]] {
        let request = signed_at(&creator.key, inner("GET", &path, headers, Vec::new()), now)?;
        assert_eq!(call(&server, &request).await?.status.get(), 401);
    }
    // A credential instead of the session starts a new session.
    let paid = signed_at(
        &creator.key,
        wallet.pay(inner("GET", &path, Vec::new(), Vec::new()))?,
        now,
    )?;
    let answer = call(&server, &paid).await?;
    assert_eq!(answer.status.get(), 200);
    assert!(response_header(&answer, "pingers-group-session").is_some());
    // Sessions end with the bucket.
    server.clock.advance(pingers_time::Bucket::LENGTH);
    let late = creator.request("GET", "/state", Vec::new())?;
    assert_eq!(call(&server, &late).await?.status.get(), 401);
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_logs_take_one_commit_per_epoch_and_rotate_the_key() -> TestResult {
    let server = started(DeliveryLive).await?;
    let wallet = Wallet::fetch(&server, 1, 1).await?;
    let mut creator = create_group(&server, &wallet, 8).await?;
    let state = call(&server, &creator.request("GET", "/state", Vec::new())?).await?;
    assert_eq!(state.status.get(), 200);
    assert_eq!(GroupState::decode(&state.body)?.epoch, Epoch::new(0));

    let next = LogAuthKeyPair::from_bytes(Zeroizing::new([9; 32]));
    let won = call(
        &server,
        &creator.request("POST", "/commit", commit_body(0, &next)?)?,
    )
    .await?;
    assert_eq!(won.status.get(), 200);
    assert!(matches!(
        CommitResponse::decode(&won.body)?,
        CommitResponse::Accepted { .. }
    ));
    // The old key no longer opens the log; the next one does, and a second commit for epoch 0
    // loses with the head epoch.
    let stale_key = call(&server, &creator.request("GET", "/msgs", Vec::new())?).await?;
    assert_eq!(stale_key.status.get(), 403);
    creator.key = next;
    let lost = call(
        &server,
        &creator.request(
            "POST",
            "/commit",
            commit_body(0, &LogAuthKeyPair::from_bytes(Zeroizing::new([10; 32])))?,
        )?,
    )
    .await?;
    assert_eq!(lost.status.get(), 409);
    assert_eq!(
        CommitResponse::decode(&lost.body)?,
        CommitResponse::Conflict {
            head_epoch: Epoch::new(1)
        }
    );

    let message = AppMessage {
        epoch: Epoch::new(1),
        body: MlsMessageBytes::from_bytes(vec![0xA0; 3]),
        delivery: Mode::Stored,
        wake: None,
    };
    let posted = call(
        &server,
        &creator.request("POST", "/msgs", message.encode()?)?,
    )
    .await?;
    assert_eq!(posted.status.get(), 200);
    assert_eq!(MessageAccepted::decode(&posted.body)?.sequence.get(), 2);
    let ephemeral = AppMessage {
        delivery: Mode::Ephemeral,
        ..message
    };
    let sent = call(
        &server,
        &creator.request("POST", "/msgs", ephemeral.encode()?)?,
    )
    .await?;
    assert_eq!(sent.status.get(), 202);
    let read = call(
        &server,
        &creator.request("GET", "/msgs?after=1", Vec::new())?,
    )
    .await?;
    assert_eq!(read.status.get(), 200);
    let messages = GroupMessages::decode(&read.body)?;
    assert_eq!(messages.head_epoch, Epoch::new(1));
    assert_eq!(messages.messages.len(), 1);
    let bad_after = call(
        &server,
        &creator.request("GET", "/msgs?after=x", Vec::new())?,
    )
    .await?;
    assert_eq!(bad_after.status.get(), 400);
    // The commit carried no snapshot, so the last one stays.
    let kept = call(&server, &creator.request("GET", "/state", Vec::new())?).await?;
    assert_eq!(kept.status.get(), 200);
    assert_eq!(GroupState::decode(&kept.body)?.epoch, Epoch::new(0));

    let deleted = call(&server, &creator.request("DELETE", "", Vec::new())?).await?;
    assert_eq!(deleted.status.get(), 204);
    let gone = call(&server, &creator.request("GET", "/state", Vec::new())?).await?;
    assert_eq!(gone.status.get(), 404);
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_are_put_and_deleted_by_their_own_id() -> TestResult {
    use pingers_proto::{
        PushHandle, PushRelayKeyId, RelayId, RelayRef, SealedPush, SealedPushBlob, Subscription,
        SubscriptionId, TimeToLive,
    };
    let server = started(DeliveryLive).await?;
    let wallet = Wallet::fetch(&server, 1, 1).await?;
    let creator = create_group(&server, &wallet, 11).await?;
    let subscription = Subscription {
        id: SubscriptionId::from_bytes([1; 16]),
        push: SealedPush {
            relay: RelayRef::Listed(RelayId::new(1)),
            key_id: PushRelayKeyId::new(1),
            blob: SealedPushBlob::from_bytes([2; 320]),
            urgency: Urgency::Quiet,
            handle: PushHandle::from_bytes([3; 8]),
        },
        time_to_live: TimeToLive::new(Duration::from_hours(24))?,
    }
    .encode()?;
    let own = format!("/subs/{}", hex::encode([1_u8; 16]));
    let other = format!("/subs/{}", hex::encode([2_u8; 16]));
    for (method, rest, body, expected) in [
        ("PUT", other.as_str(), subscription.clone(), 400),
        ("PUT", "/subs/zz", subscription.clone(), 400),
        ("PUT", own.as_str(), subscription, 204),
        ("DELETE", own.as_str(), Vec::new(), 204),
        ("DELETE", "/subs/zz", Vec::new(), 400),
    ] {
        let answer = call(&server, &creator.request(method, rest, body)?).await?;
        assert_eq!(answer.status.get(), expected, "{method} {rest}");
    }
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_channels_fan_out_frames_and_commits_to_the_other_members() -> TestResult {
    let server = started(DeliveryLive).await?;
    let wallet = Wallet::fetch(&server, 1, 1).await?;
    let creator = create_group(&server, &wallet, 12).await?;
    let live_head = |sub: u8| {
        creator.request(
            "POST",
            &format!("/live?sub={}", hex::encode([sub; 16])),
            Vec::new(),
        )
    };
    let (a_sender, mut a_client, mut a_body) = open_live_with(&server, &live_head(1)?).await?;
    // An exchange is answered only once its channel is attached, so both are attached now.
    let (_b_sender, b_client, mut b_body) = open_live_with(&server, &live_head(2)?).await?;
    let frame = LiveFrame {
        epoch: Epoch::new(0),
        body: MlsMessageBytes::from_bytes(vec![0x55; 40]),
        delivery: Mode::Ephemeral,
    }
    .encode()?;
    a_sender.send(Bytes::from(
        a_client.seal_chunk(&frame, ChunkPosition::NonFinal)?,
    ))?;
    let mut b_decoder = b_client.response_decoder();
    let received = next_payload(&mut b_body, &mut b_decoder).await?;
    assert_eq!(received, frame);

    // A commit posted by request reaches both channels as a stored frame.
    let next = LogAuthKeyPair::from_bytes(Zeroizing::new([13; 32]));
    let committed = call(
        &server,
        &creator.request("POST", "/commit", commit_body(0, &next)?)?,
    )
    .await?;
    assert_eq!(committed.status.get(), 200);
    let mut a_decoder = a_client.response_decoder();
    for (body, decoder) in [(&mut a_body, &mut a_decoder), (&mut b_body, &mut b_decoder)] {
        let payload = next_payload(body, decoder).await?;
        let frame = LiveFrame::decode(&payload)?;
        assert_eq!(frame.delivery, Mode::Stored);
        assert_eq!(frame.body, MlsMessageBytes::from_bytes(vec![0xC0; 4]));
    }
    server.stop().await
}

/// The next non-keep-alive payload on a live response.
async fn next_payload(
    body: &mut axum::body::Body,
    decoder: &mut pingers_ohttp::ResponseChunkDecoder,
) -> Result<Vec<u8>, TestError> {
    loop {
        let bytes = tokio::time::timeout(STEP, next_data(body))
            .await??
            .ok_or("the channel ended")?;
        for chunk in decoder.feed(&bytes)? {
            if !chunk.payload.is_empty() && chunk.payload != crate::KEEP_ALIVE_CHUNK {
                return Ok(chunk.payload);
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accounts_register_authenticate_and_delete() -> TestResult {
    let server = started(Echo).await?;
    let first = Wallet::fetch(&server, 1, 0).await?;
    let member = enrol(&server, &first).await?;
    // Enrolling moved the clock an hour on: authenticate again for a fresh bearer.
    let wallet = Wallet::fetch(&server, 1, 0).await?;
    let list = leaves(&server).await?;
    assert_eq!(list.leaves.len(), 1);
    assert_eq!(member.position, 0);
    assert_eq!(list.next, MembershipIndex::new(1));
    let deleted = direct(
        &server,
        "DELETE /v1/accounts/me",
        Some(&wallet.bearer),
        Vec::new(),
    )
    .await?;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let again = direct(
        &server,
        "DELETE /v1/accounts/me",
        Some(&wallet.bearer),
        Vec::new(),
    )
    .await?;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED);
    // The leaf is zeroed at the next root.
    let now = server.clock.now();
    server.clock.advance(Duration::from_hours(1));
    assert!(server.clock.now() > now);
    let list = leaves(&server).await?;
    assert_eq!(list.zeroed, vec![MembershipIndex::new(0)]);
    let params = direct(&server, "GET /v1/params", None, Vec::new()).await?;
    let params = Params::decode(&params.body)?;
    assert_eq!(params.server_time, server.clock.now());
    assert_eq!(params.server_operator, "https://pingers.test");
    server.stop().await
}
