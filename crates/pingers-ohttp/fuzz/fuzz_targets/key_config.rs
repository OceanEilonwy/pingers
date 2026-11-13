// SPDX-License-Identifier: AGPL-3.0-only
//! Key config decoding: never panics; an accepted config keeps its bytes exactly.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pingers_ohttp::KeyConfig;

fuzz_target!(|data: &[u8]| {
    if let Ok(config) = KeyConfig::decode(data) {
        assert_eq!(config.encoded(), data);
        assert_eq!(Some(&config.key_id()), data.first());
    }
});
