// SPDX-License-Identifier: AGPL-3.0-only
//! The `push_token_plaintext` fixed-layout decoder never panics and accepts only its own encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(padded) = <[u8; pingers_proto::PushTokenPlaintext::PADDED_SIZE]>::try_from(data) else {
        return;
    };
    if let Ok(plaintext) = pingers_proto::PushTokenPlaintext::decode_padded(&padded) {
        assert_eq!(plaintext.encode_padded().ok(), Some(padded));
    }
});
