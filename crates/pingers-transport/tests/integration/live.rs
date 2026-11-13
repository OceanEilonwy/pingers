// SPDX-License-Identifier: AGPL-3.0-only
//! The chunked live channel through the relay: both directions, keep-alives, reconnects,
//! the split-channel fallback, and how it ends.

use std::collections::BTreeSet;
use std::time::Duration;

use pingers_testkit::seeded_rng;
use pingers_transport::{
    DuplexStream, KEEP_ALIVE_CHUNK, KeyPin, OhttpConfig, OhttpTransport, TlsRoots, Transport,
    TransportError,
};

use crate::support::{
    STEP, TestError, World, advancing, eventually, live_request, real_pause, relay_at,
    start_buffering_relay, start_relay, world,
};

async fn receive(stream: &mut Box<dyn DuplexStream>) -> Result<Vec<u8>, TestError> {
    tokio::time::timeout(STEP, stream.receive())
        .await??
        .ok_or(TestError::Setup("stream ended"))
}

fn frame(label: &str, index: usize) -> Vec<u8> {
    // Real frames are padded to 256 bytes (design B.6.1).
    let mut frame = format!("{label} {index}").into_bytes();
    frame.resize(256, b'.');
    frame
}

fn echo(frame: &[u8]) -> Vec<u8> {
    [b"echo ".as_slice(), frame].concat()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_channel_carries_both_directions_interleaved_with_keep_alives()
-> Result<(), TestError> {
    let world = world().await?;
    let client = world.client()?;
    let mut stream = tokio::time::timeout(STEP, client.stream(live_request())).await??;

    for round in 0..200 {
        // Up and down at the same time: the client's frame and a frame the gateway pushes on
        // its own may arrive in either order.
        stream.send(frame("up", round)).await?;
        world.gateway.push(&frame("down", round))?;
        let got: BTreeSet<Vec<u8>> = [receive(&mut stream).await?, receive(&mut stream).await?]
            .into_iter()
            .collect();
        let want: BTreeSet<Vec<u8>> = [echo(&frame("up", round)), frame("down", round)]
            .into_iter()
            .collect();
        assert_eq!(got, want, "round {round}");

        // Every tenth round, 20 quiet seconds pass: a keep-alive goes up and comes back,
        // and is never handed to the caller.
        if round % 10 == 9 {
            let before = world
                .gateway
                .seen()
                .exchanges
                .first()
                .map_or(0, |e| e.keep_alives);
            world.clock.advance(Duration::from_secs(20));
            eventually(|| {
                world
                    .gateway
                    .seen()
                    .exchanges
                    .first()
                    .map_or(0, |e| e.keep_alives)
                    > before
            })
            .await?;
        }
    }
    // An hour and six minutes of keep-alives on one exchange: no reconnect happened.
    {
        let seen = world.gateway.seen();
        let [exchange] = seen.exchanges.as_slice() else {
            return Err(TestError::Setup("one exchange"));
        };
        assert_eq!(exchange.keep_alives, 20);
        assert_eq!(exchange.frames.len(), 200);
        assert_eq!(exchange.frames.first(), Some(&frame("up", 0)));
    }

    // A clean close sends the final chunk and the gateway answers with its own.
    tokio::time::timeout(STEP, stream.close()).await??;
    eventually(|| {
        world
            .gateway
            .seen()
            .exchanges
            .first()
            .is_some_and(|e| e.finished)
    })
    .await?;
    assert!(matches!(
        stream.send(frame("late", 0)).await,
        Err(TransportError::StreamClosed)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frame_the_channel_cannot_carry_is_refused() -> Result<(), TestError> {
    let world = world().await?;
    let client = world.client()?;
    let mut stream = tokio::time::timeout(STEP, client.stream(live_request())).await??;
    for bad in [
        Vec::new(),
        KEEP_ALIVE_CHUNK.to_vec(),
        vec![1; 16 * 1024 + 1],
    ] {
        assert!(matches!(
            stream.send(bad).await,
            Err(TransportError::InvalidRequest)
        ));
    }
    let mut with_body = live_request();
    with_body.body = b"frames go in the stream".to_vec();
    assert!(matches!(
        client.stream(with_body).await,
        Err(TransportError::InvalidRequest)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_channel_reconnects_with_a_fresh_context_after_the_relay_drops() -> Result<(), TestError>
{
    let mut world = world().await?;
    let client = world.client()?;
    let mut stream = tokio::time::timeout(STEP, client.stream(live_request())).await??;
    stream.send(frame("before", 0)).await?;
    assert_eq!(receive(&mut stream).await?, echo(&frame("before", 0)));

    // The relay goes away, taking the exchange with it, and comes back on the same address
    // a little later.
    let address = world.relay_address()?;
    world.relay = None;
    for _ in 0..5 {
        real_pause().await;
    }
    world.relay = Some(start_relay(address, world.gateway_address).await?);

    // The driver retries on the injected timer's backoff. Frames sent while it is down are
    // kept and go out on the new exchange (one sent just as the old one broke may be lost).
    let mut sent = Vec::new();
    let got = tokio::time::timeout(STEP, async {
        loop {
            let probe = frame("during", sent.len());
            stream.send(probe.clone()).await?;
            sent.push(probe);
            world.clock.advance(Duration::from_secs(1));
            if let Ok(received) =
                tokio::time::timeout(Duration::from_millis(50), stream.receive()).await
            {
                return received?.ok_or(TestError::Setup("stream ended"));
            }
        }
    })
    .await??;
    assert!(sent.iter().any(|probe| echo(probe) == got));

    let seen = world.gateway.seen();
    let [first, second] = seen.exchanges.as_slice() else {
        return Err(TestError::Setup("two exchanges"));
    };
    assert_ne!(
        first.encapsulation, second.encapsulation,
        "a reconnect uses a fresh HPKE context"
    );
    assert_eq!(first.frames, [frame("before", 0)]);
    assert!(!second.frames.is_empty());
    Ok(())
}

fn client_with_live(
    world: &World,
    relays: Vec<pingers_transport::Relay>,
    tune: impl FnOnce(&mut OhttpConfig),
) -> Result<OhttpTransport, TestError> {
    let mut config = OhttpConfig::new(
        relays,
        KeyPin::Pinned(world.gateway.config.hash()),
        TlsRoots::from_der(&[])?,
    );
    tune(&mut config);
    Ok(OhttpTransport::new(
        config,
        std::sync::Arc::new(world.clock.clone()),
        Box::new(seeded_rng(8)),
    )?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnecting_gives_up_after_the_limit() -> Result<(), TestError> {
    let mut world = world().await?;
    let client = client_with_live(&world, vec![relay_at(world.relay_address()?)?], |config| {
        config.live.max_reconnects = 2;
    })?;
    let mut stream = tokio::time::timeout(STEP, client.stream(live_request())).await??;
    world.relay = None;
    let result = advancing(&world.clock, Duration::from_secs(1), stream.receive()).await?;
    assert!(matches!(result, Err(TransportError::NoRelay)));
    assert!(matches!(
        stream.receive().await,
        Err(TransportError::StreamClosed)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gateway_ending_the_channel_ends_receive() -> Result<(), TestError> {
    let world = world().await?;
    let client = world.client()?;
    let mut stream = tokio::time::timeout(STEP, client.stream(live_request())).await??;
    world.gateway.push(&frame("last", 0))?;
    assert_eq!(receive(&mut stream).await?, frame("last", 0));
    world.gateway.end_live();
    assert_eq!(tokio::time::timeout(STEP, stream.receive()).await??, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_that_buffers_request_bodies_gets_the_split_channel() -> Result<(), TestError> {
    let world = world().await?;
    let buffering = start_buffering_relay(world.gateway_address).await?;
    let client = world.client_with(
        vec![relay_at(buffering.address)?],
        KeyPin::Pinned(world.gateway.config.hash()),
    )?;

    // No response head arrives while the request body is held back: after the probe time
    // the client re-opens as a split channel.
    let mut stream = advancing(
        &world.clock,
        Duration::from_secs(1),
        client.stream(live_request()),
    )
    .await??;
    stream.send(frame("up", 1)).await?;
    assert_eq!(receive(&mut stream).await?, echo(&frame("up", 1)));
    world.gateway.push(&frame("down", 1))?;
    assert_eq!(receive(&mut stream).await?, frame("down", 1));

    {
        let seen = world.gateway.seen();
        let split = seen.exchanges.last().ok_or(TestError::Setup("exchange"))?;
        // The downlink's request ended right after its head; the frame came up one-shot.
        assert!(split.finished);
        assert_eq!(split.keep_alives, 0);
        assert_eq!(split.frames, [frame("up", 1)]);
        assert!(
            seen.inner_paths
                .iter()
                .any(|path| path == crate::support::LIVE_PATH)
        );
    }

    // The relay is remembered: the next channel goes split at once, with no probe wait.
    let exchanges = world.gateway.seen().exchanges.len();
    let mut second = tokio::time::timeout(STEP, client.stream(live_request())).await??;
    eventually(|| world.gateway.seen().exchanges.len() == exchanges.saturating_add(1)).await?;
    world.gateway.push(&frame("down", 2))?;
    assert_eq!(receive(&mut second).await?, frame("down", 2));
    Ok(())
}
