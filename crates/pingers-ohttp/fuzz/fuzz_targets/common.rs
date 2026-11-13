// SPDX-License-Identifier: AGPL-3.0-only
//! Fixed keys and helpers shared by the fuzz targets.
#![allow(dead_code)]

use pingers_ohttp::{
    BinaryRequest, ChunkedClient, ChunkedClientStart, Gateway, GatewayKey, Header, KeyConfig,
    OpenedChunk, OhttpError,
};

/// The fixed gateway and its config, built once per process (key setup is the slow part).
pub fn gateway() -> &'static (Gateway, KeyConfig) {
    static GATEWAY: std::sync::OnceLock<(Gateway, KeyConfig)> = std::sync::OnceLock::new();
    GATEWAY.get_or_init(|| {
        let key = GatewayKey::from_bytes(1, zeroize::Zeroizing::new([0x42; 32]))
            .expect("fixed key is valid");
        let config = key.config();
        (Gateway::new(vec![key]), config)
    })
}

pub fn live_request() -> BinaryRequest {
    BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/groups/00/live".to_owned(),
        headers: vec![Header {
            name: "authorization".to_owned(),
            value: b"token".to_vec(),
        }],
        body: Vec::new(),
    }
}

pub fn start(config: &KeyConfig, seed: u64) -> ChunkedClientStart {
    ChunkedClient::start(config, &live_request(), &mut pingers_testkit::seeded_rng(seed))
        .expect("fixed request starts")
}

/// Feeds `bytes` in pieces whose sizes come from `splits` (cycled), collecting chunks.
pub fn feed_split(
    feed: &mut dyn FnMut(&[u8]) -> Result<Vec<OpenedChunk>, OhttpError>,
    bytes: &[u8],
    splits: &[u8],
) -> Result<Vec<OpenedChunk>, OhttpError> {
    let mut opened = Vec::new();
    let mut rest = bytes;
    let mut sizes = splits.iter().cycle();
    while !rest.is_empty() {
        let size = sizes.next().map_or(rest.len(), |size| usize::from(*size).max(1));
        let (piece, after) = rest.split_at(size.min(rest.len()));
        opened.extend(feed(piece)?);
        rest = after;
    }
    Ok(opened)
}
