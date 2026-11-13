// SPDX-License-Identifier: AGPL-3.0-only
//! Known-answer tests from `vectors/ohttp/` (style guide §7), shared with other
//! implementations. Randomness is given as bytes and replayed, so results are exact.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod common;

use common::{TestError, gateway_key};
use pingers_ohttp::{
    BinaryRequest, BinaryResponse, ChunkPosition, ChunkedClient, ChunkedGateway, Gateway, Header,
    KeyConfig, OhttpError, OpenedChunk, StatusCode, encapsulate_request,
};
use pingers_testkit::{FixedRng, Vector, load_vectors};

fn text(vector: &Vector, field: &str) -> Result<String, TestError> {
    vector
        .inputs
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or(TestError::Setup("missing text field"))
}

/// A header if the vector has one, under `<prefix>header_name` and `<prefix>header_value`.
fn headers(vector: &Vector, prefix: &str) -> Result<Vec<Header>, TestError> {
    let name_field = format!("{prefix}header_name");
    if !vector.inputs.contains_key(&name_field) {
        return Ok(Vec::new());
    }
    Ok(vec![Header {
        name: text(vector, &name_field)?,
        value: vector.input_bytes(&format!("{prefix}header_value"))?,
    }])
}

fn request(vector: &Vector) -> Result<BinaryRequest, TestError> {
    Ok(BinaryRequest {
        method: text(vector, "method")?,
        path: text(vector, "path")?,
        headers: headers(vector, "")?,
        body: if vector.inputs.contains_key("request_body") {
            vector.input_bytes("request_body")?
        } else {
            Vec::new()
        },
    })
}

fn gateway(vector: &Vector) -> Result<Gateway, TestError> {
    let secret = <[u8; 32]>::try_from(vector.input_bytes("gateway_secret_key")?)
        .map_err(|_| TestError::Setup("secret length"))?;
    let key_id =
        u8::try_from(vector.input_u64("key_id")?).map_err(|_| TestError::Setup("key id"))?;
    Ok(Gateway::new(vec![gateway_key(key_id, secret)?]))
}

fn config(vector: &Vector) -> Result<KeyConfig, TestError> {
    let secret = <[u8; 32]>::try_from(vector.input_bytes("gateway_secret_key")?)
        .map_err(|_| TestError::Setup("secret length"))?;
    let key_id =
        u8::try_from(vector.input_u64("key_id")?).map_err(|_| TestError::Setup("key id"))?;
    Ok(gateway_key(key_id, secret)?.config())
}

fn expected_error(vector: &Vector) -> Option<&str> {
    vector
        .expected
        .get("error")
        .and_then(serde_json::Value::as_str)
}

