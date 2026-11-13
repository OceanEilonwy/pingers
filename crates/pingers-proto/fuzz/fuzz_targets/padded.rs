// SPDX-License-Identifier: AGPL-3.0-only
//! The `padded` fixed-layout decoder never panics and accepts only its own encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(padded) = pingers_proto::Padded::from_wire(data.to_vec()) {
        if let Ok(payload) = padded.payload() {
            let again = pingers_proto::Padded::pad(payload, padded.class());
            assert_eq!(again.ok().as_ref(), Some(&padded));
        }
    }
});
