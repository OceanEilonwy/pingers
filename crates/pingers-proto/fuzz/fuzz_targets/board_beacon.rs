// SPDX-License-Identifier: AGPL-3.0-only
//! The `board_beacon` fixed-layout decoder never panics and accepts only its own encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: [u8; pingers_proto::BoardBeacon::SIZE]| {
    if let Ok(beacon) = pingers_proto::BoardBeacon::from_bytes(&data) {
        assert_eq!(beacon.to_bytes(), data);
    }
});
