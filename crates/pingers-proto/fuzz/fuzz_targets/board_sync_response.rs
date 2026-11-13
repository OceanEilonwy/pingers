// SPDX-License-Identifier: AGPL-3.0-only
//! `BoardSyncResponse::decode` never panics and accepts only its own canonical encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| pingers_proto_fuzz::decodes_only_canonical::<
    pingers_proto::BoardSyncResponse,
>(data));
