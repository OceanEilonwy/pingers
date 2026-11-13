// SPDX-License-Identifier: AGPL-3.0-only
//! Chunked OHTTP (draft-ietf-ohai-chunked-ohttp-08) through the public API.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod common;

use common::{Setup, TestError, live_request, setup};
use pingers_ohttp::{
    ChunkPosition, ChunkedClient, ChunkedClientStart, ChunkedGateway, OhttpError, OpenedChunk,
};
use pingers_testkit::seeded_rng;
use proptest::prelude::*;

fn non_final(payload: &[u8]) -> OpenedChunk {
    OpenedChunk {
        payload: payload.to_vec(),
        position: ChunkPosition::NonFinal,
    }
}

fn last(payload: &[u8]) -> OpenedChunk {
    OpenedChunk {
        payload: payload.to_vec(),
        position: ChunkPosition::Final,
    }
}

struct Exchange {
    client: ChunkedClient,
    gateway: ChunkedGateway,
}

fn exchange(setup: &Setup, seed: u64) -> Result<(Exchange, ChunkedClientStart), TestError> {
    let mut rng = seeded_rng(seed);
    let start = ChunkedClient::start(&setup.config, &live_request(), &mut rng)?;
    let ChunkedClientStart { preamble, client } = start;
    let gateway = ChunkedGateway::accept(&setup.gateway, &preamble, &mut rng)?;
    let again = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(seed))?;
    Ok((Exchange { client, gateway }, again))
}

#[test]
fn chunks_flow_both_ways_interleaved() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 1)?;
    assert_eq!(exchange.gateway.request(), &live_request());
    let mut uplink = exchange.gateway.request_decoder();
    let mut downlink = exchange.client.response_decoder();
    let preamble = exchange.gateway.response_preamble();
    assert_eq!(preamble.len(), 16);
    assert!(downlink.feed(&preamble)?.is_empty());
    for round in 0_u8..5 {
        let up = exchange
            .client
            .seal_chunk(&[round; 40], ChunkPosition::NonFinal)?;
        assert_eq!(uplink.feed(&up)?, [non_final(&[round; 40])]);
        let down = exchange
            .gateway
            .seal_chunk(&[round, round], ChunkPosition::NonFinal)?;
        assert_eq!(downlink.feed(&down)?, [non_final(&[round, round])]);
    }
    let up = exchange.client.seal_chunk(b"bye", ChunkPosition::Final)?;
    assert!(uplink.feed(&up)?.is_empty());
    assert_eq!(uplink.finish()?, Some(last(b"bye")));
    let down = exchange.gateway.seal_chunk(&[], ChunkPosition::Final)?;
    assert!(downlink.feed(&down)?.is_empty());
    assert_eq!(downlink.finish()?, Some(last(&[])));
    Ok(())
}

#[test]
fn start_takes_its_randomness_from_the_caller() -> Result<(), TestError> {
    let setup = setup()?;
    let first = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(2))?;
    let again = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(2))?;
    let other = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(3))?;
    assert_eq!(first.preamble, again.preamble);
    assert_ne!(first.preamble, other.preamble);
    Ok(())
}

#[test]
fn the_response_nonce_comes_from_the_gateways_rng() -> Result<(), TestError> {
    let setup = setup()?;
    let start = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(4))?;
    let first = ChunkedGateway::accept(&setup.gateway, &start.preamble, &mut seeded_rng(5))?;
    let again = ChunkedGateway::accept(&setup.gateway, &start.preamble, &mut seeded_rng(5))?;
    let other = ChunkedGateway::accept(&setup.gateway, &start.preamble, &mut seeded_rng(6))?;
    assert_eq!(first.response_preamble(), again.response_preamble());
    assert_ne!(first.response_preamble(), other.response_preamble());
    Ok(())
}

