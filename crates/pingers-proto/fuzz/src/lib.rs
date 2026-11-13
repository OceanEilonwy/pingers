// SPDX-License-Identifier: AGPL-3.0-only
//! Shared checks for the pingers-proto fuzz targets.
//!
//! Run a target with `cargo +nightly fuzz run <target>` from `crates/pingers-proto`.

use pingers_proto::{WireDecode, WireEncode};

/// Decoding never panics, and whatever decodes re-encodes to exactly the input: there is one
/// encoding per value.
pub fn decodes_only_canonical<T: WireDecode + WireEncode>(data: &[u8]) {
    if let Ok(value) = T::decode(data) {
        let encoded = value.encode().ok();
        assert_eq!(encoded.as_deref(), Some(data));
    }
}
