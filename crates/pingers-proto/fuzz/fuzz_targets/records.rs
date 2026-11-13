// SPDX-License-Identifier: AGPL-3.0-only
//! The `records` fixed-layout decoder never panics and accepts only its own encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(records) = pingers_proto::Record::parse_concatenated(data) {
        let encoded: Vec<u8> = records.iter().flat_map(pingers_proto::Record::to_bytes).collect();
        assert_eq!(encoded, data);
    }
});
