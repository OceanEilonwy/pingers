// SPDX-License-Identifier: AGPL-3.0-only
//! The client's response decoder on arbitrary bytes in arbitrary splits: never panics.
//! Half the inputs start with the real response nonce so the framing after it is exercised
//! with the right keys.
#![no_main]

mod common;

use libfuzzer_sys::{arbitrary, fuzz_target};

#[derive(arbitrary::Arbitrary, Debug)]
struct Input {
    real_nonce: bool,
    splits: Vec<u8>,
    stream: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let (gateway, config) = common::gateway();
    let start = common::start(&config, 4);
    let mut bytes = Vec::new();
    if input.real_nonce {
        let accepted = pingers_ohttp::ChunkedGateway::accept(
            &gateway,
            &start.preamble,
            &mut pingers_testkit::seeded_rng(5),
        )
        .expect("real preamble is accepted");
        bytes.extend(accepted.response_preamble());
    }
    bytes.extend_from_slice(&input.stream);
    let mut decoder = start.client.response_decoder();
    if common::feed_split(&mut |piece| decoder.feed(piece), &bytes, &input.splits).is_ok() {
        let _ = decoder.finish();
    }
});
