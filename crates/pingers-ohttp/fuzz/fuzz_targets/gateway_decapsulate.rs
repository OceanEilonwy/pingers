// SPDX-License-Identifier: AGPL-3.0-only
//! One-shot gateway decapsulation of arbitrary bytes, and client opening of arbitrary
//! responses: never panics. Inputs starting with a valid header reach HPKE.
#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;
use pingers_ohttp::encapsulate_request;

fuzz_target!(|data: &[u8]| {
    let (gateway, config) = common::gateway();
    // Steer half the inputs past the header check so HPKE and Binary HTTP see them.
    let mut message = data.to_vec();
    if data.first().is_some_and(|byte| byte & 1 == 1) {
        let mut prefixed = vec![1, 0x00, 0x20, 0x00, 0x01, 0x00, 0x01];
        prefixed.extend_from_slice(data.get(1..).unwrap_or_default());
        message = prefixed;
    }
    let _ = gateway.decapsulate(&message);

    let encapsulated =
        encapsulate_request(&config, &common::live_request(), &mut pingers_testkit::seeded_rng(1))
            .expect("fixed request encapsulates");
    let _ = encapsulated.response_context.open(data);
});
