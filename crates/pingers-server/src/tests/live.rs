// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-transport's live-channel rules on the gateway side: keep-alives both ways, the split
//! channel's downlink and one-shot uplink.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use pingers_ohttp::{BinaryRequest, ChunkPosition, OpenedChunk};
use tokio::sync::mpsc;

use super::{Running, Setup, TestError, TestResult, connect, oblivious, open_live, start};
use crate::live::next_data;
use crate::{
    DeliveryLive, KEEP_ALIVE_CHUNK, KEEP_ALIVE_INTERVAL, LiveChannel, LiveFrame, LiveRefused,
};

const STEP: Duration = Duration::from_secs(10);

/// Downlinks by path; an uplink frame to a path goes to its downlink. Records what members
/// sent on their own exchanges.
#[derive(Default)]
struct Hub {
    downlinks: Mutex<HashMap<String, mpsc::Sender<LiveFrame>>>,
    inbound: Mutex<Vec<mpsc::Receiver<LiveFrame>>>,
}

impl LiveChannel for Arc<Hub> {
    fn open(
        &self,
        request: &BinaryRequest,
        inbound: mpsc::Receiver<LiveFrame>,
    ) -> Result<mpsc::Receiver<LiveFrame>, LiveRefused> {
        let (sender, receiver) = mpsc::channel(8);
        if let (Ok(mut downlinks), Ok(mut kept)) = (self.downlinks.lock(), self.inbound.lock()) {
            downlinks.insert(request.path.clone(), sender);
            kept.push(inbound);
        }
        Ok(receiver)
    }

    fn send(&self, request: &BinaryRequest, frame: LiveFrame) -> Result<(), LiveRefused> {
        let downlinks = self.downlinks.lock().map_err(|_| LiveRefused::NoChannel)?;
        let downlink = downlinks.get(&request.path).ok_or(LiveRefused::NoChannel)?;
        downlink.try_send(frame).map_err(|_| LiveRefused::NoChannel)
    }
}

/// The next opened chunks of a response, waiting up to `STEP`.
async fn next_chunks(
    body: &mut Body,
    decoder: &mut pingers_ohttp::ResponseChunkDecoder,
) -> Result<Vec<OpenedChunk>, TestError> {
    loop {
        let bytes = tokio::time::timeout(STEP, next_data(body))
            .await??
            .ok_or("the exchange ended")?;
        let opened = decoder.feed(&bytes)?;
        if !opened.is_empty() {
            return Ok(opened);
        }
    }
}

fn non_final(payload: &[u8]) -> Vec<OpenedChunk> {
    vec![OpenedChunk {
        payload: payload.to_vec(),
        position: ChunkPosition::NonFinal,
    }]
}

/// Advances the clock until the response yields chunks.
async fn chunks_after_advancing(
    server: &Running,
    body: &mut Body,
    decoder: &mut pingers_ohttp::ResponseChunkDecoder,
) -> Result<Vec<OpenedChunk>, TestError> {
    for _ in 0..100 {
        server.clock.advance(KEEP_ALIVE_INTERVAL);
        if let Ok(next) = tokio::time::timeout(Duration::from_millis(50), next_data(body)).await {
            let bytes = next?.ok_or("the exchange ended")?;
            let opened = decoder.feed(&bytes)?;
            if !opened.is_empty() {
                return Ok(opened);
            }
        }
    }
    Err("no keep-alive came".into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quiet_exchanges_get_keep_alives_and_member_keep_alives_are_dropped() -> TestResult {
    let hub = Arc::new(Hub::default());
    let server = start(Setup::default(), Arc::clone(&hub)).await?;
    let (sender, mut client, mut body) = open_live(&server, "/v1/groups/g1/live").await?;
    let mut decoder = client.response_decoder();
    // The response nonce comes at once, with the head.
    let nonce = tokio::time::timeout(STEP, next_data(&mut body))
        .await??
        .ok_or("no nonce")?;
    assert!(decoder.feed(&nonce)?.is_empty());

    // Just short of the interval, nothing; then a keep-alive.
    server
        .clock
        .advance(KEEP_ALIVE_INTERVAL.saturating_sub(Duration::from_secs(1)));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), next_data(&mut body))
            .await
            .is_err()
    );
    let keep_alive = chunks_after_advancing(&server, &mut body, &mut decoder).await?;
    assert_eq!(keep_alive, non_final(&KEEP_ALIVE_CHUNK));

    // The member's keep-alive is not a frame; its next frame is.
    sender.send(Bytes::from(
        client.seal_chunk(&KEEP_ALIVE_CHUNK, ChunkPosition::NonFinal)?,
    ))?;
    sender.send(Bytes::from(
        client.seal_chunk(b"f", ChunkPosition::NonFinal)?,
    ))?;
    let mut inbound = hub
        .inbound
        .lock()
        .map_err(|_| "lock")?
        .pop()
        .ok_or("not opened")?;
    let first = tokio::time::timeout(STEP, inbound.recv()).await?;
    assert_eq!(first, Some(LiveFrame::new(b"f".to_vec())));
    drop(sender);
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_split_channel_has_a_lasting_downlink_and_one_shot_uplinks() -> TestResult {
    let hub = Arc::new(Hub::default());
    let server = start(Setup::default(), Arc::clone(&hub)).await?;
    let path = "/v1/groups/g1/live?sub=s1";
    let (sender, mut client, mut body) = open_live(&server, path).await?;
    // The request stream ends right after its head: downlink only.
    sender.send(Bytes::from(client.seal_chunk(&[], ChunkPosition::Final)?))?;
    drop(sender);
    let mut decoder = client.response_decoder();

    // The member's half is held open, so the exchange does not end.
    let mut inbound = hub
        .inbound
        .lock()
        .map_err(|_| "lock")?
        .pop()
        .ok_or("not opened")?;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), inbound.recv())
            .await
            .is_err()
    );

    // An uplink frame as a one-shot request reaches the downlink.
    let mut relay = connect(server.address, super::LOOPBACK).await?;
    let uplink = |body: &[u8]| BinaryRequest {
        method: "POST".to_owned(),
        path: path.to_owned(),
        headers: Vec::new(),
        body: body.to_vec(),
    };
    let (_, answer) = oblivious(&mut relay, &server.config, &uplink(b"up")).await?;
    assert_eq!(answer.map(|answer| answer.status.get()), Some(202));
    assert_eq!(
        next_chunks(&mut body, &mut decoder).await?,
        non_final(b"up")
    );

    // To a path with no downlink, 404.
    let elsewhere = BinaryRequest {
        path: "/v1/groups/g2/live".to_owned(),
        ..uplink(b"up")
    };
    let (_, answer) = oblivious(&mut relay, &server.config, &elsewhere).await?;
    assert_eq!(answer.map(|answer| answer.status.get()), Some(404));
    server.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uplinks_wait_on_pingers_delivery() -> TestResult {
    let server = start(Setup::default(), DeliveryLive).await?;
    let mut relay = connect(server.address, super::LOOPBACK).await?;
    let uplink = BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/groups/g1/live".to_owned(),
        headers: Vec::new(),
        body: b"frame".to_vec(),
    };
    let (_, answer) = oblivious(&mut relay, &server.config, &uplink).await?;
    assert_eq!(answer.map(|answer| answer.status.get()), Some(501));
    server.stop().await
}
