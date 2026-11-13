// SPDX-License-Identifier: AGPL-3.0-only
//! One-shot anonymous requests through the relay: fresh contexts, relay choice and health,
//! key pinning, and never sending direct.

use std::convert::Infallible;
use std::time::Duration;

use pingers_ohttp::{GatewayKey, OhttpError};
use pingers_testkit::seeded_rng;
use pingers_transport::{KeyPin, PrivacyClass, Transport, TransportError};
use tokio::net::TcpListener;

use crate::support::{
    STEP, TestError, advancing_through, after, closed_address, live_request, relay_at, request,
    serve_h2, start_relay, world,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anonymous_requests_go_through_the_relay_each_with_a_fresh_context() -> Result<(), TestError>
{
    let world = world().await?;
    let client = world.client()?;
    for body in [b"one".as_slice(), b"two"] {
        let response = tokio::time::timeout(
            STEP,
            client.request(PrivacyClass::Anonymous, request("/v1/board/sync", body)),
        )
        .await??;
        assert_eq!(response.status.get(), 200);
        assert_eq!(
            response.body,
            [b"seen /v1/board/sync ".as_slice(), body].concat()
        );
    }
    let seen = world.gateway.seen();
    // The key config is fetched once, through the relay, then kept.
    assert_eq!(seen.paths, ["/v1/ohttp/config", "/ohttp", "/ohttp"]);
    assert_eq!(seen.inner_paths, ["/v1/board/sync", "/v1/board/sync"]);
    let [first, second] = seen.one_shot_encapsulations.as_slice() else {
        return Err(TestError::Setup("two requests"));
    };
    assert_ne!(first, second, "each request has its own HPKE context");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ohttp_transport_carries_only_anonymous_requests() -> Result<(), TestError> {
    let world = world().await?;
    let client = world.client()?;
    for class in [PrivacyClass::Identified, PrivacyClass::Stream] {
        assert!(matches!(
            client.request(class, request("/v1/accounts", b"")).await,
            Err(TransportError::ClassNotAllowed)
        ));
    }
    assert!(world.gateway.seen().paths.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_relay_reachable_nothing_is_sent_at_all() -> Result<(), TestError> {
    let world = world().await?;
    let client = world.client_with(
        vec![relay_at(closed_address().await?)?],
        KeyPin::Pinned(world.gateway.config.hash()),
    )?;
    for _ in 0..2 {
        let result = tokio::time::timeout(
            STEP,
            client.request(PrivacyClass::Anonymous, request("/v1/board/sync", b"x")),
        )
        .await?;
        assert!(matches!(result, Err(TransportError::NoRelay)));
    }
    let stream = tokio::time::timeout(STEP, client.stream(live_request())).await?;
    assert!(matches!(stream, Err(TransportError::NoRelay)));
    // The gateway is up and listening, and nothing reached it: no direct fallback.
    assert!(world.gateway.seen().paths.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_relay_is_skipped_for_the_next_one() -> Result<(), TestError> {
    let world = world().await?;
    let client = world.client_with(
        vec![
            relay_at(closed_address().await?)?,
            relay_at(world.relay_address()?)?,
        ],
        KeyPin::Pinned(world.gateway.config.hash()),
    )?;
    let response = tokio::time::timeout(
        STEP,
        client.request(PrivacyClass::Anonymous, request("/v1/mailboxes", b"m")),
    )
    .await??;
    assert_eq!(response.body, b"seen /v1/mailboxes m");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_config_that_does_not_match_the_pin_fails_closed() -> Result<(), TestError> {
    let world = world().await?;
    let other = GatewayKey::generate(1, &mut seeded_rng(99))?.config();
    let client = world.client_with(
        vec![relay_at(world.relay_address()?)?],
        KeyPin::Pinned(other.hash()),
    )?;
    let anonymous = || client.request(PrivacyClass::Anonymous, request("/v1/board/sync", b"x"));

    let first = tokio::time::timeout(STEP, anonymous()).await?;
    assert!(matches!(
        first,
        Err(TransportError::Ohttp(OhttpError::KeyMismatch))
    ));
    // Only the key config was fetched; no request was encapsulated to the unpinned key.
    assert_eq!(world.gateway.seen().paths, ["/v1/ohttp/config"]);

    // Latched: later requests and streams fail without touching the network.
    let again = tokio::time::timeout(STEP, anonymous()).await?;
    assert!(matches!(
        again,
        Err(TransportError::Ohttp(OhttpError::KeyMismatch))
    ));
    let stream = tokio::time::timeout(STEP, client.stream(live_request())).await?;
    assert!(matches!(
        stream,
        Err(TransportError::Ohttp(OhttpError::KeyMismatch))
    ));
    assert_eq!(world.gateway.seen().paths, ["/v1/ohttp/config"]);

    // Once resolved (the user accepted the new key), traffic resumes.
    client.repin(KeyPin::Pinned(world.gateway.config.hash()));
    let response = tokio::time::timeout(STEP, anonymous()).await??;
    assert_eq!(response.status.get(), 200);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_use_pins_the_config_fetched_through_the_relay() -> Result<(), TestError> {
    let world = world().await?;
    let client = world.client_with(vec![relay_at(world.relay_address()?)?], KeyPin::FirstUse)?;
    assert_eq!(client.key_config_hash(), None);
    tokio::time::timeout(
        STEP,
        client.request(PrivacyClass::Anonymous, request("/v1/board/sync", b"x")),
    )
    .await??;
    assert_eq!(client.key_config_hash(), Some(world.gateway.config.hash()));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_error_status_is_reported() -> Result<(), TestError> {
    let world = world().await?;
    // A relay whose gateway is gone answers 502.
    let orphan = start_relay(
        "127.0.0.1:0"
            .parse()
            .map_err(|_| TestError::Setup("address"))?,
        closed_address().await?,
    )
    .await?;
    let client = world.client_with(
        vec![relay_at(orphan.address)?],
        KeyPin::Pinned(world.gateway.config.hash()),
    )?;
    let result = tokio::time::timeout(
        STEP,
        client.request(PrivacyClass::Anonymous, request("/v1/board/sync", b"x")),
    )
    .await?;
    assert!(matches!(
        result,
        Err(TransportError::Status { status: 502 })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_relay_times_out_on_the_injected_timer_and_then_rests() -> Result<(), TestError> {
    let world = world().await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let silent = listener.local_addr()?;
    let _silent_tasks = serve_h2(listener, |_request| async {
        std::future::pending::<Result<_, Infallible>>().await
    });
    let client = world.client_with(
        vec![relay_at(silent)?],
        KeyPin::Pinned(world.gateway.config.hash()),
    )?;
    // Time moves only once the client waits on its 30 s request deadline.
    let result = advancing_through(
        &world.clock,
        &[after(&world.clock, Duration::from_secs(30))],
        client.request(PrivacyClass::Anonymous, request("/v1/board/sync", b"x")),
    )
    .await?;
    assert!(matches!(result, Err(TransportError::Timeout)));

    // The relay rests after the failure; with no other relay, nothing is tried.
    let result = tokio::time::timeout(
        STEP,
        client.request(PrivacyClass::Anonymous, request("/v1/board/sync", b"x")),
    )
    .await?;
    assert!(matches!(result, Err(TransportError::NoRelay)));
    Ok(())
}
