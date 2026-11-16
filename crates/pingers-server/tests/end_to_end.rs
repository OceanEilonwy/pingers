// SPDX-License-Identifier: AGPL-3.0-only
//! A device's day through the in-process harness: register, fetch and spend Privacy Pass
//! tokens, prove an RLN board sync, race two commits for one epoch, see a quiet push arrive
//! after its delay, and see a `410 Gone` from the push relay remove the subscription.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::{Body, HttpBody as _};
use hyper::client::conn::http2::SendRequest;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use pingers_anon::{
    LeafPosition, LeafUpdate, LeafValue, MembershipTree, MessageId, PrivacyPassToken, ProofRequest,
    RlnSecret, TokenCount, begin_token_issuance, prove, request_digest,
};
use pingers_crypto::{
    AccountSigningKey, GroupRequest, LogAuthKeyPair, MailboxReadKey, PushRelayPublicKey,
    PushTokenToSeal, RevokeKey, mailbox_id, push_bind_for_subscription, revoke_commitment,
    seal_push_token, sign_auth_challenge, sign_group_request, sign_registration,
};
use pingers_delivery::MAX_QUIET_DELAY;
use pingers_ohttp::{BinaryRequest, BinaryResponse, Header, KeyConfig, encapsulate_request};
use pingers_proto::{
    AppMessage, AuthRequest, AuthResponse, BoardSyncRequest, BoardSyncResponse, CommitRequest,
    CommitResponse, CreateGroup, Delivery, Epoch, GroupId, Lease, MailboxSpec,
    MembershipLeafRequest, MembershipLeafResponse, MembershipLeaves, MlsMessageBytes,
    PrivacyPassIssueRequest, PrivacyPassIssueResponse, PrivacyPassKeys, PushClass, PushHandle,
    PushPlatform, PushRelayKeyId, PushToken, PushTokenPlaintext, ReadBoard, Record,
    RecordRandomness, RecordTag, Redacted, RegisterRequest, RegisterResponse, RelayRef, RlnScope,
    SealedBeacon, SealedPush, ServerId, Subscription, SubscriptionId, TimeToLive, Urgency,
    WireDecode, WireEncode,
};
use pingers_testkit::{
    CapturedPush, HARNESS_PUSH_RELAY_ID, HarnessConfig, InProcessHarness, seeded_rng,
};
use pingers_time::{Clock as _, ManualClock};
use zeroize::Zeroizing;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// How long any single network step may take.
const STEP: Duration = Duration::from_secs(10);

/// A device talking to the harness: identified requests direct to the server, anonymous ones
/// through the OHTTP relay.
struct Device {
    server: SocketAddr,
    relay: SocketAddr,
    key_config: KeyConfig,
    server_id: ServerId,
    clock: ManualClock,
    seed: u64,
}

