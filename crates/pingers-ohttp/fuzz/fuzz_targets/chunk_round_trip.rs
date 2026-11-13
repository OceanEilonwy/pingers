// SPDX-License-Identifier: AGPL-3.0-only
//! Real chunked exchanges with fuzzer-chosen chunks (including empty and maximum-size ones),
//! transport splits and corruption, in both directions.
//!
//! Unmodified streams must decode to exactly what was sealed. A corrupted stream must never
//! yield a chunk that was not sealed: whatever opens is a prefix of what was sent. Never
//! panics.
#![no_main]

mod common;

use libfuzzer_sys::{arbitrary, fuzz_target};
use pingers_ohttp::{ChunkPosition, ChunkedGateway, OhttpError, OpenedChunk};

#[derive(arbitrary::Arbitrary, Debug)]
enum Corruption {
    None,
    FlipBit { offset: u16, bit: u8 },
    Truncate { keep: u16 },
    Insert { offset: u16, byte: u8 },
}

#[derive(arbitrary::Arbitrary, Debug)]
struct Chunk {
    length: u16,
    fill: u8,
}

#[derive(arbitrary::Arbitrary, Debug)]
struct Input {
    seed: u64,
    chunks: Vec<Chunk>,
    final_length: u16,
    splits: Vec<u8>,
    corruption: Corruption,
}

/// Applies the corruption; returns whether the stream changed.
fn corrupt(stream: &mut Vec<u8>, corruption: &Corruption) -> bool {
    match *corruption {
        Corruption::None => false,
        Corruption::FlipBit { offset, bit } => {
            let index = usize::from(offset) % stream.len().max(1);
            match stream.get_mut(index) {
                Some(byte) => {
                    *byte ^= 1 << (bit % 8);
                    true
                }
                None => false,
            }
        }
        Corruption::Truncate { keep } => {
            let keep = usize::from(keep);
            let changed = keep < stream.len();
            stream.truncate(keep);
            changed
        }
        Corruption::Insert { offset, byte } => {
            let index = usize::from(offset) % (stream.len() + 1);
            stream.insert(index, byte);
            true
        }
    }
}

/// What came out of decoding one stream.
struct Decoded {
    opened: Vec<OpenedChunk>,
    clean: bool,
}

fn check(sent: &[OpenedChunk], decoded: &Decoded, corrupted: bool) {
    assert!(decoded.opened.len() <= sent.len());
    assert_eq!(
        decoded.opened.as_slice(),
        &sent[..decoded.opened.len()],
        "a chunk opened that was not sent"
    );
    if !corrupted {
        assert!(decoded.clean, "an intact stream failed to decode");
        assert_eq!(decoded.opened, sent);
    }
}

fuzz_target!(|input: Input| {
    let (gateway, config) = common::gateway();
    let start = common::start(&config, input.seed);
    let mut client = start.client;
    let mut gateway_half = ChunkedGateway::accept(
        &gateway,
        &start.preamble,
        &mut pingers_testkit::seeded_rng(input.seed),
    )
    .expect("real preamble is accepted");

    let mut sent = Vec::new();
    let mut up = Vec::new();
    let mut down = gateway_half.response_preamble();
    for chunk in input.chunks.iter().take(16) {
        let payload = vec![chunk.fill; usize::from(chunk.length) % (16 * 1024 + 2)];
        let up_result = client.seal_chunk(&payload, ChunkPosition::NonFinal);
        let down_result = gateway_half.seal_chunk(&payload, ChunkPosition::NonFinal);
        match (up_result, down_result) {
            (Ok(up_bytes), Ok(down_bytes)) => {
                up.extend(up_bytes);
                down.extend(down_bytes);
                sent.push(OpenedChunk { payload, position: ChunkPosition::NonFinal });
            }
            (Err(OhttpError::EmptyNonFinalChunk), Err(OhttpError::EmptyNonFinalChunk)) => {
                assert!(payload.is_empty());
            }
            (Err(OhttpError::ChunkTooLarge), Err(OhttpError::ChunkTooLarge)) => {
                assert!(payload.len() > 16 * 1024);
            }
            other => panic!("unexpected sealing result: {other:?}"),
        }
    }
    let final_payload = vec![0xee; usize::from(input.final_length) % 1024];
    up.extend(client.seal_chunk(&final_payload, ChunkPosition::Final).expect("final seals"));
    down.extend(
        gateway_half
            .seal_chunk(&final_payload, ChunkPosition::Final)
            .expect("final seals"),
    );
    sent.push(OpenedChunk { payload: final_payload, position: ChunkPosition::Final });

    let corrupted_up = corrupt(&mut up, &input.corruption);
    let corrupted_down = corrupt(&mut down, &input.corruption);

    let mut uplink = gateway_half.request_decoder();
    let mut uplink_feed = |piece: &[u8]| uplink.feed(piece);
    let fed = common::feed_split(&mut uplink_feed, &up, &input.splits);
    let decoded = match fed {
        Ok(mut opened) => match uplink.finish() {
            Ok(last) => {
                opened.extend(last);
                Decoded { opened, clean: true }
            }
            Err(_) => Decoded { opened, clean: false },
        },
        Err(_) => Decoded { opened: Vec::new(), clean: false },
    };
    check(&sent, &decoded, corrupted_up);

    let mut downlink = client.response_decoder();
    let decoded = {
        let mut feed = |piece: &[u8]| downlink.feed(piece);
        match common::feed_split(&mut feed, &down, &input.splits) {
            Ok(mut opened) => match downlink.finish() {
                Ok(last) => {
                    opened.extend(last);
                    Decoded { opened, clean: true }
                }
                Err(_) => Decoded { opened, clean: false },
            },
            Err(_) => Decoded { opened: Vec::new(), clean: false },
        }
    };
    check(&sent, &decoded, corrupted_down);
});
