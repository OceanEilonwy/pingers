// SPDX-License-Identifier: AGPL-3.0-only
//! The board and mailbox routes through OHTTP, the push relay client, and push jobs.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use pingers_crypto::{MailboxReadKey, RevokeKey, mailbox_id, revoke_commitment};
use pingers_mailbox::{
    ListedRelays, PushRelay as _, PushRequest, RelayAnswer, RelayCredential, RelayTarget,
    RelayUnreachable,
};
use pingers_ohttp::{BinaryRequest, BinaryResponse, Header};
use pingers_proto::{
    BoardSyncRequest, BoardSyncResponse, Lease, LeaseId, MailboxMessages, MailboxSpec,
    MessageAccepted, Padded, PushHandle, ReadBoard, Record, RecordRandomness, RecordTag, RelayId,
    RelayRef, RelayUrl, SealedBeacon, SealedPush, SealedPushBlob, SizeClass, TimeToLive, Urgency,
    WireDecode, WireEncode,
};
use pingers_time::{Clock as _, ManualClock, UnixSeconds};
use zeroize::Zeroizing;

use super::wallet::Wallet;
use super::{Echo, Running, Setup, TestError, TestResult, connect, oblivious, start};
use crate::tasks::{HttpPushRelay, RelayConfigKind, push_url};
use crate::wire::READ_KEY_HEADER;

/// An inner request with a body and headers.
fn inner(method: &str, path: String, headers: Vec<Header>, body: Vec<u8>) -> BinaryRequest {
    BinaryRequest {
        method: method.to_owned(),
        path,
        headers,
        body,
    }
}

/// Sends `request` through OHTTP and returns the inner response.
async fn call(server: &Running, request: &BinaryRequest) -> Result<BinaryResponse, TestError> {
    let mut client = connect(server.gateway, super::LOOPBACK).await?;
    let (status, opened) = oblivious(&mut client, &server.config, request).await?;
    assert_eq!(status, StatusCode::OK);
    opened.ok_or_else(|| "no inner response".into())
}

fn record(fill: u8) -> Record {
    Record {
        randomness: RecordRandomness::from_bytes([fill; 16]),
        tag: RecordTag::from_bytes([fill; 16]),
        sealed: SealedBeacon::from_bytes([fill; 64]),
    }
}

fn lease(server: &Running, key: u8) -> Lease {
    Lease {
        start: server.clock.now().bucket(),
        records: [0, 1, 2, 3].map(|slot| record(key.wrapping_add(slot))),
        revoke_commitment: revoke_commitment(&RevokeKey::from_bytes(Zeroizing::new([key; 32]))),
    }
}