async fn connect(address: SocketAddr) -> Result<SendRequest<Body>, TestError> {
    let stream = tokio::net::TcpStream::connect(address).await?;
    let (sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(sender)
}

async fn exchange(
    address: SocketAddr,
    request: Request<Body>,
) -> Result<(StatusCode, Vec<u8>), TestError> {
    let mut client = connect(address).await?;
    let response = tokio::time::timeout(STEP, client.send_request(request)).await??;
    let status = response.status();
    let mut body = response.into_body();
    let mut all = Vec::new();
    while let Some(frame) = tokio::time::timeout(
        STEP,
        std::future::poll_fn(|context| std::pin::Pin::new(&mut body).poll_frame(context)),
    )
    .await?
    {
        if let Ok(data) = frame?.into_data() {
            all.extend_from_slice(&data);
        }
    }
    Ok((status, all))
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

impl Device {
    fn new(harness: &InProcessHarness, clock: &ManualClock) -> Result<Self, TestError> {
        let endpoints = harness.endpoints();
        Ok(Self {
            server: endpoints.server,
            relay: endpoints.ohttp_relay,
            key_config: KeyConfig::decode(&endpoints.ohttp_key_config)?,
            server_id: ServerId::new(endpoints.server_id.clone())?,
            clock: clock.clone(),
            seed: 1,
        })
    }

    /// A direct (identified or public) request.
    async fn direct(
        &self,
        route: (Method, &str),
        bearer: Option<&str>,
        body: Vec<u8>,
    ) -> Result<(StatusCode, Vec<u8>), TestError> {
        let (method, path) = route;
        let mut request = Request::builder()
            .method(method)
            .uri(format!("http://server{path}"));
        if let Some(bearer) = bearer {
            request = request.header(AUTHORIZATION, bearer);
        }
        exchange(self.server, request.body(Body::from(body))?).await
    }

    /// An anonymous request through the relay.
    async fn oblivious(&mut self, inner: &BinaryRequest) -> Result<BinaryResponse, TestError> {
        self.seed = self.seed.wrapping_add(1);
        let encapsulated =
            encapsulate_request(&self.key_config, inner, &mut seeded_rng(self.seed))?;
        let request = Request::builder()
            .method(Method::POST)
            .uri("http://relay/ohttp")
            .header(CONTENT_TYPE, "message/ohttp-req")
            .body(Body::from(encapsulated.message))?;
        let (status, sealed) = exchange(self.relay, request).await?;
        assert_eq!(status, StatusCode::OK);
        Ok(encapsulated.response_context.open(&sealed)?)
    }

    /// Registers the account and returns its bearer.
    async fn register(&self, signing: &AccountSigningKey) -> Result<String, TestError> {
        let registration = RegisterRequest {
            signature_key: *signing.public_key().as_bytes(),
            proof: *sign_registration(signing, &self.server_id)?.as_bytes(),
            registration_code: None,
        };
        let (status, body) = self
            .direct((Method::POST, "/v1/accounts"), None, registration.encode()?)
            .await?;
        assert_eq!(status, StatusCode::OK);
        let auth = AuthRequest {
            account_id: RegisterResponse::decode(&body)?.account_id,
            nonce_signature: *sign_auth_challenge(
                signing,
                &self.server_id,
                self.clock.now().bucket(),
            )?
            .as_bytes(),
        };
        let (status, body) = self
            .direct((Method::POST, "/v1/auth"), None, auth.encode()?)
            .await?;
        assert_eq!(status, StatusCode::OK);
        Ok(format!(
            "Bearer {}",
            AuthResponse::decode(&body)?.bearer.expose()
        ))
    }

    /// Fetches a batch of Privacy Pass tokens.
    async fn tokens(&self, bearer: &str, count: u16) -> Result<Vec<PrivacyPassToken>, TestError> {
        let (status, body) = self
            .direct((Method::GET, "/v1/pp/keys"), None, Vec::new())
            .await?;
        assert_eq!(status, StatusCode::OK);
        let keys = PrivacyPassKeys::decode(&body)?;
        let key = keys.keys.last().ok_or("no issuer key")?;
        let pending = begin_token_issuance(
            &key.public_key,
            TokenCount::new(count)?,
            &mut seeded_rng(40),
        )?;
        let (status, body) = self
            .direct(
                (Method::POST, "/v1/pp/issue"),
                Some(bearer),
                PrivacyPassIssueRequest::new(pending.request().to_vec())?.encode()?,
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        Ok(pending.finish(PrivacyPassIssueResponse::decode(&body)?.token_response())?)
    }
}

fn spend(tokens: &mut Vec<PrivacyPassToken>) -> Result<Header, TestError> {
    let token = tokens.pop().ok_or("out of tokens")?;
    Ok(header("authorization", token.authorization().as_str()))
}

/// Unpadded base64url, as the device encodes its RLN header.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut group = [0_u8; 4];
        for (slot, byte) in group.iter_mut().skip(1).zip(chunk) {
            *slot = *byte;
        }
        let bits = u32::from_be_bytes(group);
        for shift in [18_u32, 12, 6, 0]
            .into_iter()
            .take(chunk.len().saturating_add(1))
        {
            let index = usize::try_from((bits >> shift) & 0x3f).unwrap_or(0);
            out.push(ALPHABET.get(index).map_or('A', |byte| char::from(*byte)));
        }
    }
    out
}

fn board_sync(clock: &ManualClock) -> Result<BinaryRequest, TestError> {
    let record = |fill: u8| Record {
        randomness: RecordRandomness::from_bytes([fill; 16]),
        tag: RecordTag::from_bytes([fill; 16]),
        sealed: SealedBeacon::from_bytes([fill; 64]),
    };
    Ok(BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/board/sync".to_owned(),
        headers: Vec::new(),
        body: BoardSyncRequest {
            lease: Lease {
                start: clock.now().bucket(),
                records: [1, 2, 3, 4].map(record),
                revoke_commitment: revoke_commitment(&RevokeKey::from_bytes(Zeroizing::new(
                    [9; 32],
                ))),
            },
            replaces: None,
            read: ReadBoard::Read,
        }
        .encode()?,
    })
}