#[test]
fn preamble_is_header_then_one_framed_chunk() -> Result<(), TestError> {
    let setup = setup()?;
    let start = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(7))?;
    let sealed_length = live_request().encode()?.len() + 16;
    assert!((64..0x4000).contains(&sealed_length));
    // 7-byte header, 32-byte key, two-byte length, head sealed with a 16-byte tag.
    assert_eq!(start.preamble.len(), 7 + 32 + 2 + sealed_length);
    assert_eq!(
        start.preamble.get(..7),
        Some([1, 0x00, 0x20, 0x00, 0x01, 0x00, 0x01].as_slice())
    );
    let prefix = u16::try_from(sealed_length).map_err(|_| TestError::Setup("length"))? | 0x4000;
    assert_eq!(
        start.preamble.get(39..41),
        Some(prefix.to_be_bytes().as_slice())
    );
    Ok(())
}

#[test]
fn accept_asks_for_more_bytes_until_the_head_chunk_is_complete() -> Result<(), TestError> {
    let setup = setup()?;
    let start = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(8))?;
    for length in [0, 1, 7, 38, 39, 40, 41, start.preamble.len() - 1] {
        let partial = start.preamble.get(..length).unwrap_or_default();
        assert!(
            matches!(
                ChunkedGateway::accept(&setup.gateway, partial, &mut seeded_rng(9)),
                Err(OhttpError::Truncated)
            ),
            "length {length}"
        );
    }
    Ok(())
}

#[test]
fn chunks_that_arrive_with_the_preamble_come_out_of_the_decoder() -> Result<(), TestError> {
    let setup = setup()?;
    let ChunkedClientStart {
        preamble,
        mut client,
    } = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(10))?;
    let mut bytes = preamble;
    bytes.extend(client.seal_chunk(b"one", ChunkPosition::NonFinal)?);
    bytes.extend(client.seal_chunk(b"two", ChunkPosition::NonFinal)?);
    let partial = client.seal_chunk(b"three", ChunkPosition::Final)?;
    let (start_of_final, rest_of_final) = partial.split_at(3);
    bytes.extend_from_slice(start_of_final);
    let gateway = ChunkedGateway::accept(&setup.gateway, &bytes, &mut seeded_rng(11))?;
    let mut decoder = gateway.request_decoder();
    assert_eq!(decoder.feed(&[])?, [non_final(b"one"), non_final(b"two")]);
    assert!(decoder.feed(rest_of_final)?.is_empty());
    assert_eq!(decoder.finish()?, Some(last(b"three")));
    Ok(())
}

#[test]
fn finishing_before_feeding_out_preamble_chunks_is_an_error() -> Result<(), TestError> {
    let setup = setup()?;
    let ChunkedClientStart {
        preamble,
        mut client,
    } = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(12))?;
    let mut bytes = preamble;
    bytes.extend(client.seal_chunk(b"one", ChunkPosition::NonFinal)?);
    bytes.extend(client.seal_chunk(b"", ChunkPosition::Final)?);
    let gateway = ChunkedGateway::accept(&setup.gateway, &bytes, &mut seeded_rng(13))?;
    assert!(matches!(
        gateway.request_decoder().finish(),
        Err(OhttpError::Truncated)
    ));
    Ok(())
}

#[test]
fn only_the_first_request_decoder_works() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 14)?;
    let mut first = exchange.gateway.request_decoder();
    let mut second = exchange.gateway.request_decoder();
    let chunk = exchange.client.seal_chunk(b"x", ChunkPosition::NonFinal)?;
    assert!(matches!(second.feed(&chunk), Err(OhttpError::OpenFailed)));
    assert_eq!(first.feed(&chunk)?, [non_final(b"x")]);
    Ok(())
}

#[test]
fn a_request_whose_first_chunk_is_final_has_no_head() -> Result<(), TestError> {
    let setup = setup()?;
    let start = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(15))?;
    // Replace the head's two-byte length with the final indicator.
    let mut bytes = start.preamble.get(..39).unwrap_or_default().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(start.preamble.get(41..).unwrap_or_default());
    assert!(matches!(
        ChunkedGateway::accept(&setup.gateway, &bytes, &mut seeded_rng(16)),
        Err(OhttpError::BadMessage)
    ));
    Ok(())
}

