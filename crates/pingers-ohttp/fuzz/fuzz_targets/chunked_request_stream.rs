// SPDX-License-Identifier: AGPL-3.0-only
//! The gateway's chunked path on arbitrary bytes: `accept` on a preamble, then the request
//! decoder on the rest in arbitrary splits. Never panics. Half the inputs are prefixed with a
//! real preamble so the decoder sees framing after a valid head.
#![no_main]

mod common;

use libfuzzer_sys::{arbitrary, fuzz_target};

#[derive(arbitrary::Arbitrary, Debug)]
struct Input {
    real_preamble: bool,
    /// How many stream bytes go to `accept` with the preamble.
    with_preamble: u16,
    splits: Vec<u8>,
    stream: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let (gateway, config) = common::gateway();
    let mut rng = pingers_testkit::seeded_rng(2);
    let mut bytes = Vec::new();
    if input.real_preamble {
        bytes.extend(common::start(&config, 3).preamble);
    }
    let cut = bytes
        .len()
        .saturating_add(usize::from(input.with_preamble))
        .min(bytes.len().saturating_add(input.stream.len()));
    bytes.extend_from_slice(&input.stream);
    let (preamble, rest) = bytes.split_at(cut);
    let Ok(accepted) = pingers_ohttp::ChunkedGateway::accept(&gateway, preamble, &mut rng) else {
        return;
    };
    let mut decoder = accepted.request_decoder();
    if decoder.feed(&[]).is_ok()
        && common::feed_split(&mut |piece| decoder.feed(piece), rest, &input.splits).is_ok()
    {
        let _ = decoder.finish();
    }
});