/// A Grp request: signed with `key` now, carrying `session`.
/// Who signs a group request and the session it carries.
struct GroupAuth<'a> {
    key: &'a LogAuthKeyPair,
    session: &'a str,
}

fn group_request(
    clock: &ManualClock,
    auth: &GroupAuth<'_>,
    route: (&str, String),
    body: Vec<u8>,
) -> Result<BinaryRequest, TestError> {
    let GroupAuth { key, session } = *auth;
    let (method, path) = route;
    let now = clock.now();
    let signature = sign_group_request(
        key,
        &GroupRequest {
            method,
            path: &path,
            body: &body,
            timestamp: now,
        },
    )?;
    Ok(BinaryRequest {
        method: method.to_owned(),
        path,
        headers: vec![
            header("pingers-group-session", session),
            header("pingers-group-time", &now.get().to_string()),
            header(
                "pingers-group-signature",
                &hex::encode(signature.as_bytes()),
            ),
        ],
        body,
    })
}

/// A subscription whose push wakes `token` through the harness relay.
fn subscription(
    harness: &InProcessHarness,
    clock: &ManualClock,
    fill: u8,
    token: &PushToken,
) -> Result<Subscription, TestError> {
    let id = SubscriptionId::from_bytes([fill; 16]);
    let relay_key =
        PushRelayPublicKey::new(1, harness.endpoints().push_relay_key.as_slice().try_into()?);
    let plaintext = PushTokenPlaintext {
        platform: PushPlatform::UnifiedPush,
        class: PushClass::Standard,
        token: token.clone(),
        not_after: clock
            .now()
            .checked_add(Duration::from_hours(24))
            .ok_or("time")?,
    };
    let blob = seal_push_token(
        &relay_key,
        &PushTokenToSeal {
            bind: push_bind_for_subscription(&id),
            plaintext: &plaintext,
        },
        &mut seeded_rng(u64::from(fill)),
    )?;
    Ok(Subscription {
        id,
        push: SealedPush {
            relay: RelayRef::Listed(HARNESS_PUSH_RELAY_ID),
            key_id: PushRelayKeyId::new(1),
            blob,
            urgency: Urgency::Quiet,
            handle: PushHandle::from_bytes([fill; 8]),
        },
        time_to_live: TimeToLive::new(Duration::from_hours(2))?,
        owner: pingers_proto::SubscriptionOwner::from_bytes([7; 32]),
    })
}