#[test]
fn accept_checks_key_id_and_suite() -> Result<(), TestError> {
    let setup = setup()?;
    let start = ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(17))?;
    let mut other_key = start.preamble.clone();
    let mut chacha = start.preamble.clone();
    if let (Some(key_id), Some(aead)) = (other_key.first_mut(), chacha.get_mut(6)) {
        *key_id = 2;
        *aead = 3;
    }
    assert!(matches!(
        ChunkedGateway::accept(&setup.gateway, &other_key, &mut seeded_rng(18)),
        Err(OhttpError::UnknownKeyId { key_id: 2 })
    ));
    assert!(matches!(
        ChunkedGateway::accept(&setup.gateway, &chacha, &mut seeded_rng(18)),
        Err(OhttpError::UnsupportedSuite)
    ));
    Ok(())
}

#[test]
fn tampered_head_does_not_open() -> Result<(), TestError> {
    let setup = setup()?;
    let mut preamble =
        ChunkedClient::start(&setup.config, &live_request(), &mut seeded_rng(19))?.preamble;
    if let Some(byte) = preamble.last_mut() {
        *byte ^= 1;
    }
    assert!(matches!(
        ChunkedGateway::accept(&setup.gateway, &preamble, &mut seeded_rng(20)),
        Err(OhttpError::OpenFailed)
    ));
    Ok(())
}

#[test]
fn senders_refuse_empty_non_final_chunks() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 21)?;
    assert!(matches!(
        exchange.client.seal_chunk(&[], ChunkPosition::NonFinal),
        Err(OhttpError::EmptyNonFinalChunk)
    ));
    assert!(matches!(
        exchange.gateway.seal_chunk(&[], ChunkPosition::NonFinal),
        Err(OhttpError::EmptyNonFinalChunk)
    ));
    // The refusal does not consume a sequence number: the next chunk still opens.
    let chunk = exchange.client.seal_chunk(b"ok", ChunkPosition::NonFinal)?;
    assert_eq!(
        exchange.gateway.request_decoder().feed(&chunk)?,
        [non_final(b"ok")]
    );
    Ok(())
}

#[test]
fn nothing_can_be_sealed_after_the_final_chunk() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 22)?;
    exchange.client.seal_chunk(b"end", ChunkPosition::Final)?;
    exchange.gateway.seal_chunk(b"end", ChunkPosition::Final)?;
    for position in [ChunkPosition::NonFinal, ChunkPosition::Final] {
        assert!(matches!(
            exchange.client.seal_chunk(b"more", position),
            Err(OhttpError::DataAfterFinalChunk)
        ));
        assert!(matches!(
            exchange.gateway.seal_chunk(b"more", position),
            Err(OhttpError::DataAfterFinalChunk)
        ));
    }
    Ok(())
}

#[test]
fn chunks_above_sixteen_kibibytes_are_refused() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 23)?;
    let largest = vec![1; 16 * 1024];
    let mut decoder = exchange.gateway.request_decoder();
    let chunk = exchange
        .client
        .seal_chunk(&largest, ChunkPosition::NonFinal)?;
    assert_eq!(decoder.feed(&chunk)?, [non_final(&largest)]);
    let too_large = vec![1; 16 * 1024 + 1];
    assert!(matches!(
        exchange
            .client
            .seal_chunk(&too_large, ChunkPosition::NonFinal),
        Err(OhttpError::ChunkTooLarge)
    ));
    assert!(matches!(
        exchange
            .gateway
            .seal_chunk(&too_large, ChunkPosition::Final),
        Err(OhttpError::ChunkTooLarge)
    ));
    Ok(())
}

