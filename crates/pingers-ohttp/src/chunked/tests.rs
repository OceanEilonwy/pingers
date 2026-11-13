// SPDX-License-Identifier: AGPL-3.0-only
//! Tests that need the crate's internals: the draft-08 worked example, which does not use
//! Pingers' head-chunk convention, and empty non-final chunks, which the public API refuses
//! to send.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_testkit::{FixedRng, seeded_rng};
use zeroize::Zeroizing;

use super::{ChunkedClient, ChunkedGateway};
use crate::framing::{ChunkReader, frame};
use crate::{BinaryRequest, ChunkPosition, Gateway, GatewayKey, Header, OhttpError, OpenedChunk};

fn unhex(text: &str) -> Vec<u8> {
    hex::decode(text).unwrap_or_default()
}

/// draft-ietf-ohai-chunked-ohttp-08 Appendix A, from the gateway's side (the client side
/// needs the ephemeral secret key, which HPKE here derives from randomness instead).
#[test]
fn gateway_reproduces_chunked_draft_appendix_a() -> Result<(), OhttpError> {
    let secret = <[u8; 32]>::try_from(unhex(
        "1c190d72acdbe4dbc69e680503bb781a932c70a12c8f3754434c67d8640d8698",
    ))
    .map_err(|_| OhttpError::Internal)?;
    let key = GatewayKey::from_bytes(1, Zeroizing::new(secret))?;
    // The served config (single suite) carries the draft's public key.
    assert_eq!(
        key.config().encoded().get(3..35),
        unhex("668eb21aace159803974a4c67f08b4152d29bed10735fd08f98ccdd6fe095708").get(..)
    );

    let request = unhex(concat!(
        "01002000010001",
        "8811eb457e100811c40a0aa71340a1b81d804bb986f736f2f566a7199761a032",
        "1c2ad24942d4d692563012f2980c8fef437a336b9b2fc938ef77a5834f",
        "1d2e33d8fd25577afe31bd1c79d094f76b6250ae6549b473ecd950501311",
        "001c6c1395d0ef7c1022297966307b8a7f",
    ));
    let (header, chunks) = request.split_at(39);
    let mut opener = key
        .server()
        .decapsulate_chunked(header)
        .map_err(|_| OhttpError::OpenFailed)?;
    let mut reader = ChunkReader::new();
    let mut plaintext = Vec::new();
    for sealed in reader.feed(chunks)? {
        plaintext.extend(
            opener
                .open(&sealed, false)
                .map_err(|_| OhttpError::OpenFailed)?,
        );
    }
    let last = opener
        .open(&reader.finish()?, true)
        .map_err(|_| OhttpError::OpenFailed)?;
    assert!(last.is_empty());
    assert_eq!(
        plaintext,
        unhex("00034745540568747470730b6578616d706c652e636f6d012f")
    );

    let mut response = opener
        .response(&mut FixedRng::new(&unhex(
            "bcce7f4cb921309ba5d62edf1769ef09",
        )))
        .map_err(|_| OhttpError::Internal)?;
    let mut encapsulated = response.nonce().to_vec();
    for (chunk, is_last) in [(&[0x01][..], false), (&[0x40, 0xc8], false), (&[], true)] {
        let sealed = response
            .seal(chunk, is_last)
            .map_err(|_| OhttpError::Internal)?;
        encapsulated.extend(frame(&sealed, is_last)?);
    }
    assert_eq!(
        hex::encode(encapsulated),
        concat!(
            "bcce7f4cb921309ba5d62edf1769ef09",
            "1179bf1cc87fa0e2c02de4546945aa3d1e48",
            "12b348b5bd4c594c16b6170b07b475845d1f32",
            "00ed9d8a796617a5b27265f4d73247f639",
        )
    );
    Ok(())
}

struct Pair {
    client: ChunkedClient,
    gateway: ChunkedGateway,
}

fn pair(seed: u64) -> Result<Pair, OhttpError> {
    let mut rng = seeded_rng(seed);
    let key = GatewayKey::generate(1, &mut rng)?;
    let config = key.config();
    let gateway = Gateway::new(vec![key]);
    let request = BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/groups/00/live".to_owned(),
        headers: vec![Header {
            name: "authorization".to_owned(),
            value: b"token".to_vec(),
        }],
        body: Vec::new(),
    };
    let start = ChunkedClient::start(&config, &request, &mut rng)?;
    Ok(Pair {
        client: start.client,
        gateway: ChunkedGateway::accept(&gateway, &start.preamble, &mut rng)?,
    })
}

/// ohttp issue #102: upstream accepted this; draft-08 §6 makes it a decryption error.
#[test]
fn gateway_rejects_an_authentic_empty_non_final_request_chunk() -> Result<(), OhttpError> {
    let mut pair = pair(1)?;
    let empty = pair
        .client
        .inner
        .seal_empty_non_final()
        .map_err(|_| OhttpError::Internal)?;
    let mut decoder = pair.gateway.request_decoder();
    assert!(matches!(
        decoder.feed(&frame(&empty, false)?),
        Err(OhttpError::EmptyNonFinalChunk)
    ));
    // And the stream cannot carry on past it.
    let next = pair.client.seal_chunk(b"next", ChunkPosition::NonFinal)?;
    assert!(matches!(decoder.feed(&next), Err(OhttpError::OpenFailed)));
    Ok(())
}

#[test]
fn client_rejects_an_authentic_empty_non_final_response_chunk() -> Result<(), OhttpError> {
    let mut pair = pair(2)?;
    let mut decoder = pair.client.response_decoder();
    decoder.feed(&pair.gateway.response_preamble())?;
    let first = pair.gateway.seal_chunk(b"first", ChunkPosition::NonFinal)?;
    assert_eq!(
        decoder.feed(&first)?,
        [OpenedChunk {
            payload: b"first".to_vec(),
            position: ChunkPosition::NonFinal,
        }]
    );
    let empty = pair
        .gateway
        .response
        .seal_empty_non_final()
        .map_err(|_| OhttpError::Internal)?;
    assert!(matches!(
        decoder.feed(&frame(&empty, false)?),
        Err(OhttpError::EmptyNonFinalChunk)
    ));
    Ok(())
}

#[test]
fn an_empty_final_chunk_is_accepted() -> Result<(), OhttpError> {
    let mut pair = pair(3)?;
    let mut decoder = pair.gateway.request_decoder();
    decoder.feed(&pair.client.seal_chunk(&[], ChunkPosition::Final)?)?;
    assert_eq!(
        decoder.finish()?,
        Some(OpenedChunk {
            payload: Vec::new(),
            position: ChunkPosition::Final,
        })
    );
    Ok(())
}
