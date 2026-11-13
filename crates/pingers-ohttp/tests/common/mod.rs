// SPDX-License-Identifier: AGPL-3.0-only
//! Helpers shared by the integration tests.

#![expect(dead_code, reason = "each test binary uses a different subset")]
#![expect(unreachable_pub, reason = "test helper module")]

use pingers_ohttp::{
    BinaryRequest, BinaryResponse, Gateway, GatewayKey, Header, KeyConfig, OhttpError, StatusCode,
};
use zeroize::Zeroizing;

/// Why a test could not run.
#[derive(Debug)]
pub enum TestError {
    Ohttp(OhttpError),
    Vector(pingers_testkit::VectorError),
    Hex(hex::FromHexError),
    Setup(&'static str),
}

impl From<OhttpError> for TestError {
    fn from(error: OhttpError) -> Self {
        Self::Ohttp(error)
    }
}

impl From<pingers_testkit::VectorError> for TestError {
    fn from(error: pingers_testkit::VectorError) -> Self {
        Self::Vector(error)
    }
}

impl From<hex::FromHexError> for TestError {
    fn from(error: hex::FromHexError) -> Self {
        Self::Hex(error)
    }
}

/// The gateway key every test uses unless it needs another.
pub const SECRET: [u8; 32] = [
    0x3c, 0x16, 0x89, 0x75, 0x67, 0x4b, 0x2f, 0xa8, 0xe4, 0x65, 0x97, 0x0b, 0x79, 0xc8, 0xdc, 0xf0,
    0x9f, 0x1c, 0x74, 0x16, 0x26, 0x48, 0x0b, 0xd4, 0xc6, 0x16, 0x2f, 0xc5, 0xb6, 0xa9, 0x8e, 0x1a,
];

pub fn gateway_key(key_id: u8, secret: [u8; 32]) -> Result<GatewayKey, OhttpError> {
    GatewayKey::from_bytes(key_id, Zeroizing::new(secret))
}

pub struct Setup {
    pub gateway: Gateway,
    pub config: KeyConfig,
}

/// A gateway with one key, id 1, and its config.
pub fn setup() -> Result<Setup, OhttpError> {
    let key = gateway_key(1, SECRET)?;
    let config = key.config();
    Ok(Setup {
        gateway: Gateway::new(vec![key]),
        config,
    })
}

pub fn request() -> BinaryRequest {
    BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/board/sync".to_owned(),
        headers: vec![Header {
            name: "authorization".to_owned(),
            value: b"PrivateToken token=abc".to_vec(),
        }],
        body: vec![0xa1, 0x01, 0x02],
    }
}

pub fn live_request() -> BinaryRequest {
    BinaryRequest {
        method: "POST".to_owned(),
        path: "/v1/groups/0011/live".to_owned(),
        headers: vec![Header {
            name: "authorization".to_owned(),
            value: b"group-token".to_vec(),
        }],
        body: Vec::new(),
    }
}

pub fn response() -> Result<BinaryResponse, OhttpError> {
    Ok(BinaryResponse {
        status: StatusCode::new(200)?,
        headers: vec![Header {
            name: "content-type".to_owned(),
            value: b"application/cbor".to_vec(),
        }],
        body: vec![0x80],
    })
}