#[test]
fn a_head_of_exactly_one_full_chunk_is_accepted() -> Result<(), TestError> {
    let setup = setup()?;
    let mut request = live_request();
    // The body's length prefix shrinks with the body, so converge on the exact size.
    request.body = vec![0; 16 * 1024 - 100];
    for _ in 0..4 {
        let length = request.encode()?.len();
        request
            .body
            .resize(request.body.len() + 16 * 1024 - length, 0);
    }
    assert_eq!(request.encode()?.len(), 16 * 1024);
    let start = ChunkedClient::start(&setup.config, &request, &mut seeded_rng(33))?;
    let gateway = ChunkedGateway::accept(&setup.gateway, &start.preamble, &mut seeded_rng(34))?;
    assert_eq!(gateway.request(), &request);
    Ok(())
}

#[test]
fn opened_chunk_debug_shows_the_length_but_never_the_payload() {
    let chunk = non_final(b"lat=51.5,lon=-0.1");
    let shown = format!("{chunk:?}");
    assert!(shown.contains("17"));
    assert!(!shown.contains("lat"));
}

#[test]
fn a_head_longer_than_one_chunk_is_refused() -> Result<(), TestError> {
    let setup = setup()?;
    let mut request = live_request();
    request.body = vec![0; 16 * 1024];
    assert!(matches!(
        ChunkedClient::start(&setup.config, &request, &mut seeded_rng(24)),
        Err(OhttpError::BadMessage)
    ));
    Ok(())
}

#[test]
fn response_stream_without_final_chunk_is_truncated() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 25)?;
    let mut decoder = exchange.client.response_decoder();
    decoder.feed(&exchange.gateway.response_preamble())?;
    decoder.feed(&exchange.gateway.seal_chunk(b"a", ChunkPosition::NonFinal)?)?;
    assert!(matches!(decoder.finish(), Err(OhttpError::Truncated)));
    // Cut inside the nonce.
    let mut early = exchange.client.response_decoder();
    early.feed(
        exchange
            .gateway
            .response_preamble()
            .get(..15)
            .unwrap_or_default(),
    )?;
    assert!(matches!(early.finish(), Err(OhttpError::Truncated)));
    Ok(())
}

#[test]
fn request_stream_without_final_chunk_is_truncated() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 26)?;
    let mut decoder = exchange.gateway.request_decoder();
    decoder.feed(&exchange.client.seal_chunk(b"a", ChunkPosition::NonFinal)?)?;
    assert!(matches!(decoder.finish(), Err(OhttpError::Truncated)));
    Ok(())
}

#[test]
fn reordered_chunks_do_not_open() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 27)?;
    let first = exchange
        .client
        .seal_chunk(b"first", ChunkPosition::NonFinal)?;
    let second = exchange
        .client
        .seal_chunk(b"second", ChunkPosition::NonFinal)?;
    let mut decoder = exchange.gateway.request_decoder();
    assert!(matches!(decoder.feed(&second), Err(OhttpError::OpenFailed)));
    // The decoder stays failed, even for the chunk that would have been next.
    assert!(matches!(decoder.feed(&first), Err(OhttpError::OpenFailed)));
    assert!(matches!(decoder.finish(), Err(OhttpError::OpenFailed)));
    Ok(())
}

#[test]
fn a_non_final_chunk_cannot_pose_as_the_final_one() -> Result<(), TestError> {
    // Truncation attack: drop the real end and relabel a non-final chunk as final. The AAD
    // ("final") differs, so it does not open.
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 28)?;
    let chunk = exchange
        .client
        .seal_chunk(b"middle", ChunkPosition::NonFinal)?;
    let mut relabelled = vec![0];
    relabelled.extend_from_slice(chunk.get(1..).unwrap_or_default());
    let mut decoder = exchange.gateway.request_decoder();
    assert!(decoder.feed(&relabelled)?.is_empty());
    assert!(matches!(decoder.finish(), Err(OhttpError::OpenFailed)));
    Ok(())
}

