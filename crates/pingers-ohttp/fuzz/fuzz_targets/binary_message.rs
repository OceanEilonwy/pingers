// SPDX-License-Identifier: AGPL-3.0-only
//! Binary HTTP decoding: never panics; whatever decodes re-encodes to something that decodes
//! to the same message.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pingers_ohttp::{BinaryRequest, BinaryResponse};

fuzz_target!(|data: &[u8]| {
    if let Ok(request) = BinaryRequest::decode(data) {
        let encoded = request.encode().expect("a decoded request encodes");
        assert_eq!(BinaryRequest::decode(&encoded).expect("round trip"), request);
    }
    if let Ok(response) = BinaryResponse::decode(data) {
        let encoded = response.encode().expect("a decoded response encodes");
        assert_eq!(BinaryResponse::decode(&encoded).expect("round trip"), response);
    }
});