/// The variant name, as vectors spell it.
fn kind(error: &OhttpError) -> String {
    let debug = format!("{error:?}");
    debug
        .split([' ', '{', '('])
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn encapsulation_vectors() -> Result<(), TestError> {
    let file = load_vectors("ohttp/encapsulation")?;
    assert!(file.vectors.len() >= 5);
    for vector in &file.vectors {
        let gateway = gateway(vector)?;
        if let Some(error) = expected_error(vector) {
            let message = vector.input_bytes("encapsulated_request")?;
            let result = gateway.decapsulate(&message).map(|_| ());
            assert_eq!(
                result.as_ref().map_err(kind).err().as_deref(),
                Some(error),
                "{}",
                vector.name
            );
            continue;
        }
        let config = config(vector)?;
        assert_eq!(config.encoded(), vector.expected_bytes("key_config")?);
        assert_eq!(
            config.hash().as_bytes().as_slice(),
            vector.expected_bytes("key_config_hash")?
        );
        let request = request(vector)?;
        assert_eq!(request.encode()?, vector.expected_bytes("binary_request")?);

        let mut rng = FixedRng::new(&vector.input_bytes("encapsulation_randomness")?);
        let encapsulated = encapsulate_request(&config, &request, &mut rng)?;
        assert!(rng.exhausted_exactly(), "{}", vector.name);
        assert_eq!(
            encapsulated.message,
            vector.expected_bytes("encapsulated_request")?,
            "{}",
            vector.name
        );

        let response = BinaryResponse {
            status: StatusCode::new(
                u16::try_from(vector.input_u64("response_status")?)
                    .map_err(|_| TestError::Setup("status"))?,
            )?,
            headers: headers(vector, "response_")?,
            body: vector.input_bytes("response_body")?,
        };
        assert_eq!(
            response.encode()?,
            vector.expected_bytes("binary_response")?
        );
        let decapsulated = gateway.decapsulate(&encapsulated.message)?;
        assert_eq!(decapsulated.request, request);
        let mut rng = FixedRng::new(&vector.input_bytes("response_nonce")?);
        let sealed = decapsulated.responder.seal(&response, &mut rng)?;
        assert!(rng.exhausted_exactly());
        assert_eq!(sealed, vector.expected_bytes("encapsulated_response")?);
        assert_eq!(encapsulated.response_context.open(&sealed)?, response);
    }
    Ok(())
}

struct Opened {
    chunks: Vec<OpenedChunk>,
}

/// Feeds a whole stream one byte at a time (the hardest split), then finishes it.
fn decode_stream(
    feed: &mut impl FnMut(&[u8]) -> Result<Vec<OpenedChunk>, OhttpError>,
    stream: &[u8],
) -> Result<Opened, OhttpError> {
    let mut chunks = Vec::new();
    for byte in stream {
        chunks.extend(feed(&[*byte])?);
    }
    Ok(Opened { chunks })
}

fn non_final(payload: Vec<u8>) -> OpenedChunk {
    OpenedChunk {
        payload,
        position: ChunkPosition::NonFinal,
    }
}

#[test]
fn chunked_vectors() -> Result<(), TestError> {
    let file = load_vectors("ohttp/chunked")?;
    assert!(file.vectors.len() >= 6);
    for vector in &file.vectors {
        let gateway = gateway(vector)?;
        let config = config(vector)?;
        let request = request(vector)?;
        let mut client_rng = FixedRng::new(&vector.input_bytes("encapsulation_randomness")?);
        let start = ChunkedClient::start(&config, &request, &mut client_rng)?;
        assert!(client_rng.exhausted_exactly());
        let mut gateway_rng = FixedRng::new(&vector.input_bytes("response_nonce")?);
        let accepted = ChunkedGateway::accept(&gateway, &start.preamble, &mut gateway_rng)?;
        assert!(gateway_rng.exhausted_exactly());

        if let Some(error) = expected_error(vector) {
            let result = if vector.inputs.contains_key("request_stream") {
                let stream = vector.input_bytes("request_stream")?;
                // The stream starts with the same preamble; the rest goes to the decoder.
                let rest = stream
                    .strip_prefix(start.preamble.as_slice())
                    .ok_or(TestError::Setup("request stream preamble"))?;
                let mut decoder = accepted.request_decoder();
                decode_stream(&mut |bytes| decoder.feed(bytes), rest)
                    .and_then(|_| decoder.finish().map(|_| ()))
            } else {
                let stream = vector.input_bytes("response_stream")?;
                let mut decoder = start.client.response_decoder();
                decode_stream(&mut |bytes| decoder.feed(bytes), &stream)
                    .and_then(|_| decoder.finish().map(|_| ()))
            };
            assert_eq!(
                result.as_ref().map_err(kind).err().as_deref(),
                Some(error),
                "{}",
                vector.name
            );
            continue;
        }

        assert_eq!(
            request.encode()?,
            vector.expected_bytes("binary_request_head")?
        );
        assert_eq!(start.preamble, vector.expected_bytes("preamble")?);
        assert_eq!(accepted.request(), &request);
        let mut client = start.client;
        let mut gateway_half = accepted;

        let mut uplink = Vec::new();
        let mut sent = Vec::new();
        for index in 0..2 {
            let payload = vector.input_bytes(&format!("request_chunk_{index}"))?;
            let framed = client.seal_chunk(&payload, ChunkPosition::NonFinal)?;
            assert_eq!(
                framed,
                vector.expected_bytes(&format!("request_chunk_{index}_framed"))?
            );
            uplink.extend(framed);
            sent.push(non_final(payload));
        }
        let final_payload = vector.input_bytes("request_final_chunk")?;
        let framed = client.seal_chunk(&final_payload, ChunkPosition::Final)?;
        assert_eq!(framed, vector.expected_bytes("request_final_chunk_framed")?);
        uplink.extend(framed);
        let mut decoder = gateway_half.request_decoder();
        let opened = decode_stream(&mut |bytes| decoder.feed(bytes), &uplink)?;
        assert_eq!(opened.chunks, sent);
        assert_eq!(
            decoder.finish()?.map(|chunk| chunk.payload),
            Some(final_payload)
        );

        let mut downlink = gateway_half.response_preamble();
        assert_eq!(downlink, vector.expected_bytes("response_preamble")?);
        let payload = vector.input_bytes("response_chunk_0")?;
        let framed = gateway_half.seal_chunk(&payload, ChunkPosition::NonFinal)?;
        assert_eq!(framed, vector.expected_bytes("response_chunk_0_framed")?);
        downlink.extend(framed);
        let final_payload = vector.input_bytes("response_final_chunk")?;
        let framed = gateway_half.seal_chunk(&final_payload, ChunkPosition::Final)?;
        assert_eq!(
            framed,
            vector.expected_bytes("response_final_chunk_framed")?
        );
        downlink.extend(framed);
        let mut decoder = client.response_decoder();
        let opened = decode_stream(&mut |bytes| decoder.feed(bytes), &downlink)?;
        assert_eq!(opened.chunks, [non_final(payload)]);
        assert_eq!(
            decoder.finish()?.map(|chunk| chunk.payload),
            Some(final_payload)
        );
    }
    Ok(())
}