fn sync_body(lease: &Lease, read: ReadBoard) -> Result<Vec<u8>, TestError> {
    Ok(BoardSyncRequest {
        lease: *lease,
        replaces: None,
        read,
    }
    .encode()?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_board_syncs_reads_and_revokes_through_ohttp() -> TestResult {
    let server = start(Setup::default(), Echo).await?;
    let wallet = Wallet::fetch(&server, 1, 3).await?;
    let synced = call(
        &server,
        &wallet.pay(inner(
            "POST",
            "/v1/board/sync".to_owned(),
            Vec::new(),
            sync_body(&lease(&server, 1), ReadBoard::Read)?,
        ))?,
    )
    .await?;
    assert_eq!(synced.status.get(), 200);
    assert!(
        synced
            .headers
            .iter()
            .any(|header| header.name == "content-type" && header.value == b"application/cbor")
    );
    let response = BoardSyncResponse::decode(&synced.body)?;
    let page = response.page.ok_or("asked for a page")?;
    assert_eq!(page.records.len(), 4);
    assert_eq!(response.server_time, server.clock.now());

    // A lease from long ago is refused; an undecodable body too.
    let mut stale = lease(&server, 2);
    stale.start = UnixSeconds::new(1_000).bucket();
    for (body, expected) in [
        (sync_body(&stale, ReadBoard::Skip)?, 422),
        (vec![1, 2, 3], 400),
    ] {
        let refused = call(
            &server,
            &wallet.pay(inner("POST", "/v1/board/sync".to_owned(), Vec::new(), body))?,
        )
        .await?;
        assert_eq!(refused.status.get(), expected);
    }

    let lease_path = format!(
        "/v1/board/lease/{}",
        hex::encode(response.lease_id.as_bytes())
    );
    let unknown_path = format!("/v1/board/lease/{}", hex::encode([7_u8; LeaseId::LENGTH]));
    for (path, body, expected) in [
        (lease_path.clone(), vec![2; 32], 403),
        (lease_path.clone(), vec![1; 31], 400),
        ("/v1/board/lease/zz".to_owned(), vec![1; 32], 400),
        (unknown_path, vec![1; 32], 404),
        (lease_path.clone(), vec![1; 32], 204),
        (lease_path, vec![1; 32], 404),
    ] {
        let answer = call(&server, &inner("DELETE", path.clone(), Vec::new(), body)).await?;
        assert_eq!(answer.status.get(), expected, "{path}");
    }
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn board_pages_need_a_cursor_this_board_sealed() -> TestResult {
    let server = start(Setup::default(), Echo).await?;
    for (path, expected) in [
        ("/v1/board/page", 400),
        ("/v1/board/page?c=zz", 400),
        ("/v1/board/page?c=00112233", 400),
    ] {
        let answer = call(
            &server,
            &inner("GET", path.to_owned(), Vec::new(), Vec::new()),
        )
        .await?;
        assert_eq!(answer.status.get(), expected, "{path}");
    }
    server.stop().await
}

fn read_key_header(key: &[u8; 32]) -> Vec<Header> {
    vec![Header {
        name: READ_KEY_HEADER.to_owned(),
        value: hex::encode(key).into_bytes(),
    }]
}

fn spec(key: &[u8; 32], push: Option<SealedPush>) -> Result<Vec<u8>, TestError> {
    let id = mailbox_id(&MailboxReadKey::from_bytes(Zeroizing::new(*key)));
    Ok(MailboxSpec {
        mailbox_id: id,
        time_to_live: TimeToLive::new(Duration::from_secs(3600))?,
        push,
    }
    .encode()?)
}

fn mailbox_path(key: &[u8; 32], rest: &str) -> String {
    let id = mailbox_id(&MailboxReadKey::from_bytes(Zeroizing::new(*key)));
    format!("/v1/mailboxes/{}{rest}", hex::encode(id.as_bytes()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailboxes_register_post_read_and_delete_through_ohttp() -> TestResult {
    let server = start(Setup::default(), Echo).await?;
    let wallet = Wallet::fetch(&server, 1, 4).await?;
    let key = [5_u8; 32];
    let register = inner(
        "POST",
        "/v1/mailboxes".to_owned(),
        Vec::new(),
        spec(&key, None)?,
    );
    let paid = wallet.pay(register.clone())?;
    assert_eq!(call(&server, &paid).await?.status.get(), 201);
    // The same token again is a double spend.
    assert_eq!(call(&server, &paid).await?.status.get(), 403);
    let again = wallet.pay(register)?;
    assert_eq!(call(&server, &again).await?.status.get(), 409);

    let post_path = mailbox_path(&key, "/msgs");
    let post_body = Padded::pad(b"knock", SizeClass::Bytes1024)?;
    let posted = call(
        &server,
        &wallet.pay(inner(
            "POST",
            post_path.clone(),
            Vec::new(),
            post_body.as_bytes().to_vec(),
        ))?,
    )
    .await?;
    assert_eq!(posted.status.get(), 202);
    let first = MessageAccepted::decode(&posted.body)?.sequence;
    let unpadded = call(
        &server,
        &wallet.pay(inner("POST", post_path.clone(), Vec::new(), vec![0; 100]))?,
    )
    .await?;
    assert_eq!(unpadded.status.get(), 422);

    let read = call(
        &server,
        &inner("GET", post_path.clone(), read_key_header(&key), Vec::new()),
    )
    .await?;
    assert_eq!(read.status.get(), 200);
    let messages = MailboxMessages::decode(&read.body)?.messages;
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages.first().map(|message| message.sequence),
        Some(first)
    );
    assert_eq!(
        messages.first().map(|message| &message.body),
        Some(&post_body)
    );
    let after_first = call(
        &server,
        &inner(
            "GET",
            format!("{post_path}?after={}", first.get()),
            read_key_header(&key),
            Vec::new(),
        ),
    )
    .await?;
    assert!(
        MailboxMessages::decode(&after_first.body)?
            .messages
            .is_empty()
    );

    for (method, path, headers, expected) in [
        ("GET", post_path.clone(), read_key_header(&[6; 32]), 403),
        ("GET", post_path.clone(), Vec::new(), 403),
        (
            "GET",
            format!("{post_path}?after=x"),
            read_key_header(&key),
            400,
        ),
        (
            "GET",
            "/v1/mailboxes/zz/msgs".to_owned(),
            read_key_header(&key),
            400,
        ),
        (
            "GET",
            mailbox_path(&[6; 32], "/msgs"),
            read_key_header(&[6; 32]),
            404,
        ),
        ("DELETE", mailbox_path(&key, ""), read_key_header(&key), 204),
        ("GET", post_path.clone(), read_key_header(&key), 404),
        ("DELETE", mailbox_path(&key, ""), read_key_header(&key), 404),
    ] {
        let answer = call(&server, &inner(method, path.clone(), headers, Vec::new())).await?;
        assert_eq!(answer.status.get(), expected, "{method} {path}");
    }
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_post_answers_at_once_and_its_wake_goes_to_the_relay() -> TestResult {
    // A listed relay that accepts connections and never completes TLS: the post is answered
    // all the same, and the push job's connection shows the wake was sent.
    let relay_socket = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let relays = ListedRelays::new([(
        RelayId::new(1),
        RelayTarget {
            url: RelayUrl::new(format!("https://{}", relay_socket.local_addr()?))?.into(),
            credential: RelayCredential::new("credential".to_owned()),
        },
    )]);
    let server = start(
        Setup {
            push_relays: relays,
            ..Setup::default()
        },
        Echo,
    )
    .await?;
    let wallet = Wallet::fetch(&server, 1, 3).await?;
    let key = [8_u8; 32];
    let push = |relay| SealedPush {
        relay: RelayRef::Listed(RelayId::new(relay)),
        key_id: pingers_proto::PushRelayKeyId::new(1),
        blob: SealedPushBlob::from_bytes([3; 320]),
        urgency: Urgency::Alert,
        handle: PushHandle::from_bytes([4; 8]),
    };
    // A relay this server does not forward to is refused at registration.
    let unknown = inner(
        "POST",
        "/v1/mailboxes".to_owned(),
        Vec::new(),
        spec(&key, Some(push(2)))?,
    );
    assert_eq!(
        call(&server, &wallet.pay(unknown)?).await?.status.get(),
        422
    );
    let register = inner(
        "POST",
        "/v1/mailboxes".to_owned(),
        Vec::new(),
        spec(&key, Some(push(1)))?,
    );
    assert_eq!(
        call(&server, &wallet.pay(register)?).await?.status.get(),
        201
    );
    let body = Padded::pad(b"knock", SizeClass::Bytes1024)?;
    let posted = call(
        &server,
        &wallet.pay(inner(
            "POST",
            mailbox_path(&key, "/msgs"),
            Vec::new(),
            body.as_bytes().to_vec(),
        ))?,
    )
    .await?;
    assert_eq!(posted.status.get(), 202);
    let (wake, _) = tokio::time::timeout(super::STEP, relay_socket.accept()).await??;
    // Stopping the server cancels the job, which closes its connection.
    server.stop().await?;
    let closed = async {
        let mut buffer = [0_u8; 1024];
        loop {
            if wake.readable().await.is_err() {
                return;
            }
            match wake.try_read(&mut buffer) {
                Ok(0) => return,
                Err(error) if error.kind() != std::io::ErrorKind::WouldBlock => return,
                Ok(_) | Err(_) => {}
            }
        }
    };
    tokio::time::timeout(super::STEP, closed).await?;
    Ok(())
}

/// What a test push relay received.
#[derive(Clone, Debug, Default)]
struct Received {
    authorization: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

/// A push relay on loopback answering every push with `status`.
async fn fake_relay(
    status: StatusCode,
) -> Result<(SocketAddr, Arc<Mutex<Vec<Received>>>), TestError> {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let router = Router::new().route(
        "/v1/push",
        post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let sink = Arc::clone(&sink);
            async move {
                let text = |name: &str| {
                    headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned)
                };
                if let Ok(mut sink) = sink.lock() {
                    sink.push(Received {
                        authorization: text("authorization"),
                        content_type: text("content-type"),
                        body: body.to_vec(),
                    });
                }
                status
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((address, received))
}

fn push_request() -> PushRequest {
    PushRequest {
        key_id: pingers_proto::PushRelayKeyId::new(3),
        blob: SealedPushBlob::from_bytes([0xab; 320]),
        bind: pingers_proto::PushBind::from_bytes([1; 32]),
        urgency: Urgency::Quiet,
        handle: PushHandle::from_bytes([0xfe; 8]),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_push_client_sends_the_credential_and_body_and_maps_the_status() -> TestResult {
    let clock = ManualClock::new(UnixSeconds::new(1));
    for (status, expected) in [
        (StatusCode::ACCEPTED, RelayAnswer::Accepted),
        (StatusCode::GONE, RelayAnswer::Gone),
        (StatusCode::TOO_MANY_REQUESTS, RelayAnswer::RateLimited),
    ] {
        let (address, received) = fake_relay(status).await?;
        let relay =
            HttpPushRelay::new(Arc::new(clock.clone()), Duration::from_secs(10), Vec::new())?;
        let answer = relay
            .post(
                &format!("http://{address}/v1/push"),
                "Bearer secret".to_owned(),
                &push_request(),
            )
            .await;
        assert_eq!(answer, Ok(expected));
        let received = received.lock().map_err(|_| "lock")?.clone();
        let only = received.first().ok_or("nothing received")?;
        assert_eq!(received.len(), 1);
        assert_eq!(only.authorization.as_deref(), Some("Bearer secret"));
        assert_eq!(only.content_type.as_deref(), Some("application/cbor"));
        assert_eq!(only.body, push_request().encode()?);
    }
    Ok(())
}

/// A push relay on loopback whose `GET /v1/config` answers `status` with `body`.
async fn config_relay(status: StatusCode, body: Vec<u8>) -> Result<SocketAddr, TestError> {
    config_relay_at("/v1/config", status, body).await
}

/// A push relay on loopback whose `GET path` answers `status` with `body`.
async fn config_relay_at(
    path: &'static str,
    status: StatusCode,
    body: Vec<u8>,
) -> Result<SocketAddr, TestError> {
    let router = Router::new().route(
        path,
        axum::routing::get(move || {
            let body = body.clone();
            async move { (status, body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(address)
}

/// A relay config whose encoding is exactly `length` bytes.
fn config_of_length(length: usize) -> Result<Vec<u8>, TestError> {
    let max_time_to_live = TimeToLive::new(Duration::from_hours(24))?;
    let config = |operator: String| pingers_proto::PushRelayConfig {
        key_config: pingers_proto::PushRelayKeyConfig {
            key_id: pingers_proto::PushRelayKeyId::new(1),
            public_key: [5; 32],
        },
        platforms: vec![pingers_proto::PushPlatform::Fcm],
        max_time_to_live,
        operator,
    };
    // The operator's length header grows by two bytes past 255 characters; aim past it.
    let base = config("x".repeat(300)).encode()?.len();
    let extra = length.checked_sub(base).ok_or("too short")?;
    let operator = "x".repeat(extra.saturating_add(300));
    let encoded = config(operator).encode()?;
    if encoded.len() != length {
        return Err("length not reached".into());
    }
    Ok(encoded)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_relay_config_is_passed_on_only_when_it_is_one() -> TestResult {
    let clock = ManualClock::new(UnixSeconds::new(1));
    let relay = HttpPushRelay::new(Arc::new(clock), Duration::from_secs(10), Vec::new())?;
    let largest = config_of_length(crate::tasks::MAX_RELAY_CONFIG_BYTES)?;
    let too_large = crate::tasks::MAX_RELAY_CONFIG_BYTES
        .checked_add(1)
        .ok_or("overflow")?;
    let small = config_of_length(400)?;
    for (status, body, expected) in [
        (StatusCode::OK, small.clone(), Ok(small.clone())),
        (StatusCode::OK, largest.clone(), Ok(largest)),
        (
            StatusCode::OK,
            config_of_length(too_large)?,
            Err(RelayUnreachable),
        ),
        (StatusCode::OK, b"not cbor".to_vec(), Err(RelayUnreachable)),
        (StatusCode::NOT_FOUND, small, Err(RelayUnreachable)),
    ] {
        let address = config_relay(status, body).await?;
        let fetched = relay
            .fetch_config(&format!("http://{address}/"), RelayConfigKind::Published)
            .await;
        assert_eq!(fetched, expected.map(Some), "{status}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_next_key_config_is_passed_on_with_its_activation_or_reported_absent() -> TestResult {
    let clock = ManualClock::new(UnixSeconds::new(1));
    let relay = HttpPushRelay::new(Arc::new(clock), Duration::from_secs(10), Vec::new())?;
    let published = config_of_length(400)?;
    let scheduled = pingers_proto::ScheduledPushRelayConfig {
        config: pingers_proto::PushRelayConfig::decode(&published)?,
        activates_at: UnixSeconds::new(4_102_444_800),
    }
    .encode()?;
    for (status, body, expected) in [
        (StatusCode::OK, scheduled.clone(), Ok(Some(scheduled))),
        // A bare config is the published key's body, not a schedule.
        (StatusCode::OK, published, Err(RelayUnreachable)),
        (StatusCode::NOT_FOUND, Vec::new(), Ok(None)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Vec::new(),
            Err(RelayUnreachable),
        ),
    ] {
        let address = config_relay_at("/v1/config/next", status, body).await?;
        let fetched = relay
            .fetch_config(&format!("http://{address}"), RelayConfigKind::Next)
            .await;
        assert_eq!(fetched, expected, "{status}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_on_this_machine_is_reached_over_plain_http_on_loopback() -> TestResult {
    let (address, received) = fake_relay(StatusCode::ACCEPTED).await?;
    let target = RelayTarget {
        url: pingers_mailbox::PushRelayUrl::new(format!("http://{address}"))?,
        credential: RelayCredential::new("credential".to_owned()),
    };
    let clock = ManualClock::new(UnixSeconds::new(1));
    let relay = HttpPushRelay::new(Arc::new(clock), Duration::from_secs(10), Vec::new())?;
    assert_eq!(
        relay.push(&target, &push_request()).await,
        Ok(RelayAnswer::Accepted)
    );
    let received = received.lock().map_err(|_| "lock")?.clone();
    assert_eq!(
        received
            .first()
            .and_then(|only| only.authorization.as_deref()),
        Some("Bearer credential")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_or_silent_relay_is_unreachable() -> TestResult {
    let clock = ManualClock::new(UnixSeconds::new(1));
    let relay = HttpPushRelay::new(Arc::new(clock.clone()), Duration::from_secs(10), Vec::new())?;
    // Nothing listens on port 1.
    let refused = relay
        .post("http://127.0.0.1:1/v1/push", String::new(), &push_request())
        .await;
    assert_eq!(refused, Err(RelayUnreachable));

    // Accepts the connection, never answers: the injected timer ends the wait.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = silent.local_addr()?;
    let pending = relay.post(
        &format!("http://{address}/v1/push"),
        String::new(),
        &push_request(),
    );
    let mut pending = std::pin::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut pending)
            .await
            .is_err()
    );
    for _ in 0..100 {
        clock.advance(Duration::from_secs(10));
        if let Ok(answer) = tokio::time::timeout(Duration::from_millis(50), &mut pending).await {
            assert_eq!(answer, Err(RelayUnreachable));
            drop(silent);
            return Ok(());
        }
    }
    Err("the push never timed out".into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn push_urls_join_the_relay_and_the_push_path() -> TestResult {
    for (url, expected) in [
        ("https://push.example", "https://push.example/v1/push"),
        ("https://push.example/", "https://push.example/v1/push"),
        (
            "https://push.example/base",
            "https://push.example/base/v1/push",
        ),
    ] {
        let target = RelayTarget {
            url: RelayUrl::new(url.to_owned())?.into(),
            credential: RelayCredential::new("credential".to_owned()),
        };
        assert_eq!(push_url(&target), expected);
    }
    // `push` goes to that URL with the target's credential: an https URL nobody serves.
    let clock = ManualClock::new(UnixSeconds::new(1));
    let relay = HttpPushRelay::new(Arc::new(clock), Duration::from_secs(10), Vec::new())?;
    let target = RelayTarget {
        url: RelayUrl::new("https://127.0.0.1:1".to_owned())?.into(),
        credential: RelayCredential::new("credential".to_owned()),
    };
    assert_eq!(
        relay.push(&target, &push_request()).await,
        Err(RelayUnreachable)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_relay_config_is_fetched_once_per_cache_lifetime() -> TestResult {
    // A relay that counts the config fetches it answers, and fails while `failing` is set.
    let fetches = Arc::new(Mutex::new(0_u32));
    let failing = Arc::new(Mutex::new(false));
    let config = config_of_length(400)?;
    let router = {
        let (fetches, failing, config) =
            (Arc::clone(&fetches), Arc::clone(&failing), config.clone());
        Router::new().route(
            "/v1/config",
            axum::routing::get(move || {
                let (fetches, failing, config) =
                    (Arc::clone(&fetches), Arc::clone(&failing), config.clone());
                async move {
                    if let Ok(mut count) = fetches.lock() {
                        *count = count.saturating_add(1);
                    }
                    if failing.lock().is_ok_and(|failing| *failing) {
                        (StatusCode::SERVICE_UNAVAILABLE, Vec::new())
                    } else {
                        (StatusCode::OK, config)
                    }
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let relays = ListedRelays::new([(
        RelayId::new(1),
        RelayTarget {
            url: pingers_mailbox::PushRelayUrl::new(format!("http://{address}"))?,
            credential: RelayCredential::new("credential".to_owned()),
        },
    )]);
    let server = start(
        Setup {
            push_relays: relays,
            ..Setup::default()
        },
        Echo,
    )
    .await?;
    let ask = || super::wallet::direct(&server, "GET /v1/push-relays/1/config", None, Vec::new());
    let count = || fetches.lock().map(|count| *count).map_err(|_| "lock");
    for _ in 0..5 {
        let answer = ask().await?;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.body, config);
    }
    assert_eq!(count()?, 1);
    // Once the cached config is stale the relay is asked again; a failure is remembered for
    // a shorter time.
    *failing.lock().map_err(|_| "lock")? = true;
    server.clock.advance(crate::tasks::RELAY_CONFIG_TTL);
    for _ in 0..3 {
        assert_eq!(ask().await?.status, StatusCode::BAD_GATEWAY);
    }
    assert_eq!(count()?, 2);
    *failing.lock().map_err(|_| "lock")? = false;
    server.clock.advance(crate::tasks::RELAY_CONFIG_RETRY);
    assert_eq!(ask().await?.status, StatusCode::OK);
    assert_eq!(count()?, 3);
    server.stop().await
}

/// A relay config for key `key_id` with public key `public`.
fn relay_config(key_id: u8, public: u8) -> Result<Vec<u8>, TestError> {
    Ok(pingers_proto::PushRelayConfig {
        key_config: pingers_proto::PushRelayKeyConfig {
            key_id: pingers_proto::PushRelayKeyId::new(key_id),
            public_key: [public; 32],
        },
        platforms: vec![pingers_proto::PushPlatform::Fcm],
        max_time_to_live: TimeToLive::new(Duration::from_hours(24))?,
        operator: "Me".to_owned(),
    }
    .encode()?)
}

/// The pin of a relay config's key, as `/v1/params` lists it.
fn relay_pin(config: &[u8]) -> Result<pingers_proto::KeyConfigHash, TestError> {
    let config = pingers_proto::PushRelayConfig::decode(config)?;
    Ok(pingers_crypto::key_config_hash(
        &config.key_config.to_bytes(),
    ))
}

/// A push relay on loopback whose `GET /v1/config` answers what `config` holds now, or `503`
/// while it holds nothing.
async fn changing_relay(config: Arc<Mutex<Option<Vec<u8>>>>) -> Result<SocketAddr, TestError> {
    let router = Router::new().route(
        "/v1/config",
        axum::routing::get(move || {
            let held = config.lock().ok().and_then(|held| held.clone());
            async move {
                match held {
                    Some(body) => (StatusCode::OK, body),
                    None => (StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(address)
}

/// The push relays `/v1/params` lists now.
async fn listed_relays(server: &Running) -> Result<Vec<pingers_proto::PushRelayInfo>, TestError> {
    let params = super::wallet::direct(server, "GET /v1/params", None, Vec::new()).await?;
    Ok(pingers_proto::Params::decode(&params.body)?.push_relays)
}

/// Waits until `/v1/params` lists exactly `expected` as the learned relay's key, or `None` for
/// no relay. The learning task runs on its own, so the clock moves on by `step` between looks:
/// whenever the task is asleep, the next step wakes it.
async fn until_listed(
    server: &Running,
    expected: Option<pingers_proto::KeyConfigHash>,
    step: Duration,
) -> TestResult {
    for _ in 0..1000 {
        let listed: Vec<_> = listed_relays(server)
            .await?
            .into_iter()
            .map(|relay| relay.key_config_hash)
            .collect();
        if listed == expected.into_iter().collect::<Vec<_>>() {
            return Ok(());
        }
        tokio::task::yield_now().await;
        server.clock.advance(step);
    }
    Err(format!("the listed relay key never became {expected:?}").into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_learned_relay_is_listed_once_it_answers_and_follows_its_key() -> TestResult {
    let config = Arc::new(Mutex::new(None));
    let address = changing_relay(Arc::clone(&config)).await?;
    let relays = ListedRelays::new([(
        RelayId::new(1),
        RelayTarget {
            url: pingers_mailbox::PushRelayUrl::new(format!("http://{address}"))?,
            credential: RelayCredential::new("credential".to_owned()),
        },
    )]);
    let server = super::run(
        crate::ServerConfig {
            learned_push_relays: vec![crate::LearnedPushRelay {
                relay_id: RelayId::new(1),
                url: RelayUrl::new("https://push.example".to_owned())?,
                operator: "Me".to_owned(),
            }],
            ..super::configured(Setup {
                push_relays: relays,
                ..Setup::default()
            })?
        },
        Echo,
    )
    .await?;
    // The relay is not up yet: nothing is listed, and the server asks again soon.
    until_listed(&server, None, Duration::ZERO).await?;
    let first = relay_config(1, 5)?;
    *config.lock().map_err(|_| "lock")? = Some(first.clone());
    until_listed(
        &server,
        Some(relay_pin(&first)?),
        crate::tasks::RELAY_CONFIG_RETRY,
    )
    .await?;
    let listed = listed_relays(&server).await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed.first().map(|relay| relay.url.as_str()),
        Some("https://push.example")
    );
    assert_eq!(
        listed.first().map(|relay| relay.operator.as_str()),
        Some("Me")
    );
    // The relay rotates: the server lists the new key within one cache lifetime, and keeps the
    // last key it saw while the relay does not answer.
    *config.lock().map_err(|_| "lock")? = None;
    for _ in 0..10 {
        server.clock.advance(crate::tasks::RELAY_CONFIG_TTL);
        tokio::task::yield_now().await;
    }
    until_listed(&server, Some(relay_pin(&first)?), Duration::ZERO).await?;
    let second = relay_config(2, 6)?;
    *config.lock().map_err(|_| "lock")? = Some(second.clone());
    until_listed(
        &server,
        Some(relay_pin(&second)?),
        crate::tasks::RELAY_CONFIG_TTL,
    )
    .await?;
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_that_has_not_answered_is_asked_again_soon_not_after_the_cache_lifetime()
-> TestResult {
    // A relay that is down counts the times it is asked.
    let asked = Arc::new(Mutex::new(0_u32));
    let router = {
        let asked = Arc::clone(&asked);
        Router::new().route(
            "/v1/config",
            axum::routing::get(move || {
                if let Ok(mut count) = asked.lock() {
                    *count = count.saturating_add(1);
                }
                async { (StatusCode::SERVICE_UNAVAILABLE, Vec::new()) }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let relays = ListedRelays::new([(
        RelayId::new(1),
        RelayTarget {
            url: pingers_mailbox::PushRelayUrl::new(format!("http://{address}"))?,
            credential: RelayCredential::new("credential".to_owned()),
        },
    )]);
    let server = super::run(
        crate::ServerConfig {
            learned_push_relays: vec![crate::LearnedPushRelay {
                relay_id: RelayId::new(1),
                url: RelayUrl::new("https://push.example".to_owned())?,
                operator: "Me".to_owned(),
            }],
            ..super::configured(Setup {
                push_relays: relays,
                ..Setup::default()
            })?
        },
        Echo,
    )
    .await?;
    let count = || asked.lock().map(|count| *count).map_err(|_| "lock");
    // Each further ask comes within the retry interval, one second at a time, well before the
    // five-minute cache lifetime the server waits once every relay is known.
    let mut seen = 0;
    for round in 0..3 {
        let mut asked_again = false;
        for _ in 0..crate::tasks::RELAY_CONFIG_RETRY.as_secs().saturating_add(5) {
            // A real moment for the request to arrive (the server's own clock is the manual one).
            let _ =
                tokio::time::timeout(Duration::from_millis(30), std::future::pending::<()>()).await;
            if count()? > seen {
                asked_again = true;
                break;
            }
            server.clock.advance(Duration::from_secs(1));
        }
        assert!(
            asked_again,
            "round {round}: the relay was not asked again in time"
        );
        seen = count()?;
    }
    server.stop().await
}
