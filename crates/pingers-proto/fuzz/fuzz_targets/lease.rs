// SPDX-License-Identifier: AGPL-3.0-only
//! The `lease` fixed-layout decoder never panics and accepts only its own encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(lease) = pingers_proto::Lease::from_bytes(data) {
        assert_eq!(lease.to_bytes().as_slice(), data);
    }
});