/// Waits until `ready` holds for the harness's captured pushes, which arrive on the harness's
/// own thread; `patience` only bounds a hang, nothing waits for time to pass.
async fn until(
    harness: &InProcessHarness,
    patience: Duration,
    ready: impl Fn(&InProcessHarness) -> bool,
) -> Result<(), TestError> {
    tokio::time::timeout(patience, async {
        while !ready(harness) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| "the push did not arrive".into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one device's day, step by step")]
async fn a_device_registers_proves_commits_and_is_woken() -> TestResult {
    let clock = pingers_testkit::test_clock();
    let harness = InProcessHarness::start(&HarnessConfig {
        clock: clock.clone(),
        ..HarnessConfig::default()
    })?;
    let mut device = Device::new(&harness, &clock)?;

    // Register, fetch a batch of tokens, and spend one on a mailbox.
    let signing = AccountSigningKey::from_bytes(Zeroizing::new([1; 32]));
    let bearer = device.register(&signing).await?;
    let mut tokens = device.tokens(&bearer, 8).await?;
    assert_eq!(tokens.len(), 8);
    let read_key = MailboxReadKey::from_bytes(Zeroizing::new([2; 32]));
    let mailbox = BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/mailboxes".to_owned(),
        headers: vec![spend(&mut tokens)?],
        body: MailboxSpec {
            mailbox_id: mailbox_id(&read_key),
            time_to_live: TimeToLive::new(Duration::from_hours(1))?,
            push: None,
        }
        .encode()?,
    };
    assert_eq!(device.oblivious(&mailbox).await?.status.get(), 201);
    // The token is spent: the same request again is refused.
    assert_eq!(device.oblivious(&mailbox).await?.status.get(), 403);

    // Register an RLN leaf; an hour later it is in the accepted root, and a board sync proved
    // against the public leaf list is admitted.
    let secret = RlnSecret::generate(&mut seeded_rng(3))?;
    let leaf = MembershipLeafRequest {
        id_commitment: Redacted::new(*secret.id_commitment()?.as_bytes()),
    };
    let (status, body) = device
        .direct(
            (Method::POST, "/v1/rln/leaf"),
            Some(&bearer),
            leaf.encode()?,
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    let position = MembershipLeafResponse::decode(&body)?.index.get();
    clock.advance(Duration::from_hours(1));
    let leaves = device
        .oblivious(&BinaryRequest {
            method: "GET".to_owned(),
            path: "/v1/rln/leaves?after=0".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        })
        .await?;
    let list = MembershipLeaves::decode(&leaves.body)?;
    let mut tree = MembershipTree::empty()?;
    tree.apply(
        &list
            .leaves
            .iter()
            .map(|leaf| LeafUpdate::Appended(LeafValue::from_bytes(*leaf)))
            .collect::<Vec<_>>(),
    )?;
    let mut sync = board_sync(&clock)?;
    let digest = request_digest(&sync.method, &sync.path, &sync.body)?;
    let path = tree.path(LeafPosition::new(position))?;
    let root = tree.root();
    let server_id = device.server_id.clone();
    let bucket = clock.now().bucket();
    let auth = tokio::task::spawn_blocking(move || {
        prove(
            &ProofRequest {
                secret: &secret,
                path: &path,
                root,
                server: &server_id,
                scope: RlnScope::Board,
                bucket,
                message_id: MessageId::new(0)?,
                digest,
            },
            &mut seeded_rng(4),
        )
    })
    .await??;
    sync.headers.push(header(
        "authorization",
        &format!("PingersRln {}", base64url(&auth.encode()?)),
    ));
    let synced = device.oblivious(&sync).await?;
    assert_eq!(synced.status.get(), 200);
    assert_eq!(
        BoardSyncResponse::decode(&synced.body)?
            .page
            .ok_or("asked for a page")?
            .records
            .len(),
        4
    );

    // A Meetup group with two members' subscriptions, created with a token.
    let first_token = PushToken::from_bytes(b"https://push.example/first".to_vec());
    let second_token = PushToken::from_bytes(b"https://push.example/second".to_vec());
    let key = LogAuthKeyPair::from_bytes(Zeroizing::new([5; 32]));
    let group = GroupId::from_bytes([6; 32]);
    let create = BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/groups?lifetime=meetup".to_owned(),
        headers: vec![spend(&mut tokens)?],
        body: CreateGroup {
            group_id: group,
            auth_key: key.public_key(),
            subscriptions: vec![
                subscription(&harness, &clock, 1, &first_token)?,
                subscription(&harness, &clock, 2, &second_token)?,
            ],
            state: None,
        }
        .encode()?,
    };
    let created = device.oblivious(&create).await?;
    assert_eq!(created.status.get(), 201);
    let session = response_header(&created, "pingers-group-session")
        .ok_or("no session")?
        .to_owned();
    let group_path = |rest: &str| format!("/v1/groups/{}{rest}", hex::encode(group.as_bytes()));

    // Two members commit for epoch 0 at once: exactly one wins, the other gets 409.
    let commit = |fill: u8| -> Result<BinaryRequest, TestError> {
        group_request(
            &clock,
            &GroupAuth {
                key: &key,
                session: &session,
            },
            ("POST", group_path("/commit")),
            CommitRequest {
                epoch: Epoch::new(0),
                commit: MlsMessageBytes::from_bytes(vec![fill; 8]),
                next_auth_key: LogAuthKeyPair::from_bytes(Zeroizing::new([fill; 32])).public_key(),
                state: None,
                subscriptions_added: Vec::new(),
                subscriptions_removed: Vec::new(),
                wake: Urgency::Quiet,
            }
            .encode()?,
        )
    };
    let (first, second) = (commit(7)?, commit(8)?);
    let mut racer = Device::new(&harness, &clock)?;
    racer.seed = 100;
    let committed_at = clock.now();
    let (one, other) = tokio::join!(device.oblivious(&first), racer.oblivious(&second));
    let (one, other) = (one?, other?);
    let (winner, loser) = match (one.status.get(), other.status.get()) {
        (200, _) => (7_u8, other),
        (_, 200) => (8_u8, one),
        statuses => return Err(format!("no commit won: {statuses:?}").into()),
    };
    // The loser gets 409 with the head epoch, whether its commit reached the log check first
    // or arrived signed with the key the winner had just rotated away.
    assert_eq!(loser.status.get(), 409);
    assert_eq!(
        CommitResponse::decode(&loser.body)?,
        CommitResponse::Conflict {
            head_epoch: Epoch::new(1)
        }
    );

    // The winning commit wakes both subscriptions quietly, each after its own delay of at most
    // 30 s on the injected timer: once the clock has moved 30 s, both reach the relay.
    let woken_by = |token: &PushToken| -> Option<CapturedPush> {
        harness
            .pushes()
            .into_iter()
            .find(|push| push.token == *token)
    };
    clock.advance(MAX_QUIET_DELAY);
    until(&harness, STEP, |harness| harness.pushes().len() == 2).await?;
    for token in [&first_token, &second_token] {
        let push = woken_by(token).ok_or("a member was not woken")?;
        assert_eq!(push.class, PushClass::Standard);
        let delay = push.sent_at.saturating_duration_since(committed_at);
        assert!(delay <= MAX_QUIET_DELAY, "{delay:?}");
    }

    // The first member's token is now gone at the push service. An alert wakes both again:
    // the relay answers 410 for the first, and the server deletes that subscription, so the
    // next alerts reach only the second member and the relay never sees the first again.
    harness.mark_token_gone(first_token.clone());
    let next_key = LogAuthKeyPair::from_bytes(Zeroizing::new([winner; 32]));
    let alert = |fill: u8| -> Result<BinaryRequest, TestError> {
        group_request(
            &clock,
            &GroupAuth {
                key: &next_key,
                session: &session,
            },
            ("POST", group_path("/msgs")),
            AppMessage {
                epoch: Epoch::new(1),
                body: MlsMessageBytes::from_bytes(vec![fill; 8]),
                delivery: Delivery::Stored,
                wake: Some(Urgency::Alert),
            }
            .encode()?,
        )
    };
    let before = harness.pushes().len();
    assert_eq!(device.oblivious(&alert(1)?).await?.status.get(), 200);
    until(&harness, STEP, |harness| {
        harness.gone_pushes().len() == 1 && harness.pushes().len() == before.saturating_add(1)
    })
    .await?;
    // The next alert goes out as soon as the relay has answered 410, so the server may not
    // have deleted the subscription yet when it makes the alert's wakes. A wake for the first
    // member then waits for that subscription's turn, which the 410'd push holds until the
    // delete is done, and finds it gone: the relay hears from the first member exactly once.
    for (round, fill) in [(2_usize, 2_u8), (3, 3)] {
        assert_eq!(device.oblivious(&alert(fill)?).await?.status.get(), 200);
        until(&harness, STEP, |harness| {
            harness.pushes().len() == before.saturating_add(round)
        })
        .await?;
        assert_eq!(
            harness.gone_pushes().len(),
            1,
            "the deleted subscription was pushed again"
        );
    }
    assert!(
        harness
            .gone_pushes()
            .iter()
            .all(|push| push.token == first_token)
    );
    assert!(
        harness
            .pushes()
            .iter()
            .skip(before)
            .all(|push| push.token == second_token)
    );
    harness.shutdown();
    Ok(())
}