#[test]
fn tampered_response_chunk_poisons_the_decoder() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut exchange, _) = exchange(&setup, 29)?;
    let mut decoder = exchange.client.response_decoder();
    decoder.feed(&exchange.gateway.response_preamble())?;
    let mut chunk = exchange
        .gateway
        .seal_chunk(b"data", ChunkPosition::NonFinal)?;
    if let Some(byte) = chunk.get_mut(2) {
        *byte ^= 4;
    }
    assert!(matches!(decoder.feed(&chunk), Err(OhttpError::OpenFailed)));
    let next = exchange
        .gateway
        .seal_chunk(b"next", ChunkPosition::NonFinal)?;
    assert!(matches!(decoder.feed(&next), Err(OhttpError::OpenFailed)));
    assert!(matches!(decoder.finish(), Err(OhttpError::OpenFailed)));
    Ok(())
}

#[test]
fn a_response_stream_opens_only_for_its_own_exchange() -> Result<(), TestError> {
    let setup = setup()?;
    let (mut ours, _) = exchange(&setup, 30)?;
    let (theirs, _) = exchange(&setup, 31)?;
    let mut decoder = theirs.client.response_decoder();
    decoder.feed(&ours.gateway.response_preamble())?;
    let chunk = ours
        .gateway
        .seal_chunk(b"secret", ChunkPosition::NonFinal)?;
    assert!(matches!(decoder.feed(&chunk), Err(OhttpError::OpenFailed)));
    Ok(())
}

#[test]
fn oversized_length_prefix_is_refused_before_buffering() -> Result<(), TestError> {
    let setup = setup()?;
    let (exchange, _) = exchange(&setup, 32)?;
    let mut decoder = exchange.gateway.request_decoder();
    assert!(matches!(
        decoder.feed(&[0x80, 0x00, 0x40, 0x11]),
        Err(OhttpError::ChunkTooLarge)
    ));
    Ok(())
}

fn feed_in_pieces(
    feed: &mut impl FnMut(&[u8]) -> Result<Vec<OpenedChunk>, OhttpError>,
    bytes: &[u8],
    splits: &[usize],
) -> Result<Vec<OpenedChunk>, OhttpError> {
    let mut opened = Vec::new();
    let mut rest = bytes;
    for split in splits {
        let (piece, after) = rest.split_at((*split).min(rest.len()));
        opened.extend(feed(piece)?);
        rest = after;
    }
    opened.extend(feed(rest)?);
    Ok(opened)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever the chunk sizes and however the transport splits the bytes, both directions
    /// return exactly the chunks that were sealed.
    #[test]
    fn any_split_returns_the_sealed_chunks(
        payloads in proptest::collection::vec(
            proptest::collection::vec(any::<u8>(), 1..300), 0..6),
        final_payload in proptest::collection::vec(any::<u8>(), 0..300),
        splits in proptest::collection::vec(0_usize..80, 0..30),
        seed in any::<u64>(),
    ) {
        let setup = setup()?;
        let mut rng = seeded_rng(seed);
        let ChunkedClientStart { preamble, mut client } =
            ChunkedClient::start(&setup.config, &live_request(), &mut rng)?;
        let mut gateway = ChunkedGateway::accept(&setup.gateway, &preamble, &mut rng)?;

        let mut expected: Vec<OpenedChunk> = payloads.iter().map(|p| non_final(p)).collect();
        let mut up = Vec::new();
        let mut down = gateway.response_preamble();
        for payload in &payloads {
            up.extend(client.seal_chunk(payload, ChunkPosition::NonFinal)?);
            down.extend(gateway.seal_chunk(payload, ChunkPosition::NonFinal)?);
        }
        up.extend(client.seal_chunk(&final_payload, ChunkPosition::Final)?);
        down.extend(gateway.seal_chunk(&final_payload, ChunkPosition::Final)?);

        let mut uplink = gateway.request_decoder();
        let opened = feed_in_pieces(&mut |piece| uplink.feed(piece), &up, &splits)?;
        prop_assert_eq!(&opened, &expected);
        prop_assert_eq!(uplink.finish()?, Some(last(&final_payload)));

        let mut downlink = client.response_decoder();
        let opened = feed_in_pieces(&mut |piece| downlink.feed(piece), &down, &splits)?;
        prop_assert_eq!(&opened, &expected);
        expected.clear();
        prop_assert_eq!(downlink.finish()?, Some(last(&final_payload)));
    }
}
